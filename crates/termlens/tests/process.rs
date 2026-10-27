//! Process ergonomics: working directory, pid, signals, and the per-call
//! wait timeout.

use std::time::{Duration, Instant};

#[cfg(unix)]
use termlens::Signal;
use termlens::{Error, Key, Terminal};

mod common;

/// The `emit` fixture; steps are documented in `fixtures/emit/src/main.rs`.
fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(Terminal::builder().timeout(Duration::from_secs(10)), steps)
}

/// `to` relative to `from`, both absolute: `..` for each component of
/// `from` past the common prefix, then the rest of `to`.
#[cfg(unix)]
fn relative_path(from: &std::path::Path, to: &std::path::Path) -> std::path::PathBuf {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = std::path::PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for part in &to[common..] {
        out.push(part);
    }
    out
}

/// A relative `current_dir` is the test process's, and a relative program
/// path then resolves inside it. Handed to the PTY layer relative, the
/// directory was applied twice — joined onto `./emit`, then entered by the
/// child before exec — so the exec looked for `dir/./emit` from inside
/// `dir`, and the child died of a Rust runtime abort it could not report.
/// Seen through `termlens inspect --cwd ./examples ./myapp`.
///
/// The scratch directory sits deeper than the test's own working directory
/// so that the doubled path cannot land back on it: a directory as deep as
/// this one, such as `target/debug` beside `crates/termlens`, maps onto
/// itself and hid the bug. The test checks that before it trusts a pass.
/// Unix-only, as the CLI's relative-program test is: the fixture is
/// linked in, and the program path is spelled the Unix way.
#[cfg(unix)]
#[test]
fn a_relative_current_dir_is_the_test_process_s_and_a_relative_program_resolves_in_it(
) -> termlens::Result<()> {
    let emit = common::fixture_bin("emit").canonicalize()?;
    let scratch = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("relative-cwd-{}", std::process::id()))
        .join("a")
        .join("b");
    std::fs::create_dir_all(&scratch)?;
    let scratch = scratch.canonicalize()?;
    let link = scratch.join("emit");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&emit, &link)?;

    let here = std::env::current_dir()?.canonicalize()?;
    let relative = relative_path(&here, &scratch);
    assert!(
        relative.is_relative() && relative.join("emit").is_file(),
        "{} from {}",
        relative.display(),
        here.display()
    );
    assert!(
        !scratch.join(&relative).join("emit").exists(),
        "applied twice, {} still finds the program, so this test could not fail",
        relative.display()
    );

    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .current_dir(&relative)
        .args(["--cwd", "--wait"])
        .spawn("./emit")?;
    // `--cwd` prints the directory as the kernel put the process there.
    t.wait_until(|s| s.contains(scratch.to_str().expect("utf-8 scratch directory")))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    let _ = std::fs::remove_dir_all(
        scratch
            .parent()
            .and_then(std::path::Path::parent)
            .expect("the scratch root"),
    );
    Ok(())
}

