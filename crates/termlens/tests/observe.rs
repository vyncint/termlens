//! Cumulative observations on a snapshot: repaints, bells, and inline
//! graphics payloads. All three are counters an assertion can take a delta
//! around, and all three describe behaviour that leaves the screen
//! unchanged — which is why they were previously untestable.

use std::time::Duration;

use termlens::{Key, Terminal};

mod common;

/// The `emit` fixture on a 40x6 terminal; steps are documented in
/// `fixtures/emit/src/main.rs`.
fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(
        Terminal::builder()
            .size(40, 6)
            .timeout(Duration::from_secs(10)),
        steps,
    )
}

/// The amplification assertion: one input must not become N repaints. No
/// content predicate can see this, because every intermediate frame shows
/// correct content.
#[test]
fn repaints_count_completed_updates_not_changes() -> termlens::Result<()> {
    let mut t = emit(&[
        "READY",
        "--wait",
        // Three complete repaints, the middle one changing nothing at all.
        "--raw",
        r"\e[?2026hone\e[?2026l",
        "--raw",
        r"\e[?2026h\e[?2026l",
        "--raw",
        r"\e[?2026htwo\e[?2026l",
        " DONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("READY"))?;
    assert_eq!(t.screen().repaints(), 0, "nothing has repainted yet");

    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("DONE"))?;
    assert_eq!(
        t.screen().repaints(),
        3,
        "a Begin/End pair that drew nothing is still a repaint"
    );

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// An application that never brackets a repaint has nothing to count, and
/// says zero rather than guessing from its redraws.
#[test]
fn an_app_without_synchronized_output_reports_no_repaints() -> termlens::Result<()> {
    let mut t = emit(&["drew\n", "drew again\n", "DONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    assert_eq!(t.screen().repaints(), 0);
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// "Pressing an invalid key does nothing visible" and "pressing an invalid
/// key is refused with a bell" are different behaviours and the same screen.
#[test]
fn a_bell_is_observable_and_a_title_terminator_is_not_a_bell() -> termlens::Result<()> {
    let mut t = emit(&[
        "READY",
        "--wait",
        "--raw",
        r"\a",
        "--raw",
        r"\e]0;set by osc\a",
        "--raw",
        r"\a",
        " DONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("READY"))?;
    let before = t.screen().bells();
    assert_eq!(before, 0);

    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.bells() - before, 2, "two bells, not three: {s}");
    // The OSC still did its job, which is what would break if the BEL were
    // counted before the state machine saw it.
    assert_eq!(s.title(), "set by osc");
    // And nothing about the bells reached the grid, which is the point: the
    // rows hold the marker and the echoed newline, and not one cell more.
    assert_eq!(s.text().trim_end(), "READY\n DONE", "{s}");

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The negative assertion is the one that catches a regression: this content
/// must render as text in every terminal, so it must never go out as an
/// image.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY does not forward kitty or sixel payloads (#149)"
)]
fn graphics_payloads_are_observable_and_absence_is_assertable() -> termlens::Result<()> {
    let mut plain = emit(&["box art: +--+", " DONE", "--wait"])?;
    plain.wait_until(|s| s.contains("DONE"))?;
    assert!(
        plain.screen().graphics().is_empty(),
        "nothing was transmitted as an image"
    );
    plain.send(Key::Enter)?;
    assert!(plain.wait_exit()?.success());

    let mut drawing = emit(&[
        "text",
        "--raw",
        r"\e_Gf=24,s=1,v=1,a=T;QUJDREVG\e\\",
        "--raw",
        r"\ePq#0;2;0;0;0#0~~-~~\e\\",
        " DONE",
        "--wait",
    ])?;
    drawing.wait_until(|s| s.contains("DONE"))?;
    let g = drawing.screen().graphics();
    assert_eq!(g.kitty(), 1, "one kitty payload");
    assert_eq!(g.sixel(), 1, "one sixel payload");
    assert_eq!(g.total(), 2);
    assert!(!g.is_empty());
    assert!(g.bytes() > 20, "payload size is reported: {}", g.bytes());
    // A payload is swallowed, not drawn: the grid holds only the text.
    assert!(
        drawing.screen().contains("text DONE"),
        "{}",
        drawing.screen()
    );

    drawing.send(Key::Enter)?;
    assert!(drawing.wait_exit()?.success());
    Ok(())
}

/// The kitty graphics query used to escape the timeout note entirely: no
/// answer *and* no diagnosis, alone among the startup probes.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY does not forward kitty or sixel payloads (#149)"
)]
fn a_blocked_kitty_graphics_query_is_named_in_the_timeout() -> termlens::Result<()> {
    let mut t = common::spawn_emit(
        Terminal::builder()
            .size(40, 4)
            .timeout(Duration::from_millis(700)),
        &["--raw", r"\e_Gi=1,a=q;\e\\", "MARK", "--wait"],
    )?;
    let err = t
        .wait_until(|s| s.contains("NEVER-APPEARS"))
        .expect_err("must time out");
    let msg = err.to_string();
    assert!(
        msg.contains("queried the terminal"),
        "the probe must be diagnosed: {msg}"
    );
    assert!(msg.contains("_G"), "and named: {msg}");
    t.send(Key::Enter)?;
    Ok(())
}

/// A transmission is an instruction, not a question, so it must not put a
/// query diagnosis into an unrelated timeout of an application that draws.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY does not forward kitty or sixel payloads (#149)"
)]
fn a_kitty_transmission_does_not_pollute_the_timeout() -> termlens::Result<()> {
    let mut t = common::spawn_emit(
        Terminal::builder()
            .size(40, 4)
            .timeout(Duration::from_millis(700)),
        &["--raw", r"\e_Gf=24,a=T;QUJD\e\\", "MARK", "--wait"],
    )?;
    let err = t
        .wait_until(|s| s.contains("NEVER-APPEARS"))
        .expect_err("must time out");
    assert!(
        !err.to_string().contains("queried the terminal"),
        "a transmit is not a query: {err}"
    );
    assert_eq!(t.screen().graphics().kitty(), 1, "but it is counted");
    t.send(Key::Enter)?;
    Ok(())
}

/// The frame `wait_frame` returns must carry the repaint count too — it is
/// the screen a test is most likely to ask, since asserting on the returned
/// frame rather than a later `screen()` is the documented idiom. Retained
/// frames used to be built straight from the emulator, which skipped the
/// count and left it at zero.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY closes a DEC 2026 bracket before the content it wrapped, so no frame holds what was drawn (#149)"
)]
fn a_frame_from_wait_frame_carries_the_repaint_count() -> termlens::Result<()> {
    let mut t = emit(&[
        "READY",
        "--wait",
        "--raw",
        r"\e[?2026hone\e[?2026l",
        "--raw",
        r"\e[?2026htwo\e[?2026l",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("READY"))?;
    t.send(Key::Enter)?;

    let first = t.wait_frame(|s| s.contains("one"))?;
    assert_eq!(first.repaints(), 1, "the first frame is repaint 1: {first}");
    let second = t.wait_frame(|s| s.contains("two"))?;
    assert_eq!(second.repaints(), 2, "and the second is 2: {second}");
    // And the live grid agrees with the frame it last handed out.
    assert_eq!(t.screen().repaints(), 2);

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
