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
    t.send(Key::Esc)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn a_frame_is_internally_consistent() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    t.wait_frame(|s| s.contains("form-echo ready"))?;

    t.send_str("hi")?;
    // Both the input line and the last-key line are painted in the same
    // synchronized update, so a frame satisfying one must satisfy the
    // other. The returned frame is what the predicate saw, so the sibling
    // row is checked on that exact instant rather than on a later
    // `screen()` that may already have moved on.
    let frame = t.wait_frame(|s| s.contains("input: hi"))?;
    assert!(
        frame.contains("last: char:i"),
        "frame satisfied one row but not its sibling:\n{frame}"
    );

    t.send(Key::Esc)?;
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
    t.send(Key::F(2))?;
    let row = t.wait_frame(|s| s.contains("torn: left"))?.row_text(5);
    assert!(
        row.contains("torn: left right"),
        "wait_frame observed a torn frame: {row:?}"
    );

    t.send(Key::Esc)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn apps_without_synchronized_output_time_out_with_guidance() {
    // hello-tui never emits DEC 2026.
    let mut t = Terminal::builder()
        .size(80, 24)
        // Generous default: the fixture's first paint is not what is under
        // test here, and on a loaded runner it can take a while. The
        // deadline that matters is the per-call one below.
        .timeout(Duration::from_secs(10))
        .env_clear()
        .spawn(util::fixture_bin("hello-tui"))
        .unwrap();
    t.wait_until(|s| s.contains("╯")).unwrap();

    let start = Instant::now();
    let err = t
        .wait_frame_for(|_| true, Duration::from_millis(500))
        .unwrap_err();
    assert!(start.elapsed() >= Duration::from_millis(500));

    let msg = err.to_string();
    assert!(msg.contains("2026"), "no guidance in: {msg}");
    assert!(msg.contains("wait_until"), "no alternative named in: {msg}");
    assert!(err.screen().is_some(), "timeout must still embed a screen");

    t.send(Key::Char('q')).unwrap();
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
    // The only frame drawn was already returned, so the message says the
    // application has not repainted rather than blaming the predicate.
    let msg = err.to_string();
    assert!(
        msg.contains("has not completed a repaint") && msg.contains("1 complete frame in total"),
        "the frame count and the reason belong in the message: {msg}"
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

    // Settle on the live screen first, so all three frames have certainly
    // arrived (and been coalesced into as few reads as the OS chose)
    // before any of them is consumed. `wait_until` observes the grid, not
    // the frame ring, so it leaves the cursor where it is.
    t.wait_until(|s| s.contains("STEP 3"))?;

    // Now every frame of the burst is observable, oldest first, and each
    // call returns the frame it matched.
    assert!(t.wait_frame(|s| s.contains("STEP 1"))?.contains("STEP 1"));
    assert!(t.wait_frame(|s| s.contains("STEP 2"))?.contains("STEP 2"));
    assert!(t.wait_frame(|s| s.contains("STEP 3"))?.contains("STEP 3"));

    t.send(Key::Enter)?;
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

    t.wait_until(|s| s.contains("FRAME 12"))?;
    // The most recent 8 are retained: 05..=12, so 05 is the oldest that
    // can still be observed.
    t.wait_frame(|s| s.contains("FRAME 05"))?;
    // The first four are gone, and the error says how many were seen.
    let err = t.wait_frame(|s| s.contains("FRAME 01")).unwrap_err();
    assert!(
        err.to_string().contains("12 in total"),
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
    t.send(Key::Enter).unwrap();

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

#[test]
fn an_unmatched_end_publishes_no_frame() {
    // `?2026l` with no Begin must not manufacture a frame out of whatever
    // is on the grid — and must leave the frame count at zero, since that
    // is what gates the diagnosis below.
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(600))
        .args([
            "-c",
            r"printf '\033[2J\033[HNO-BEGIN\033[?2026l'; read guard",
        ])
        .spawn("sh")
        .unwrap();
    t.wait_until(|s| s.contains("NO-BEGIN")).unwrap();

    let err = t.wait_frame(|s| s.contains("NO-BEGIN")).unwrap_err();
    assert!(
        err.to_string().contains("never emitted"),
        "a phantom frame would both match and suppress the diagnosis: {err}"
    );
}

#[test]
fn a_defensive_mode_reset_keeps_the_never_emitted_diagnosis() {
    // Verbatim from a real crash handler: applications reset terminal modes
    // defensively, and such a string contains `?2026l`. One stray End used
    // to replace the pointed diagnosis with a frame count, which reads as
    // "the app is frame-capable, your predicate is wrong".
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(600))
        .args([
            "-c",
            concat!(
                r"printf '\033[?2026l\033[?25h\033[?1000l\033[?1002l",
                r"\033[?1003l\033[?2004l\033[?1049l'; ",
                r"printf '\033[2J\033[HPLAIN-PAINT'; read guard"
            ),
        ])
        .spawn("sh")
        .unwrap();
    t.wait_until(|s| s.contains("PLAIN-PAINT")).unwrap();

    let err = t.wait_frame(|s| s.contains("NEVER-DRAWN")).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("never emitted a DEC 2026 synchronized update"),
        "the reset must not look like a repaint: {msg}"
    );
    assert!(
        !msg.contains("complete frames observed"),
        "no frame was drawn, so no count should be claimed: {msg}"
    );
}