/// A program that exists but cannot be executed is a spawn error naming what
/// is missing, not a child dead of SIGABRT (#563). The PTY layer closed the
/// pipe std reports a failed exec through, so the child aborted writing to
/// it and `spawn` returned `Ok`. The usual culprit is the interpreter a
/// script's `#!` line names. The sneakiest is the `\r` that CRLF line
/// endings leave on it, which the message shows escaped. The messages keep
/// the prefix the skill's failure table promises.
#[cfg(unix)]
#[test]
fn a_script_whose_interpreter_cannot_run_fails_to_spawn_and_names_it() -> termlens::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let scratch = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("exec-failure-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let refused = |name: &str, script: &str, mode: u32| -> termlens::Result<String> {
        let path = scratch.join(name);
        common::write_script(&path, script);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
        match Terminal::builder()
            .timeout(Duration::from_secs(10))
            .spawn(&path)
        {
            Ok(_) => panic!("{name}: spawned, and its interpreter cannot run"),
            Err(Error::Spawn { reason, .. }) => Ok(reason),
            Err(other) => panic!("{name}: not a spawn error: {other}"),
        }
    };
    for (name, script, culprit) in [
        (
            "missing",
            "#!/nonexistent/interpreter\necho unreachable\n",
            r#"its #! interpreter "/nonexistent/interpreter" does not exist"#,
        ),
        (
            "crlf",
            "#!/bin/sh\r\necho unreachable\r\n",
            r#"its #! interpreter "/bin/sh\r" does not exist"#,
        ),
    ] {
        let reason = refused(name, script, 0o755)?;
        assert!(
            reason.starts_with("cannot execute ") && reason.ends_with(culprit),
            "{name}: the message must name the interpreter: {reason}"
        );
    }
    // An interpreter that is there but is not a program is named as well.
    // The kernel's EACCES alone would blame the script, which can be run.
    let plain = scratch.join("plain-text");
    std::fs::write(&plain, "not a program\n")?;
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644))?;
    let reason = refused(
        "plain-interpreter",
        &format!("#!{}\n", plain.display()),
        0o755,
    )?;
    assert!(
        reason.ends_with(&format!(
            "its #! interpreter {plain:?} is not an executable file"
        )),
        "{reason}"
    );
    // A script that may be run but not read cannot show its `#!` line, so
    // the message names both suspects rather than guessing the loader. Root
    // reads it anyway, and the interpreter is named outright.
    let reason = refused("execute-only", "#!/nonexistent/interpreter\n", 0o111)?;
    assert!(reason.contains("#! interpreter"), "{reason}");
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}

/// Any exec the kernel refuses is a spawn error carrying its reason, not
/// only a missing interpreter. Every Unix refuses an argument this long,
/// with E2BIG.
#[cfg(unix)]
#[test]
fn an_argument_the_kernel_refuses_is_a_spawn_error_with_its_reason() {
    let spawned = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args(["-c", "true", &"x".repeat(2 << 20)])
        .spawn("sh");
    // Matched, not unwrapped: the terminal and the error both carry the
    // 2 MiB command line, which a failing assertion would print whole.
    match spawned {
        Ok(_) => panic!("a 2 MiB argument is past every Unix's limit, and it spawned"),
        Err(Error::Spawn { reason, .. }) => {
            assert!(reason.contains("Argument list too long"), "{reason}");
        }
        Err(_) => panic!("not a spawn error"),
    }
}

/// A program named with a directory part is a path from the working
/// directory, as `std::process::Command` and `execvp` take it. The PTY
/// layer's rules searched PATH for `sub/x` and never the directory the test
/// runs in, so `spawn("target/debug/app")` failed with "No viable
/// candidates found in PATH" — or, under `env_clear()`, could not even
/// resolve PATH (#592).
#[cfg(unix)]
#[test]
fn a_program_with_a_directory_part_is_found_from_the_working_directory() -> termlens::Result<()> {
    let scratch = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("dir-part-{}", std::process::id()));
    std::fs::create_dir_all(scratch.join("sub"))?;
    common::write_script(
        &scratch.join("sub/x"),
        "#!/bin/sh\necho ran from sub\nread _\n",
    );
    for clear in [false, true] {
        let mut builder = Terminal::builder()
            .timeout(Duration::from_secs(10))
            .current_dir(&scratch);
        if clear {
            builder = builder.env_clear();
        }
        let mut t = builder.spawn("sub/x")?;
        t.wait_until(|s| s.contains("ran from sub"))?;
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success(), "env_clear: {clear}");
    }
    Ok(())
}

/// A child that ignores the hang-up is gone all the same once `drop`
/// returns: it gets the hang-up's grace, then SIGKILL. `drop` now stops the
/// child itself, outside the lifecycle lock (#589), and must still not
/// leave one behind.
#[cfg(unix)]
#[test]
fn drop_stops_a_child_that_ignores_the_hangup() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            "trap '' HUP; echo ready; while :; do sleep 0.05; done",
        ])
        .spawn("sh")?;
    t.wait_until(|s| s.contains("ready"))?;
    let pid = t.pid().expect("a pid").to_string();
    drop(t);
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid])
        .stderr(std::process::Stdio::null())
        .status()?
        .success();
    assert!(!alive, "pid {pid} outlived the drop");
    Ok(())
}

