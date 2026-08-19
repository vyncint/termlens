//! Every wait takes a per-call deadline, and the error reports the
//! deadline that actually applied.

use std::time::{Duration, Instant};

use termlens::{Error, Key, Terminal};

/// The builder default is deliberately far too short for the app; only
/// the per-call override can see each wait through.
fn slow_app(script: &str) -> Terminal {
    Terminal::builder()
        .size(80, 24)
        .env_clear()
        .timeout(Duration::from_millis(150))
        .args(["-c", script])
        .spawn("/bin/sh")
        .expect("spawn")
}

#[test]
fn wait_frame_for_overrides_the_builder_default() -> termlens::Result<()> {
    let mut t = slow_app(r"sleep 1; printf '\033[?2026hlate frame\033[?2026l'; read guard");
    t.wait_frame_for(|s| s.contains("late frame"), Duration::from_secs(30))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit_for(Duration::from_secs(30))?.success());
    Ok(())
}

#[test]
fn wait_idle_for_overrides_the_builder_default() -> termlens::Result<()> {
    let mut t = slow_app(r"printf busy; read guard");
    // A 200ms quiet period cannot be observed under the 150ms builder
    // deadline at all — the wait would expire before the silence does.
    let start = Instant::now();
    t.wait_idle_for(Duration::from_millis(200), Duration::from_secs(30))?;
    assert!(
        start.elapsed() >= Duration::from_millis(200),
        "resolved before the quiet period elapsed: {:?}",
        start.elapsed()
    );
    assert!(t.screen().contains("busy"), "{}", t.screen());
    // (Asserting that the unqualified `wait_idle` still fails here would
    // be wrong: the terminal has now been silent for longer than the
    // quiet period, so it resolves immediately without consulting any
    // deadline. `per_call_timeouts_report_their_own_deadline` covers the
    // deadline behaviour against a still-chattering child.)

    t.send(Key::Enter)?;
    assert!(t.wait_exit_for(Duration::from_secs(30))?.success());
    Ok(())
}

#[test]
fn wait_exit_for_overrides_the_builder_default() -> termlens::Result<()> {
    let mut t = slow_app("sleep 1; exit 3");
    let status = t.wait_exit_for(Duration::from_secs(30))?;
    assert_eq!(status.code(), Some(3), "status: {status}");
    Ok(())
}

/// A per-call timeout also cuts a generous default short, and the error
/// reports the deadline that actually applied — not the builder's.
#[test]
fn per_call_timeouts_report_their_own_deadline() {
    let mut t = Terminal::builder()
        .size(80, 24)
        .env_clear()
        .timeout(Duration::from_secs(30))
        .args(["-c", "read guard"])
        .spawn("/bin/sh")
        .expect("spawn");

    for (label, err) in [
        (
            "wait_frame_for",
            t.wait_frame_for(|s| s.contains("never"), Duration::from_millis(120))
                .unwrap_err(),
        ),
        (
            "wait_idle_for",
            t.wait_idle_for(Duration::from_secs(5), Duration::from_millis(120))
                .unwrap_err(),
        ),
        (
            "wait_exit_for",
            t.wait_exit_for(Duration::from_millis(120)).unwrap_err(),
        ),
    ] {
        match err {
            Error::Timeout { timeout, .. } => assert_eq!(
                timeout,
                Duration::from_millis(120),
                "{label} reported the wrong deadline"
            ),
            other => panic!("{label}: expected a timeout, got {other}"),
        }
    }
}

/// The overrides must not cost wall-clock when they are not needed.
#[test]
fn a_short_per_call_timeout_fails_fast() {
    let mut t = Terminal::builder()
        .env_clear()
        .timeout(Duration::from_secs(60))
        .args(["-c", "read guard"])
        .spawn("/bin/sh")
        .expect("spawn");
    let start = Instant::now();
    let _ = t.wait_frame_for(|s| s.contains("never"), Duration::from_millis(100));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "the per-call deadline did not cut the 60s default short: {:?}",
        start.elapsed()
    );
}
