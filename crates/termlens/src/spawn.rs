//! Starting the child inside the PTY.
//!
//! On Unix termlens forks and executes the child itself instead of handing
//! the command to the PTY layer's `spawn_command`. Between fork and exec,
//! portable-pty closes every descriptor above stdio, and one of them is the
//! pipe through which std reports a failed `exec`. A program that exists but
//! cannot be executed, such as a script whose `#!` interpreter is missing,
//! then died of a runtime abort, and `spawn` returned `Ok` with a child
//! already killed by SIGABRT (#563).
//!
//! So the command is built the way the PTY layer builds it, with the same
//! program resolution, working directory, environment, session and signals,
//! and one difference: descriptors above stdio are marked close-on-exec
//! instead of closed. A successful exec closes the same set the PTY layer
//! closed, and a failed one can still report why.
//!
//! Whatever this path is not sure of goes to the PTY layer unchanged: a
//! working directory that is not a directory (the PTY layer substitutes the
//! home directory), a program that does not resolve (the PTY layer says why,
//! in the words termlens has always reported), a slave device it cannot name
//! or open. Windows always goes there, because ConPTY reports a failed
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
        let argv = cmd.get_argv();
        let program = argv.first()?;
        let dir = Path::new(cmd.get_cwd()?);
        if !dir.is_dir() {
            return None;
        }
        let resolved = resolve(program, dir, cmd.get_env("PATH"))?;
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
            .current_dir(dir)
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
        // takes the PTY layer's own pre_exec step by step. signal, sigemptyset,
        // sigprocmask, setsid, ioctl and fcntl are async-signal-safe and act
        // on the child alone. Listing /dev/fd allocates, as the PTY layer's
        // listing always did at this point.
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
            Err(error) => Err(explain(&error, &resolved, dir)),
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
    /// where the mode bits would be a guess.
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

    /// Why the child could not be started, naming the culprit where the
    /// errno alone would mislead. `ENOENT` for a file that plainly exists
    /// means a file the kernel went looking for is missing: the `#!`
    /// interpreter of a script, or the dynamic loader of a binary.
    fn explain(error: &io::Error, program: &Path, dir: &Path) -> String {
        if error.raw_os_error() == Some(libc::ENOENT) {
            if !dir.is_dir() {
                return format!("cannot enter {}: {error}", dir.display());
            }
            if program.is_file() {
                return match interpreter(program) {
                    Some(interpreter) if !dir.join(&interpreter).exists() => format!(
                        "cannot execute {}: its #! interpreter {interpreter:?} does not exist",
                        program.display()
                    ),
                    Some(interpreter) => format!(
                        "cannot execute {}: {error}; its #! interpreter {interpreter:?} \
                         exists, so what is missing is something the interpreter needs",
                        program.display()
                    ),
                    None => format!(
                        "cannot execute {}: {error}; the file exists, so what is missing \
                         is its dynamic loader",
                        program.display()
                    ),
                };
            }
        }
        format!("cannot execute {}: {error}", program.display())
    }

    /// The interpreter a script's `#!` line names, read the way the kernel
    /// reads it: the first word after `#!`, ended by a space, a tab or the
    /// newline. A `\r` stays part of the name, which is why a script saved
    /// with CRLF line endings cannot run.
    fn interpreter(script: &Path) -> Option<PathBuf> {
        let mut head = Vec::with_capacity(256);
        std::fs::File::open(script)
            .ok()?
            .take(256)
            .read_to_end(&mut head)
            .ok()?;
        let line = head.strip_prefix(b"#!")?.split(|&b| b == b'\n').next()?;
        let name = line
            .split(|&b| b == b' ' || b == b'\t')
            .find(|word| !word.is_empty())?;
        Some(PathBuf::from(OsStr::from_bytes(name)))
    }
}