/// A failure before the exec reads as std reports it, and does not blame a
/// program that is fine. A NUL in a variable is refused before anything is
/// forked. A working directory that cannot be entered fails in the child,
/// before the exec, and the directory is named.
#[cfg(unix)]
#[test]
fn a_failure_before_the_exec_does_not_blame_the_program() -> termlens::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    use termlens::TerminalBuilder;

    let reason =
        |builder: TerminalBuilder| match builder.timeout(Duration::from_secs(10)).spawn("sh") {
            Ok(_) => panic!("spawned"),
            Err(Error::Spawn { reason, .. }) => reason,
            Err(other) => panic!("not a spawn error: {other}"),
        };
    let nul = reason(Terminal::builder().env("A", "a\0b"));
    assert!(
        nul.contains("nul byte") && !nul.contains("cannot execute"),
        "{nul}"
    );

    let scratch = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("unenterable-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o600))?;
    // Root enters any directory, and then there is nothing to refuse.
    let refused = std::fs::metadata(scratch.join("probe"))
        .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied);
    let chdir = refused.then(|| reason(Terminal::builder().current_dir(&scratch)));
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))?;
    let _ = std::fs::remove_dir_all(&scratch);
    if let Some(chdir) = chdir {
        assert!(
            chdir.starts_with("cannot enter ") && chdir.contains("unenterable-"),
            "{chdir}"
        );
    }
    Ok(())
}

