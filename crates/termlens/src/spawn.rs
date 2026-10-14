//! Starting the child inside the PTY.
//!
//! On Unix termlens forks and executes the child itself instead of handing
//! the command to the PTY layer's `spawn_command`. Between fork and exec,
//! portable-pty closes every descriptor above stdio, and one of them is the
//! pipe through which std reports a failed `exec`. A program that exists but
//! cannot be executed, such as a script whose `#!` interpreter is missing,
//! then died of a runtime abort, and `spawn` returned `Ok` with a child
//! already killed by SIGABRT (#563; upstream, wezterm/wezterm#7893).
//!
//! So the command is built the way portable-pty 0.9.0 builds it, with the
//! same program resolution, working directory, environment, session and
//! signals, and one difference: descriptors above stdio are marked
//! close-on-exec instead of closed. A successful exec closes the same set the
//! PTY layer closed, and a failed one can still report why. The test at the
//! foot of this file starts one probe both ways and requires the same
//! answers, so a portable-pty release that changes its own spawn fails there
//! rather than drifting apart from this one unseen.
//!
//! Whatever this path is not sure of goes to the PTY layer unchanged: a
//! process whose children the kernel reaps (std would panic reporting the
//! failure), a working directory that is not one with no `HOME` in the
//! child's environment to fall back to (the PTY layer reads the passwd
//! entry), a program that does not resolve (the PTY layer says why, in the
//! words termlens has always reported), a slave device it cannot name or
//! open. Windows always goes there, because ConPTY reports a failed
//! `CreateProcess` directly.

use std::ffi::OsString;

use portable_pty::{Child, CommandBuilder, PtyPair};

/// A child running in the PTY, as the PTY layer hands one back.
pub(crate) type PtyChild = Box<dyn Child + Send + Sync>;

