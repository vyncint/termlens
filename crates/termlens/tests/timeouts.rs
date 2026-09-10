//! Every wait takes a per-call deadline, and the error reports the
//! deadline that actually applied.

use std::time::{Duration, Instant};

use termlens::{Error, Key, Terminal};

mod common;

/// The builder default is deliberately far too short for the app; only
/// the per-call override can see each wait through. Steps are documented
/// in `fixtures/emit/src/main.rs`.
fn slow_app(steps: &[&str]) -> Terminal {
    common::spawn_emit(
        Terminal::builder()
            .size(80, 24)
            .env_clear()
            .timeout(Duration::from_millis(150)),
        steps,
    )
    .expect("spawn")
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY closes a DEC 2026 bracket before the content it wrapped, so no frame holds what was drawn (#149)"
)]
fn wait_frame_for_overrides_the_builder_default() -> termlens::Result<()> {
    let mut t = slow_app(&[
        "--sleep",
        "1s",
        "--raw",
        r"\e[?2026hlate frame\e[?2026l",
        "--wait",
    ]);
    t.wait_frame_for(|s| s.contains("late frame"), Duration::from_secs(30))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit_for(Duration::from_secs(30))?.success());
    Ok(())
}

#[test]
fn wait_idle_for_overrides_the_builder_default() -> termlens::Result<()> {
    let mut t = slow_app(&["busy", "--wait"]);
    // Let the child's first output land before measuring the silence.
    // `wait_idle` counts quiet from the call, and a child that has not
    // started writing yet is trivially quiet: the stress workflow saw this
    // resolve after 200 ms of *nothing* on a loaded Windows runner (ConPTY
    // holds the child until its startup handshake is answered), and the
    // screen assertion below then failed on an empty grid — a race in this
    // test, not in the wait. The per-call override is still what is under
    // test: a 200 ms quiet period cannot be observed under the 150 ms
    // builder deadline at all.
    t.wait_until_for(|s| s.contains("busy"), Duration::from_secs(30))?;
    let start = Instant::now();
    t.wait_idle_for(Duration::from_millis(200), Duration::from_secs(30))?;
    // The quiet window counts from the last byte, which landed a moment
    // before `start`, so the elapsed time is a hair under 200 ms; what the
    // test needs is that it is past the 150 ms builder deadline, under
    // which the same call would have errored instead.
    assert!(
        start.elapsed() > Duration::from_millis(150),
        "resolved inside the builder deadline, so the override did not apply: {:?}",
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
    let mut t = slow_app(&["--sleep", "1s", "--exit", "3"]);
    let status = t.wait_exit_for(Duration::from_secs(30))?;
    assert_eq!(status.code(), Some(3), "status: {status}");
    Ok(())
}

/// A per-call timeout also cuts a generous default short, and the error
/// reports the deadline that actually applied — not the builder's.
#[test]
fn per_call_timeouts_report_their_own_deadline() {
    let mut t = common::spawn_emit(
        Terminal::builder()
            .size(80, 24)
            .env_clear()
            .timeout(Duration::from_secs(30)),
        &["--wait"],
    )
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
    let mut t = common::spawn_emit(
        Terminal::builder()
            .env_clear()
            .timeout(Duration::from_secs(60)),
        &["--wait"],
    )
    .expect("spawn");
    let start = Instant::now();
    let _ = t.wait_frame_for(|s| s.contains("never"), Duration::from_millis(100));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "the per-call deadline did not cut the 60s default short: {:?}",
        start.elapsed()
    );
}
