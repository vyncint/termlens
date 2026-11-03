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

/// Run `body` on a thread of its own and fail if it has not finished in a
/// minute. A wait given no deadline that never sees its condition would hang
/// the suite; this turns that regression into a failure here.
fn bounded(body: impl FnOnce() -> termlens::Result<()> + Send + 'static) -> termlens::Result<()> {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(body());
    });
    match finished.recv_timeout(Duration::from_secs(60)) {
        Ok(outcome) => outcome,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("a wait with no deadline never saw what it waited for")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("a wait panicked; see above")
        }
    }
}

/// Draws one complete frame, then holds the terminal open for an Enter.
fn ready_frame(timeout: Duration) -> termlens::Result<Terminal> {
    common::spawn_emit(
        Terminal::builder().timeout(timeout),
        &["--raw", r"\e[?2026hready\e[?2026l", "--wait"],
    )
}

/// `Duration::MAX` says "no deadline", and every per-call wait takes it.
/// Each computed `Instant::now() + timeout`, which panics with "overflow
/// when adding duration to instant" (#629).
#[test]
fn every_wait_takes_duration_max_as_no_deadline() -> termlens::Result<()> {
    bounded(|| {
        let mut t = ready_frame(Duration::from_secs(10))?;
        t.wait_until_for(|s| s.contains("ready"), Duration::MAX)?;
        // ConPTY closes the bracket before what it wrapped (#149).
        #[cfg(not(windows))]
        t.wait_frame_for(|s| s.contains("ready"), Duration::MAX)?;
        t.wait_idle_for(Duration::from_millis(50), Duration::MAX)?;
        t.wait_stable_for(Duration::from_millis(50), Duration::MAX)?;
        t.snapshot_after_for(|s| s.contains("ready"), Duration::MAX)?;
        #[cfg(feature = "regex")]
        t.wait_until_matches_for(&regex::Regex::new("rea.y").expect("a regex"), Duration::MAX)?;
        t.send(Key::Enter)?;
        assert!(t.wait_exit_for(Duration::MAX)?.success());
        Ok(())
    })
}

/// A builder timeout of `Duration::MAX` is every default deadline, and
/// every wait that uses it takes it, as does the send that waits for its
/// acknowledgement (#629).
#[test]
fn a_builder_timeout_of_duration_max_reaches_every_wait() -> termlens::Result<()> {
    bounded(|| {
        let mut t = ready_frame(Duration::MAX)?;
        t.wait_until(|s| s.contains("ready"))?;
        #[cfg(not(windows))]
        t.wait_frame(|s| s.contains("ready"))?;
        t.wait_idle(Duration::from_millis(50))?;
        t.wait_stable(Duration::from_millis(50))?;
        t.snapshot_after(|s| s.contains("ready"))?;
        #[cfg(feature = "regex")]
        t.wait_until_matches(&regex::Regex::new("rea.y").expect("a regex"))?;
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success());
        Ok(())
    })
}