#[test]
fn a_begin_end_pair_that_drew_nothing_is_still_a_frame() {
    // Deliberate: `frames_seen` counts repaints, not changes. An
    // application that opens and closes a synchronized update completed a
    // repaint, even if the result is identical — deciding otherwise would
    // mean diffing grids and calling a genuine no-op repaint a non-event.
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(5))
        .args([
            "-c",
            r"printf '\033[2J\033[HSTATIC'; printf '\033[?2026h\033[?2026l'; read guard",
        ])
        .spawn("sh")
        .unwrap();
    t.wait_frame(|s| s.contains("STATIC")).unwrap();
    t.send(Key::Enter).unwrap();
}

/// The headline case: `send(key); wait_frame(OLD_STATE)` used to pass on
/// the retained frame, and the assertion after it read the old screen. A
/// regression in which the key stopped working was invisible.
#[test]
fn a_superseded_frame_no_longer_satisfies_a_wait() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                r"printf '\033[?2026h\033[2J\033[HSTATE-A\033[?2026l'; read a; ",
                r"printf '\033[?2026h\033[2J\033[HSTATE-B\033[?2026l'; read b"
            ),
        ])
        .spawn("sh")?;

    assert!(t.wait_frame(|s| s.contains("STATE-A"))?.contains("STATE-A"));
    t.send(Key::Enter)?;

    let stale = t.wait_frame_for(|s| s.contains("STATE-A"), Duration::from_millis(700));
    assert!(
        matches!(stale, Err(Error::Timeout { .. })),
        "waiting for the superseded state must fail: {stale:?}"
    );

    // The frame the application actually drew is there for the asking.
    assert!(t.wait_frame(|s| s.contains("STATE-B"))?.contains("STATE-B"));

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn one_frame_cannot_satisfy_two_waits() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            r"printf '\033[?2026h\033[2J\033[HONLY-FRAME\033[?2026l'; read guard",
        ])
        .spawn("sh")?;

    t.wait_frame(|s| s.contains("ONLY-FRAME"))?;
    let again = t.wait_frame_for(|s| s.contains("ONLY-FRAME"), Duration::from_millis(700));
    assert!(
        matches!(again, Err(Error::Timeout { .. })),
        "one repaint must not satisfy two waits: {again:?}"
    );

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The burst is observable in emission order, which means out of order it
/// is *not*: a frame already returned is behind the cursor.
#[test]
fn a_burst_frame_asked_for_out_of_order_is_gone() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(700))
        .args([
            "-c",
            concat!(
                r"printf '\033[?2026h\033[HSTEP 1\033[?2026l",
                r"\033[?2026h\033[HSTEP 2\033[?2026l",
                r"\033[?2026h\033[HSTEP 3\033[?2026l'; read guard"
            ),
        ])
        .spawn("sh")?;

    t.wait_until(|s| s.contains("STEP 3"))?;
    t.wait_frame(|s| s.contains("STEP 3"))?;

    let backwards = t.wait_frame(|s| s.contains("STEP 1"));
    assert!(
        matches!(backwards, Err(Error::Timeout { .. })),
        "STEP 1 was already passed over: {backwards:?}"
    );
    Ok(())
}

