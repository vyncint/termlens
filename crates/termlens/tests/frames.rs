//! `wait_frame` (DEC 2026 synchronized output): predicates run only on
//! complete frames, torn repaints are never observable, and apps that
//! don't speak synchronized output fail with guidance instead of silence.

use std::time::{Duration, Instant};

use termlens::{Error, Key, Terminal};

mod common;
use common as util;

fn spawn_form_echo() -> termlens::Result<Terminal> {
    Terminal::builder()
        .size(80, 24)
        .timeout(Duration::from_secs(10))
        .env_clear()
        .spawn(util::fixture_bin("form-echo"))
}

#[test]
fn the_frame_completed_before_the_call_is_evaluated() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    // The fixture's first synchronized frame has long completed by the time
    // this wait starts; entry evaluation must still see it.
    t.wait_frame(|s| s.contains("form-echo ready"))?;
    t.send(Key::Esc);
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn a_frame_is_internally_consistent() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    t.wait_frame(|s| s.contains("form-echo ready"))?;

    t.send_str("hi");
    // Both the input line and the last-key line are painted in the same
    // synchronized update; a frame satisfying one must satisfy the other.
    t.wait_frame(|s| s.contains("input: hi"))?;
    let frame_ok = std::cell::Cell::new(false);
    t.wait_frame(|s| {
        if s.contains("input: hi") {
            frame_ok.set(s.contains("last: char:i"));
            true
        } else {
            false
        }
    })?;
    assert!(
        frame_ok.get(),
        "frame satisfied one row but not its sibling"
    );

    t.send(Key::Esc);
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn a_torn_repaint_is_never_observed() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    t.wait_frame(|s| s.contains("form-echo ready"))?;

    // F2 paints "torn: left", flushes, sleeps 150ms, then paints " right"
    // and ends the synchronized update. The bytes arrive in two bursts;
    // the frame completes only with the second.
    t.send(Key::F(2));
    let mut observed = None;
    t.wait_frame(|s| {
        if s.contains("torn: left") {
            observed = Some(s.row_text(5));
            true
        } else {
            false
        }
    })?;
    let row = observed.expect("predicate matched");
    assert!(
        row.contains("torn: left right"),
        "wait_frame observed a torn frame: {row:?}"
    );

    t.send(Key::Esc);
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn apps_without_synchronized_output_time_out_with_guidance() {
    // hello-tui never emits DEC 2026.
    let mut t = Terminal::builder()
        .size(80, 24)
        .timeout(Duration::from_millis(500))
        .env_clear()
        .spawn(util::fixture_bin("hello-tui"))
        .unwrap();
    t.wait_until(|s| s.contains("╯")).unwrap();

    let start = Instant::now();
    let err = t.wait_frame(|_| true).unwrap_err();
    assert!(start.elapsed() >= Duration::from_millis(500));

    let msg = err.to_string();
    assert!(msg.contains("2026"), "no guidance in: {msg}");
    assert!(msg.contains("wait_until"), "no alternative named in: {msg}");
    assert!(err.screen().is_some(), "timeout must still embed a screen");

    t.send(Key::Char('q'));
    assert!(t.wait_exit().unwrap().success());
}

/// The timeout error must show the screen as it is *now*, like every
/// other wait: its header says so, and in CI the embedded dump is often
/// the only evidence available. The last completed frame can be
/// arbitrarily old.
#[test]
fn wait_frame_timeouts_embed_the_live_screen_not_the_last_frame() {
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(400))
        .args([
            "-c",
            // One synchronized frame, then unbracketed output that will
            // never complete a frame.
            r"printf '\033[?2026h\033[HOLD FRAME\033[?2026l'; printf '\r\nLIVE SCREEN'; read quit",
        ])
        .spawn("sh")
        .unwrap();
    t.wait_frame(|s| s.contains("OLD FRAME")).unwrap();
    t.wait_until(|s| s.contains("LIVE SCREEN")).unwrap();

    let err = t.wait_frame(|s| s.contains("never painted")).unwrap_err();
    let screen = err.screen().expect("timeouts embed a screen");
    assert!(
        screen.contains("LIVE SCREEN"),
        "the embedded screen is stale — it must match the header:\n{screen}"
    );
    // The frame count still tells you what wait_frame actually saw.
    assert!(
        err.to_string().contains("1 complete frames observed"),
        "the frame count belongs in the message: {err}"
    );
}

/// Several frames can complete inside one read. Each must stay
/// observable, in the order the application drew them — a progress
/// counter ticking 1, 2, 3 in a single write used to be visible only
/// at 3.
#[test]
fn every_frame_of_a_burst_is_observable_in_order() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            // Three complete frames in ONE write, then park.
            concat!(
                r"printf '\033[?2026h\033[HSTEP 1\033[?2026l",
                r"\033[?2026h\033[HSTEP 2\033[?2026l",
                r"\033[?2026h\033[HSTEP 3\033[?2026l'; read guard"
            ),
        ])
        .spawn("sh")?;

    // Wait for the last one first, so all three have certainly arrived
    // (and been coalesced into as few reads as the OS chose).
    t.wait_frame(|s| s.contains("STEP 3"))?;
    // Every intermediate frame is still there.
    t.wait_frame(|s| s.contains("STEP 1"))?;
    t.wait_frame(|s| s.contains("STEP 2"))?;

    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The retention bound is real and documented: beyond it, the oldest
/// frames are dropped.
#[test]
fn a_burst_longer_than_the_retention_bound_drops_its_oldest_frames() -> termlens::Result<()> {
    // 12 frames in one write, against a retention bound of 8.
    let mut script = String::new();
    for n in 1..=12 {
        script.push_str(&format!(r"\033[?2026h\033[HFRAME {n:02}\033[?2026l"));
    }
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(600))
        .args(["-c", &format!("printf '{script}'; read guard")])
        .spawn("sh")?;

    t.wait_frame(|s| s.contains("FRAME 12"))?;
    // The most recent 8 are retained: 05..=12.
    t.wait_frame(|s| s.contains("FRAME 05"))?;
    // The first four are gone, and the error says how many were seen.
    let err = t.wait_frame(|s| s.contains("FRAME 01")).unwrap_err();
    assert!(
        err.to_string().contains("12 complete frames observed"),
        "the frame count belongs in the message: {err}"
    );
    Ok(())
}

#[test]
fn wait_frame_fails_fast_on_eof() {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(30))
        .args(["-c", r"printf '\033[?2026hdone\033[?2026l'; read guard"])
        .spawn("sh")
        .unwrap();
    t.wait_frame(|s| s.contains("done")).unwrap();
    t.send(Key::Enter);

    let start = Instant::now();
    let err = t.wait_frame(|s| s.contains("never painted")).unwrap_err();
    assert!(matches!(err, Error::Eof { .. }), "expected Eof, got: {err}");
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "EOF should fail fast"
    );
}

#[test]
fn wait_idle_does_not_resolve_inside_an_open_synchronized_update() {
    // The frame never ends: BSU, content, then the app parks on `read`.
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(600))
        .args(["-c", r"printf '\033[?2026hhalf a frame'; read guard"])
        .spawn("sh")
        .unwrap();
    t.wait_until(|s| s.contains("half a frame")).unwrap();

    let err = t.wait_idle(Duration::from_millis(100)).unwrap_err();
    assert!(
        matches!(err, Error::Timeout { .. }),
        "an open synchronized update must not count as idle: {err}"
    );
    // Drop kills the parked child.
}