/// Start `cmd` on the slave side of `pair`, with `env` set on top of the
/// inherited environment, or on top of nothing when `env_clear` is set.
/// Later entries in `env` win.
pub(crate) fn spawn_child(
    pair: &PtyPair,
    mut cmd: CommandBuilder,
    env_clear: bool,
    env: &[(OsString, OsString)],
) -> Result<PtyChild, String> {
    if env_clear {
        cmd.env_clear();
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    #[cfg(unix)]
    if let Some(spawned) = unix::spawn(pair.master.as_ref(), &cmd, env_clear, env) {
        return spawned;
    }
    pair.slave
        .spawn_command(cmd)
        .map_err(|error| error.to_string())
}

#[cfg(unix)]
mod unix {
    use std::ffi::{CString, OsStr, OsString};
    use std::io::{self, Read};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;
    use std::path::{Component, Path, PathBuf};
    use std::process::Command;

    use portable_pty::{CommandBuilder, MasterPty};

    use super::PtyChild;

    /// Spawn `cmd` the way the PTY layer would, or `None` to leave it to the
    /// PTY layer.
    pub(super) fn spawn(
        master: &dyn MasterPty,
        cmd: &CommandBuilder,
        env_clear: bool,
        env: &[(OsString, OsString)],
    ) -> Option<Result<PtyChild, String>> {
        // std reports a failed exec only after reaping the child, and panics
        // when the reap fails, which it does wherever the kernel reaps
        // children itself. The PTY layer never reports the failure, so it
        // never gets that far.
        if children_reaped_by_the_kernel() {
            return None;
        }
        let argv = cmd.get_argv();
        let program = argv.first()?;
        // Where the PTY layer runs the child: the working directory, or the
        // child's HOME when that is not a directory. Without a HOME it reads
        // the passwd entry, which is left to it.
        let dir = match cmd.get_cwd().map(Path::new).filter(|dir| dir.is_dir()) {
            Some(dir) => dir.to_owned(),
            None => PathBuf::from(cmd.get_env("HOME")?.to_str()?),
        };
        let resolved = resolve(program, &dir, cmd.get_env("PATH"))?;
        // A second handle on the slave, by name, because the PTY layer does
        // not expose its own. O_NOCTTY keeps the device from becoming this
        // process's controlling terminal; std adds O_CLOEXEC.
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(master.tty_name()?)
            .ok()?;
        let stdin = slave.try_clone().ok()?;
        let stdout = slave.try_clone().ok()?;

        let mut command = Command::new(&resolved);
        command
            .arg0(program)
            .args(&argv[1..])
            .current_dir(&dir)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(slave);
        if env_clear {
            command.env_clear();
        }
        // When the test process has no SHELL, the PTY layer fills one in
        // from the login shell, and the builder holds the value it chose.
        if let Some(shell) = cmd.get_env("SHELL") {
            command.env("SHELL", shell);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let controlling_tty = cmd.get_controlling_tty();
        // SAFETY: the closure runs in the forked child before exec, where a
        // lock held by another thread of this process stays held forever. It
        // takes the PTY layer's own pre_exec step by step, but for two: the
        // umask step, which acts only on a umask termlens never sets, is left
        // out, and descriptors are marked close-on-exec rather than closed.
        // signal, sigemptyset, sigprocmask, setsid, ioctl and fcntl are
        // async-signal-safe and act on the child alone. Listing /dev/fd
        // allocates, as the PTY layer's listing always did at this point.
        #[allow(unsafe_code)]
        unsafe {
            command.pre_exec(move || {
                // Defaults for the signals a test runner tends to change, and
                // an empty mask, so the child does not inherit an ignored
                // SIGINT from whatever started the test.
                for signal in [
                    libc::SIGCHLD,
                    libc::SIGHUP,
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTERM,
                    libc::SIGALRM,
                ] {
                    libc::signal(signal, libc::SIG_DFL);
                }
                let mut none = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
                libc::sigemptyset(none.as_mut_ptr());
                libc::sigprocmask(libc::SIG_SETMASK, none.as_ptr(), std::ptr::null_mut());
                // A session of its own with the PTY as its controlling
                // terminal, which is what turns ^C into SIGINT and a closed
                // master into SIGHUP.
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if controlling_tty && libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                // The descriptors the PTY layer closes here, marked
                // close-on-exec instead. A successful exec closes the same
                // set; a failed one leaves std the pipe it reports through.
                if let Ok(entries) = std::fs::read_dir("/dev/fd") {
                    let fds: Vec<libc::c_int> = entries
                        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
                        .filter(|&fd| fd > 2)
                        .collect();
                    for fd in fds {
                        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                    }
                }
                Ok(())
            });
        }
        Some(match command.spawn() {
            Ok(child) => Ok(Box::new(child)),
            Err(error) => Err(explain(&error, &resolved, &dir)),
        })
    }

    /// Whether this process's children are reaped by the kernel the moment
    /// they exit: SIGCHLD ignored, or `SA_NOCLDWAIT` set on it, either
    /// inherited from whatever started the test.
    fn children_reaped_by_the_kernel() -> bool {
        // SAFETY: with a null new action, sigaction only writes the current
        // one into `current`, and a zeroed sigaction is a valid value for it
        // to overwrite.
        #[allow(unsafe_code)]
        let current = unsafe {
            let mut current: libc::sigaction = std::mem::zeroed();
            (libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut current) == 0).then_some(current)
        };
        current.is_some_and(|action| {
            action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0
        })
    }

    /// The file the PTY layer would execute for `program` run in `dir`,
    /// chosen by its rules, or `None` for the PTY layer to say why there is
    /// none.
    fn resolve(program: &OsStr, dir: &Path, path: Option<&OsStr>) -> Option<PathBuf> {
        let program = Path::new(program);
        let runnable = |file: &Path| !file.is_dir() && executable(file);
        if program.is_absolute() {
            return runnable(program).then(|| program.to_owned());
        }
        // `./x` and `../x` are looked up in `dir` alone.
        if matches!(
            program.components().next(),
            Some(Component::CurDir | Component::ParentDir)
        ) {
            let file = dir.join(program);
            return runnable(&file).then_some(file);
        }
        // Anything else, `sub/x` included, along PATH, each entry taken
        // relative to `dir`.
        std::env::split_paths(path?)
            .map(|entry| dir.join(entry).join(program))
            .find(|file| runnable(file))
    }

    /// access(2) with `X_OK`, as the PTY layer's resolution asks it: the
    /// kernel's verdict for this process, ACLs and noexec mounts included,
    /// where the mode bits would be a guess. For a directory, whether it can
    /// be entered.
    fn executable(file: &Path) -> bool {
        let Ok(file) = CString::new(file.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: `file` is a NUL-terminated string that outlives the call,
        // and access(2) only reads it.
        #[allow(unsafe_code)]
        let status = unsafe { libc::access(file.as_ptr(), libc::X_OK) };
        status == 0
    }

    /// Why the child could not be started. std hands back the errno of
    /// whichever step failed (the fork, a NUL in an argument, entering the
    /// working directory, the session setup, the exec), and the errno alone
    /// does not say which. So the program is blamed only for what the exec
    /// alone could raise, and where the kernel's `ENOENT` or `EACCES` points
    /// at a file that plainly is there, the file it went looking for is
    /// named: the `#!` interpreter of a script, or the loader of a binary.
    /// Anything else reads as std gave it, as the PTY layer's spawn did.
    fn explain(error: &io::Error, program: &Path, dir: &Path) -> String {
        let shown = program.display();
        match error.raw_os_error() {
            // Entering the directory fails with these too, and fails first.
            Some(
                libc::ENOENT | libc::EACCES | libc::ENOTDIR | libc::ELOOP | libc::ENAMETOOLONG,
            ) if !dir.is_dir() || !executable(dir) => {
                format!("cannot enter {}: {error}", dir.display())
            }
            Some(libc::ENOENT) if program.is_file() => match interpreter(program) {
                Ok(Some(interpreter)) if !present(dir, &interpreter) => format!(
                    "cannot execute {shown}: its #! interpreter {interpreter:?} does not exist"
                ),
                Ok(Some(interpreter)) => format!(
                    "cannot execute {shown}: {error}; its #! interpreter {interpreter:?} \
                     exists, so what is missing is something the interpreter needs"
                ),
                Ok(None) => format!(
                    "cannot execute {shown}: {error}; the file exists, so what is missing \
                     is its dynamic loader"
                ),
                Err(_) => format!(
                    "cannot execute {shown}: {error}; the file exists, so what is missing \
                     is its #! interpreter or its dynamic loader"
                ),
            },
            Some(libc::EACCES) => match interpreter(program) {
                Ok(Some(interpreter)) if !runnable(dir, &interpreter) => format!(
                    "cannot execute {shown}: its #! interpreter {interpreter:?} is not an \
                     executable file"
                ),
                _ => format!("cannot execute {shown}: {error}"),
            },
            Some(
                libc::ENOENT
                | libc::E2BIG
                | libc::ETXTBSY
                | libc::ENOEXEC
                | libc::EISDIR
                | libc::ENOTDIR
                | libc::ELOOP
                | libc::ENAMETOOLONG,
            ) => format!("cannot execute {shown}: {error}"),
            _ => error.to_string(),
        }
    }

    /// Whether an interpreter path names something, looked up as the kernel
    /// looks it up: relative to the directory the child runs in.
    fn present(dir: &Path, interpreter: &Path) -> bool {
        !interpreter.as_os_str().is_empty() && dir.join(interpreter).exists()
    }

    /// Whether an interpreter path names a file the kernel would execute.
    fn runnable(dir: &Path, interpreter: &Path) -> bool {
        let file = dir.join(interpreter);
        present(dir, interpreter) && file.is_file() && executable(&file)
    }

    /// What a script's `#!` line names, read the way the kernel reads it:
    /// the first word after `#!` on the first line, ended by a space, a tab
    /// or a NUL, and on macOS by a `#` as well. A `\r` stays part of the
    /// name, which is why a script saved with CRLF line endings cannot run.
    /// `Ok(None)` for a file that is not a script, and `Err` for one that
    /// cannot be read, which may be either.
    fn interpreter(file: &Path) -> io::Result<Option<PathBuf>> {
        let mut head = Vec::with_capacity(512);
        std::fs::File::open(file)?
            .take(512)
            .read_to_end(&mut head)?;
        let Some(rest) = head.strip_prefix(b"#!") else {
            return Ok(None);
        };
        let line_end =
            |b: &u8| matches!(b, b'\n' | b'\0') || (cfg!(target_os = "macos") && *b == b'#');
        let line = rest.split(line_end).next().unwrap_or_default();
        let name = line
            .split(|&b| b == b' ' || b == b'\t')
            .find(|word| !word.is_empty())
            .unwrap_or_default();
        Ok(Some(PathBuf::from(OsStr::from_bytes(name))))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::ffi::OsString;
    use std::io::{Read, Write};

    use portable_pty::{CommandBuilder, PtySize};

    /// What a child sees of how it was started: argv[0], the working
    /// directory, the environment (its names, and a checksum of the whole, so
    /// that a failure never prints a value), the descriptors it holds, the
    /// umask, and whether it leads a process group that is the PTY's
    /// foreground one with the PTY as its controlling terminal. It then waits
    /// for a line, so that nothing it printed can be lost to an exit before
    /// the read.
    const PROBE: &str = r#"printf 'argv0=%s\n' "$0"
pwd
env | sed 's/=.*//' | LC_ALL=C sort | tr '\n' ' '; echo
env | LC_ALL=C sort | cksum
ls /dev/fd | tr '\n' ' '; echo
umask
[ "$(ps -o pgid= -p $$ | tr -d ' ')" = "$$" ] && echo group-leader
[ "$(ps -o tpgid= -p $$ | tr -d ' ')" = "$$" ] && echo foreground
exec 3</dev/tty && echo controlling-tty
echo probe-finished
read _"#;

    /// Run [`PROBE`] in a fresh PTY, through termlens's own spawn or through
    /// the PTY layer's, and return what it printed.
    fn probe(through_termlens: bool) -> String {
        let (pair, lifecycle) = crate::terminal::open_pty(PtySize {
            rows: 80,
            cols: 300,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("a PTY opens");
        let mut cmd = CommandBuilder::new("sh");
        cmd.args(["-c", PROBE]);
        cmd.cwd(std::env::current_dir().expect("a working directory"));
        let env = [(OsString::from("TERM"), OsString::from("xterm-256color"))];
        for (key, value) in &env {
            cmd.env(key, value);
        }
        let mut reader = pair.master.try_clone_reader().expect("a reader");
        let mut writer = pair.master.take_writer().expect("a writer");
        let mut child = if through_termlens {
            super::unix::spawn(pair.master.as_ref(), &cmd, false, &env)
                .expect("termlens's own spawn takes this command")
                .expect("sh starts")
        } else {
            pair.slave.spawn_command(cmd).expect("sh starts")
        };
        drop(pair.slave);
        drop(lifecycle);

        // Read up to the marker's line ending, not just the marker: a read
        // that stops between the two leaves one side a line shorter.
        let mut seen = Vec::new();
        let mut chunk = [0; 4096];
        while !String::from_utf8_lossy(&seen)
            .replace("\r\n", "\n")
            .contains("probe-finished\n")
        {
            let read = reader.read(&mut chunk).expect("the PTY reads");
            assert!(
                read > 0,
                "the probe ended early:\n{}",
                String::from_utf8_lossy(&seen)
            );
            seen.extend_from_slice(&chunk[..read]);
        }
        writer.write_all(b"\n").expect("the PTY writes");
        child.wait().expect("sh exits");
        let _teardown = crate::terminal::pty_lifecycle_guard();
        drop(pair.master);
        String::from_utf8_lossy(&seen).replace("\r\n", "\n")
    }

    /// termlens's own spawn starts the child the PTY layer would have
    /// started, as far as the child can tell, so a portable-pty release that
    /// changes its spawn fails here instead of drifting apart from this one.
    #[test]
    fn the_child_cannot_tell_which_spawn_started_it() {
        let ours = probe(true);
        assert!(
            ours.contains("group-leader\nforeground\ncontrolling-tty\nprobe-finished"),
            "the probe did not run to the end:\n{ours}"
        );
        assert_eq!(ours, probe(false));
    }
}