#[test]
fn current_dir_runs_the_child_where_asked() -> termlens::Result<()> {
    // Canonicalize: /tmp is a symlink on macOS and `--cwd` reports the real
    // path the kernel put the process in.
    let dir = std::env::temp_dir().canonicalize()?;
    let mut t = common::spawn_emit(
        Terminal::builder()
            .timeout(Duration::from_secs(10))
            .current_dir(&dir),
        &["--cwd", "--wait"],
    )?;
    t.wait_until(|s| s.contains(dir.to_str().expect("utf-8 temp dir")))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn pid_reports_the_direct_child() -> termlens::Result<()> {
    let mut t = emit(&["pid:", "--pid", ";", "--wait"])?;
    let pid = t.pid().expect("unix reports pids");
    // The fixture's own id is the exact process the harness spawned.
    t.wait_until(|s| s.contains(&format!("pid:{pid};")))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
#[cfg(unix)]
fn signal_term_exercises_the_graceful_shutdown_path() -> termlens::Result<()> {
    let mut t = emit(&["--on-term", "got-term", "7", "ready", "--idle"])?;
    t.wait_until(|s| s.contains("ready"))?;

    t.signal(Signal::Term)?;
    t.wait_until(|s| s.contains("got-term"))?;
    let status = t.wait_exit()?;
    assert_eq!(status.code(), Some(7), "status: {status}");
    assert_eq!(status.signal(), None, "trapped, not killed: {status}");
    Ok(())
}

#[test]
#[cfg(unix)]
fn every_signal_variant_reaches_the_child() -> termlens::Result<()> {
    for (signal, expected) in [
        (Signal::Int, "Interrupt"),
        (Signal::Term, "Terminated"),
        (Signal::Hup, "Hangup"),
        (Signal::Quit, "Quit"),
        (Signal::Usr1, "User defined signal 1"),
        (Signal::Usr2, "User defined signal 2"),
        (Signal::Kill, "Killed"),
    ] {
        let mut t = emit(&["READY", "--wait"])?;
        t.wait_until(|s| s.contains("READY"))?;
        t.signal(signal)?;
        let status = t.wait_exit()?;
        let actual = status.signal().expect("signal should terminate child");
        assert!(actual.contains(expected), "{signal:?}: {status}");
    }
    Ok(())
}

#[test]
#[cfg(unix)]
fn signal_after_reap_is_a_typed_error_not_a_stray_kill() {
    let mut t = emit(&["--exit", "0"]).unwrap();
    t.wait_exit().unwrap();

    let err = t.signal(Signal::Term).unwrap_err();
    assert!(matches!(err, Error::Input(_)), "got: {err}");
    assert!(
        err.to_string().contains("already exited"),
        "unhelpful message: {err}"
    );
}

#[test]
fn wait_until_for_overrides_the_default_timeout_upward() -> termlens::Result<()> {
    // Builder default far below the app's readiness; only the per-call
    // override can see this through.
    let mut t = common::spawn_emit(
        Terminal::builder().timeout(Duration::from_millis(200)),
        // The pause after `--wait` stands in for a slow reap, so the 200 ms
        // default on `wait_exit` fails every time rather than under load.
        &[
            "--sleep",
            "1s",
            "late-bloomer",
            "--wait",
            "--sleep",
            "300ms",
        ],
    )?;
    t.wait_until_for(|s| s.contains("late-bloomer"), Duration::from_secs(30))?;
    t.send(Key::Enter)?;
    // The exit is not under test either, and it does not always fit in the
    // 200 ms default: a loaded Windows runner took longer to reap the child
    // (stress run 35852222516, four threads).
    assert!(t.wait_exit_for(Duration::from_secs(30))?.success());
    Ok(())
}

#[test]
fn wait_until_for_overrides_the_default_timeout_downward() {
    let mut t = common::spawn_emit(
        Terminal::builder().timeout(Duration::from_secs(30)),
        &["--wait"],
    )
    .unwrap();
    let start = Instant::now();
    let err = t
        .wait_until_for(|s| s.contains("never shown"), Duration::from_millis(100))
        .unwrap_err();
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "the per-call timeout must cut the 30s default short"
    );
    match err {
        Error::Timeout { timeout, .. } => assert_eq!(timeout, Duration::from_millis(100)),
        other => panic!("expected a timeout, got: {other}"),
    }
    // Drop kills the parked child.
}

/// A signalled child has no exit code, and `code()` now says so. It used to
/// report the OS placeholder (1) alongside the true signal, so
/// `assert_eq!(status.code(), 1)` passed on a SIGTERM path — and would have
/// kept passing if the application later began exiting 1 for a real reason.
#[test]
#[cfg(unix)]
fn a_signalled_child_reports_no_exit_code() -> termlens::Result<()> {
    let mut t = emit(&["READY", "--wait"])?;
    t.wait_until(|s| s.contains("READY"))?;

    t.signal(termlens::Signal::Term)?;
    let status = t.wait_exit()?;

    assert!(!status.success(), "status: {status}");
    assert_eq!(
        status.code(),
        None,
        "a signalled child has no code: {status}"
    );
    assert!(
        status.signal().is_some(),
        "the signal is the answer: {status}"
    );
    // The Display no longer carries the invented number either.
    let shown = status.to_string();
    assert!(shown.starts_with("killed by signal"), "{shown}");
    assert!(!shown.contains("code"), "{shown}");
    Ok(())
}

/// The normal path is unchanged: a real exit code is still reported, now
/// wrapped in `Some`.
#[test]
fn a_normally_exited_child_still_reports_its_code() -> termlens::Result<()> {
    let mut t = emit(&["--exit", "7"])?;
    let status = t.wait_exit()?;
    assert_eq!(status.code(), Some(7), "status: {status}");
    assert_eq!(status.signal(), None);
    assert_eq!(status.to_string(), "exit code 7");
    Ok(())
}

/// The README's "What `TestBackend` cannot see" table promises a panic is
/// assertable with `s.contains("panicked")`, and `skills/termlens/SKILL.md`
/// §1 says the same; until #311 nothing in `fixtures/` ever panicked, so
/// neither claim had a test. This is the case users reach for when their
/// TUI dies in CI, and the alternate screen is the part that could have
/// eaten the message.
///
/// **Measured, not assumed**: the message survives both ways. A panic
/// raised inside the alternate screen lands in that buffer, next to what
/// the application had drawn; a panic raised after the application tore the
/// alternate screen down lands on the restored primary screen. The exit
/// status is an exit *code* of 101 — the value the Rust runtime uses — and
/// not a signal.
#[test]
fn a_panicking_child_puts_its_message_on_the_screen() -> termlens::Result<()> {
    // Wide enough that the message is one row: the runtime's own
    // `panicked at <file>:<line>` line wraps on a narrow grid, and a
    // wrapped needle is a test about the width, not about the panic.
    //
    // And every wait below names the *message*, never the word "panicked".
    // The runtime writes the location line and the message as separate
    // writes, so `contains("panicked")` is true one line before the message
    // exists — which went red on CI with the grid holding `panicked at
    // …:281:37:` and nothing under it. Waiting for the message is rule 3 of
    // the wait-semantics contract (docs/DESIGN.md §2): name the last thing
    // the application paints, so that its truth implies the rest arrived.
    // "panicked" is then asserted rather than waited on — it cannot be
    // absent once the line after it is there.
    let wide = || {
        Terminal::builder()
            .size(100, 10)
            .timeout(Duration::from_secs(10))
    };

    // 1. A plain child, which is what the README's table is about.
    let mut t = common::spawn_emit(wide(), &["drew this ", "--panic", "plain panic here"])?;
    t.wait_until(|s| s.contains("plain panic here"))?;
    let s = t.screen();
    assert!(s.contains("panicked"), "the word the README promises: {s}");
    assert!(
        s.contains("plain panic here"),
        "the message reaches the grid: {s}"
    );
    assert!(s.contains("drew this"), "and what was drawn before it: {s}");
    let status = t.wait_exit()?;
    assert_eq!(
        status.code(),
        Some(101),
        "the Rust runtime's code: {status}"
    );
    assert!(!status.success(), "{status}");

    // 2. Dying inside the alternate screen, with no panic hook to leave it
    // — the shape a TUI that panics mid-draw actually has.
    let mut t = common::spawn_emit(
        wide(),
        &[
            "--csi",
            "?1049h",
            "TUI drawing here",
            "--panic",
            "boom in the alt screen",
        ],
    )?;
    t.wait_until(|s| s.contains("boom in the alt screen"))?;
    let s = t.screen();
    assert!(s.contains("panicked"), "{s}");
    assert!(s.alternate_screen(), "nothing tore it down: {s}");
    assert!(
        s.contains("TUI drawing here"),
        "the message joins the frame it died on: {s}"
    );
    assert_eq!(t.wait_exit()?.code(), Some(101));

    // 3. And after the teardown a panic hook would do, where the message
    // lands on the restored primary screen instead.
    let mut t = common::spawn_emit(
        wide(),
        &[
            "--csi",
            "?1049h",
            "TUI drawing here",
            "--csi",
            "?1049l",
            "--panic",
            "boom after teardown",
        ],
    )?;
    t.wait_until(|s| s.contains("boom after teardown"))?;
    let s = t.screen();
    assert!(s.contains("panicked"), "{s}");
    assert!(!s.alternate_screen(), "the child left it: {s}");
    assert!(
        !s.contains("TUI drawing here"),
        "what the alternate screen held went with it: {s}"
    );
    assert_eq!(t.wait_exit()?.code(), Some(101));
    Ok(())
}

/// A pseudoconsole never reports EOF while it is open, so on Windows a
/// reaped child plus the drain grace is the terminal closing — measured from
/// the reap. Every wait used to restart the grace, so waits shorter than it
/// never saw the exit and a send to the dead child was not refused (#593).
#[cfg(windows)]
#[test]
fn short_waits_after_the_exit_see_the_terminal_close() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args(["/c", "echo done"])
        .spawn("cmd")?;
    t.wait_until(|s| s.contains("done"))?;
    // Waits of a fifth of the grace each, for four graces in all.
    for _ in 0..20 {
        match t.wait_until_for(|_| false, Duration::from_millis(100)) {
            Err(Error::Eof { .. }) => {
                assert!(
                    matches!(t.send_str("x"), Err(Error::Write { .. })),
                    "a closed terminal refuses input"
                );
                return Ok(());
            }
            Err(Error::Timeout { .. }) => {}
            other => panic!("only the exit can end this wait: {other:?}"),
        }
    }
    panic!("two seconds of short waits after the exit never saw the terminal close");
}