/// `wait_frame` returns the instant the predicate saw, which can differ
/// from the live grid by the time the call returns.
#[test]
fn the_returned_frame_is_the_matched_instant_not_the_live_screen() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            // One complete frame, then unbracketed output that lands after
            // the frame was published.
            r"printf '\033[?2026h\033[2J\033[HFRAMED\033[?2026l'; printf '\r\nLIVE'; read guard",
        ])
        .spawn("sh")?;

    let frame = t.wait_frame(|s| s.contains("FRAMED"))?;
    t.wait_until(|s| s.contains("LIVE"))?;

    assert!(
        !frame.contains("LIVE"),
        "the returned frame must be the instant the update ended:\n{frame}"
    );
    assert!(
        t.screen().contains("LIVE"),
        "the live screen has moved on:\n{}",
        t.screen()
    );

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A frame drawn at the old size is not the repaint that answers a
/// resize, which is what makes the advice in `resize`'s stale-frame trap
/// hold for `wait_frame`.
#[test]
fn a_resize_stops_offering_frames_drawn_at_the_old_size() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .size(80, 24)
        .timeout(Duration::from_millis(700))
        .args([
            "-c",
            // Paints one frame, then ignores SIGWINCH and never repaints.
            r"printf '\033[?2026h\033[2J\033[HBEFORE-RESIZE\033[?2026l'; read guard",
        ])
        .spawn("sh")?;

    // Deliberately not consumed: this proves the resize moves the cursor,
    // not that an earlier wait did.
    t.wait_until(|s| s.contains("BEFORE-RESIZE"))?;
    t.resize(40, 10)?;

    let stale = t.wait_frame(|s| s.contains("BEFORE-RESIZE"));
    assert!(
        matches!(stale, Err(Error::Timeout { .. })),
        "a pre-resize frame must not answer a post-resize wait: {stale:?}"
    );
    Ok(())
}

/// `screen()` is the live grid even for an application that brackets
/// every repaint — the tear is real, documented, and wanted for
/// diagnosis. `wait_frame`'s return value is the frame-consistent read.
#[test]
fn a_snapshot_can_be_mid_frame_for_a_synchronized_application() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .size(80, 24)
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                // Frame OPEN, one of two rows painted.
                r"printf '\033[?2026h\033[2J\033[HROW-ONE'; read a; ",
                // Second row, then the frame closes.
                r"printf '\033[2;1HROW-TWO\033[?2026l'; read b"
            ),
        ])
        .spawn("sh")?;

    t.wait_until(|s| s.contains("ROW-ONE"))?;
    let torn = t.screen();
    assert!(torn.contains("ROW-ONE"), "row 0 is painted:\n{torn}");
    assert!(
        torn.row_text(1).trim_end().is_empty(),
        "the frame is not finished, and screen() says so:\n{torn}"
    );

    t.send(Key::Enter)?;
    // The frame-consistent read has both rows, by construction.
    let frame = t.wait_frame(|s| s.contains("ROW-TWO"))?;
    assert!(
        frame.contains("ROW-ONE") && frame.contains("ROW-TWO"),
        "{frame}"
    );

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A terminal that is silent *because* it is stuck mid-repaint used to
/// time out "waiting for 100ms of output silence", which reads as
/// nonsense next to a quiet terminal.
#[test]
fn a_wait_idle_timeout_names_an_unfinished_frame() {
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(600))
        .args(["-c", r"printf '\033[?2026hhalf a frame'; read guard"])
        .spawn("sh")
        .unwrap();
    t.wait_until(|s| s.contains("half a frame")).unwrap();

    let err = t.wait_idle(Duration::from_millis(100)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unfinished DEC 2026 synchronized update"),
        "the real state must be named: {msg}"
    );
    assert!(
        msg.contains("half-painted frame"),
        "and what the embedded screen is: {msg}"
    );
}
