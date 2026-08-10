//! Process ergonomics: working directory, pid, signals, and the per-call
//! wait timeout.

use std::time::{Duration, Instant};

use termlens::{Error, Key, Signal, Terminal};

#[test]
fn current_dir_runs_the_child_where_asked() -> termlens::Result<()> {
    // Canonicalize: /tmp is a symlink on macOS and `pwd` reports the real
    // path the kernel put the process in.
    let dir = std::env::temp_dir().canonicalize()?;
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .current_dir(&dir)
        .args(["-c", "pwd; read _"])
        .spawn("sh")?;
    t.wait_until(|s| s.contains(dir.to_str().expect("utf-8 temp dir")))?;
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn pid_reports_the_direct_child() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args(["-c", r#"printf 'pid:%s;' "$$"; read _"#])
        .spawn("sh")?;
    let pid = t.pid().expect("unix reports pids");
    // The shell's $$ is the exact process the harness spawned.
    t.wait_until(|s| s.contains(&format!("pid:{pid};")))?;
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn signal_term_exercises_the_graceful_shutdown_path() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            "trap 'printf got-term; exit 7' TERM; printf ready; while :; do sleep 0.05; done",
        ])
        .spawn("sh")?;
    t.wait_until(|s| s.contains("ready"))?;

    t.signal(Signal::Term)?;
    t.wait_until(|s| s.contains("got-term"))?;
    let status = t.wait_exit()?;
    assert_eq!(status.code(), 7, "status: {status}");
    assert_eq!(status.signal(), None, "trapped, not killed: {status}");
    Ok(())
}

#[test]
fn signal_after_reap_is_a_typed_error_not_a_stray_kill() {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args(["-c", "exit 0"])
        .spawn("sh")
        .unwrap();
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
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(200))
        .args(["-c", "sleep 1; printf late-bloomer; read _"])
        .spawn("sh")?;
    t.wait_until_for(|s| s.contains("late-bloomer"), Duration::from_secs(30))?;
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn wait_until_for_overrides_the_default_timeout_downward() {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(30))
        .args(["-c", "read _"])
        .spawn("sh")
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
