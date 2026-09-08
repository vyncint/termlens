//! `Terminal::record`: every complete frame, in order, with timestamps;
//! bounded, honest about drops, refusing an application without
//! synchronized updates, and exportable as asciicast v2 (#254).

use std::time::Duration;

use termlens::{Key, Terminal};

mod common;

/// Three frames in one write once released, so the recording has an
/// ordered sequence to be about.
const BURST: &str =
    r"\e[?2026h\e[HSTEP 1\e[?2026l\e[?2026h\e[HSTEP 2\e[?2026l\e[?2026h\e[HSTEP 3\e[?2026l";

fn emit(builder: termlens::TerminalBuilder, steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(builder.size(40, 6).timeout(Duration::from_secs(10)), steps)
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY closes a DEC 2026 bracket before the content it wrapped, so a recorded frame never holds what was drawn (#149)"
)]
fn a_recording_holds_every_frame_in_order_with_its_time() -> termlens::Result<()> {
    let mut t = emit(
        Terminal::builder(),
        &["READY", "--wait", "--raw", BURST, " DONE", "--wait"],
    )?;
    t.wait_until(|s| s.contains("READY"))?;
    let rec = t.record();
    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("DONE"))?;
    let frames = rec.stop()?;
    assert_eq!(frames.len(), 3, "three complete frames, none skipped");
    assert_eq!(frames.dropped(), 0);
    let steps: Vec<String> = frames
        .frames()
        .iter()
        .map(|(_, s)| s.row_text(0).trim_end().to_owned())
        .collect();
    assert_eq!(steps, ["STEP 1", "STEP 2", "STEP 3"], "in the order drawn");
    let times: Vec<Duration> = frames.frames().iter().map(|(at, _)| *at).collect();
    assert!(
        times.windows(2).all(|w| w[0] <= w[1]),
        "timestamps never run backwards: {times:?}"
    );
    // Frames before `record()` was called are not in it.
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY closes a DEC 2026 bracket before the content it wrapped, so a recorded frame never holds what was drawn (#149)"
)]
fn the_budget_drops_the_oldest_frames_and_says_so() -> termlens::Result<()> {
    // Two 40x6 frames fit; the third pushes the first out.
    let mut t = emit(
        Terminal::builder().record_budget(40 * 6 * 2),
        &["READY", "--wait", "--raw", BURST, " DONE", "--wait"],
    )?;
    t.wait_until(|s| s.contains("READY"))?;
    let rec = t.record();
    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("DONE"))?;
    let frames = rec.stop()?;
    assert_eq!(frames.len(), 2);
    assert_eq!(frames.dropped(), 1, "the drop is reported, not silent");
    assert!(
        frames.frames()[0].1.contains("STEP 2"),
        "the oldest went first"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn an_application_without_synchronized_updates_is_refused() -> termlens::Result<()> {
    let mut t = emit(Terminal::builder(), &["plain text\nDONE", "--wait"])?;
    let rec = t.record();
    t.wait_until(|s| s.contains("DONE"))?;
    let err = rec.stop().expect_err("nothing bracketed, nothing recorded");
    assert!(matches!(err, termlens::Error::Input(_)), "{err}");
    assert!(
        err.to_string()
            .contains("never emitted a DEC 2026 synchronized update"),
        "the diagnosis wait_frame gives: {err}"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY closes a DEC 2026 bracket before the content it wrapped, so a recorded frame never holds what was drawn (#149)"
)]
fn the_asciicast_is_a_v2_header_and_one_full_repaint_per_frame() -> termlens::Result<()> {
    let mut t = emit(
        Terminal::builder(),
        &["READY", "--wait", "--raw", BURST, " DONE", "--wait"],
    )?;
    t.wait_until(|s| s.contains("READY"))?;
    let rec = t.record();
    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("DONE"))?;
    let frames = rec.stop()?;

    let cast = frames.to_asciicast();
    let mut lines = cast.lines();
    let header = lines.next().expect("a header line");
    assert!(
        header.starts_with("{\"version\": 2, \"width\": 40, \"height\": 6"),
        "{header}"
    );
    let events: Vec<&str> = lines.collect();
    assert_eq!(events.len(), 3, "one event per frame:\n{cast}");
    for (event, step) in events.iter().zip(["STEP 1", "STEP 2", "STEP 3"]) {
        assert!(event.starts_with('[') && event.ends_with(']'), "{event}");
        assert!(
            event.contains(", \"o\", \"\\u001b[H\\u001b[2J"),
            "a full repaint: {event}"
        );
        assert!(event.contains(step), "{event}");
    }

    let path = std::env::temp_dir().join(format!("termlens-record-{}.cast", std::process::id()));
    frames.write_asciicast(&path)?;
    let written = std::fs::read_to_string(&path)?;
    let _ = std::fs::remove_file(&path);
    assert_eq!(written, cast);
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The exported event must *redraw* the frame it came from — every row, the
/// bottom one included (#295). `to_ansi` ends every row with a newline, and
/// the one after the last row scrolled the whole picture up by one when the
/// recording was replayed, so a player showed a screen the test never saw.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY closes a DEC 2026 bracket before the content it wrapped, so a recorded frame never holds what was drawn (#149)"
)]
fn the_asciicast_replays_every_row_of_every_frame() -> termlens::Result<()> {
    // 8x3, and the bottom row is filled edge to edge: a frame whose last row
    // is full is the one a stray linefeed damages most.
    let mut t = common::spawn_emit(
        Terminal::builder()
            .size(8, 3)
            .timeout(Duration::from_secs(10)),
        &[
            "READY",
            "--wait",
            "--raw",
            r"\e[?2026h\e[H\e[2JTOP\e[2;1HMIDDLE\e[3;1HBOTTOMXX\e[?2026l",
            "--wait",
            "--raw",
            r"\e[?2026h\e[H\e[2JSECOND\e[3;1HFILLEDUP\e[?2026l",
            "--wait",
        ],
    )?;
    t.wait_until(|s| s.contains("READY"))?;
    let recorder = t.record();
    t.send(Key::Enter)?;
    t.wait_frame(|s| s.contains("BOTTOMXX"))?;
    t.send(Key::Enter)?;
    t.wait_frame(|s| s.contains("FILLEDUP"))?;
    let recording = recorder.stop()?;
    assert_eq!(recording.len(), 2, "two frames were bracketed");

    let cast = recording.to_asciicast();
    let events: Vec<&str> = cast.lines().skip(1).collect();
    assert_eq!(events.len(), recording.len(), "one event per frame");
    for (event, (_, frame)) in events.iter().zip(recording.frames()) {
        let parsed: serde_json::Value = serde_json::from_str(event).expect("an asciicast event");
        let data = parsed[2].as_str().expect("the output payload");
        // Replay it into a terminal of the recorded size, exactly as a
        // player would, and hold the result against the frame it came from.
        let (cols, rows) = frame.size();
        let mut replay = vt100::Parser::new(rows, cols, 0);
        replay.process(data.as_bytes());
        let played: Vec<String> = replay
            .screen()
            .rows(0, cols)
            .map(|row| row.trim_end().to_owned())
            .collect();
        let recorded: Vec<String> = (0..rows)
            .map(|row| frame.row_text(row).trim_end().to_owned())
            .collect();
        assert_eq!(played, recorded, "the event must redraw its frame");
    }
    Ok(())
}
