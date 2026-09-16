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
        &["--sleep", "1s", "late-bloomer", "--wait"],
    )?;
    t.wait_until_for(|s| s.contains("late-bloomer"), Duration::from_secs(30))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
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
