//! Typed input beyond plain keys: mouse (mode-aware), modifier chords,
//! bracketed paste, and cursor-key modes.

use std::time::{Duration, Instant};

use termlens::{Error, Key, Scroll, Terminal};

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
fn clicks_round_trip_through_the_apps_tracking_mode() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    t.wait_frame(|s| s.contains("form-echo ready"))?;

    // crossterm enables SGR tracking; the click must arrive as one press
    // and one release at the exact cell.
    t.click(10, 5)?;
    t.wait_frame(|s| s.contains("last: mouse:up:10,5"))?;

    t.scroll(3, 2, Scroll::Up)?;
    t.wait_frame(|s| s.contains("last: mouse:scrollup:3,2"))?;
    t.scroll(3, 2, Scroll::Down)?;
    t.wait_frame(|s| s.contains("last: mouse:scrolldown:3,2"))?;

    // Modified wheel notches, decoded by crossterm's own parser: the zoom
    // and horizontal-scroll idioms, which had no way onto the wire before.
    t.scroll_with(Scroll::Up.ctrl(), 3, 2)?;
    t.wait_frame(|s| s.contains("last: mouse:ctrl+scrollup:3,2"))?;
    t.scroll_with(Scroll::Down.shift(), 4, 1)?;
    t.wait_frame(|s| s.contains("last: mouse:shift+scrolldown:4,1"))?;
    t.scroll_with(Scroll::Up.ctrl().alt(), 3, 2)?;
    t.wait_frame(|s| s.contains("last: mouse:ctrl+alt+scrollup:3,2"))?;

    t.send(Key::Esc)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn modifier_chords_round_trip_through_crossterms_parser() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    t.wait_frame(|s| s.contains("form-echo ready"))?;

    for (chord, name) in [
        (Key::Right.ctrl(), "last: ctrl+right"),
        (Key::Up.shift(), "last: shift+up"),
        (Key::PageDown.alt(), "last: alt+pagedown"),
        (Key::Home.ctrl(), "last: ctrl+home"),
        (Key::F(5).ctrl().shift(), "last: ctrl+shift+f:5"),
        (Key::Delete.alt(), "last: alt+delete"),
    ] {
        t.send(chord)?;
        t.wait_frame(|s| s.contains(name))?;
    }

    t.send(Key::Esc)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn paste_is_one_event_under_bracketed_paste() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    t.wait_frame(|s| s.contains("form-echo ready"))?;

    // One paste event — not eleven key presses — proves the wrapper.
    t.paste("hello world")?;
    t.wait_frame(|s| s.contains("input: hello world") && s.contains("last: paste:11"))?;

    t.send(Key::Esc)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn paste_falls_back_to_plain_bytes_without_the_mode() -> termlens::Result<()> {
    // A plain shell never enables mode 2004; the paste must arrive as
    // raw bytes with no ESC[200~ wrapper.
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                r"stty -icanon -echo; ",
                r#"reply=$(head -c 5); printf 'got:%s' "$reply"; read guard"#
            ),
        ])
        .spawn("/bin/sh")?;
    t.paste("plain")?;
    t.wait_until(|s| s.contains("got:plain"))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A paste marker inside the text must not end the paste early: the app
/// would see the remainder as ordinary key presses (paste injection).
#[test]
fn an_embedded_paste_marker_cannot_end_the_paste() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    t.wait_frame(|s| s.contains("form-echo ready"))?;

    t.paste("AB\x1b[201~CD")?;
    // One paste event carrying all four characters — the markers are
    // gone, so nothing arrives as key presses.
    t.wait_frame(|s| s.contains("input: ABCD") && s.contains("last: paste:4"))?;

    t.send(Key::Esc)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Pasted line breaks reach the application as CR, the byte the Enter
/// key produces — every real terminal converts, and raw mode (which
/// clears ICRNL) means nothing downstream will.
#[test]
fn a_pasted_line_break_arrives_as_carriage_return() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                // -icrnl is what raw mode does (crossterm's
                // enable_raw_mode clears it): without it the line
                // discipline rewrites our CR back to LF before the app
                // ever sees it. READY marks the settings as applied —
                // pasting earlier would race them.
                r"stty -icanon -echo -icrnl; printf READY; ",
                // Render the three bytes as hex so the wire is visible.
                r"head -c 3 | od -An -tx1 | tr -d ' \n'; printf ' WIRE-EOF'; read guard"
            ),
        ])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("READY"))?;

    t.paste("a\nb")?;
    t.wait_until(|s| s.contains("WIRE-EOF"))?;
    let row = t.screen().row_text(0);
    assert!(
        row.contains("610d62"),
        "expected 61 0d 62 (a CR b), got: {row}"
    );

    // ICRNL is off, so the guard `read` needs a literal newline — the
    // CR that Key::Enter sends is no longer translated for it.
    t.send_str("\n")?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// An application that selected the UTF-8 mouse encoding (mode 1005)
/// must receive coordinates it can decode. The legacy form writes a bare
/// byte above 127, which is not valid UTF-8 on its own — and because the
/// two encodings agree below column 95, sending the wrong one fails only
/// past a position boundary.
#[test]
fn mouse_reports_follow_the_utf8_encoding() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .size(120, 24)
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                r"stty -icanon -echo; printf '\033[?1000h\033[?1005h'; printf READY; ",
                r"head -c 7 | od -An -tx1 | tr -d ' \n'; printf ' WIRE-EOF'; ",
                // A loop with a sentinel, not a bare `read guard`: the
                // padding below is a separate write from the click, so
                // whether head's buffered read swallows it or leaves it
                // queued is a race. A bare guard loses that race — it
                // consumes a padding newline, the shell exits, and the
                // final write below hits a dead PTY with EIO. Only QUIT
                // ends this script, so the padding cannot end it early.
                r#"while read guard; do [ "$guard" = QUIT ] && exit 0; done"#
            ),
        ])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("READY"))?;

    // Column 100 is 0x85 as a bare byte; UTF-8 must send c2 85.
    t.click(100, 3)?;
    // Padding, so a regression to the 6-byte legacy form unblocks `head`
    // and shows the wire instead of timing out. Seven of them because a
    // correct click already supplied all seven bytes; these are only ever
    // read when it did not.
    t.send_str("\n\n\n\n\n\n\n")?;
    t.wait_until(|s| s.contains("WIRE-EOF"))?;

    let wire = t.screen().row_text(0);
    assert!(
        wire.contains("1b5b4d20c28524"),
        "expected ESC [ M 0x20 c2 85 0x24 (UTF-8 column 100), got: {wire}"
    );
    t.send_str("QUIT\n")?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Everything the mouse API can express, captured off the wire under
/// SGR encoding with full (any-event) tracking.
#[test]
fn buttons_modifiers_drag_and_horizontal_wheel_reach_the_wire() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        // Wide enough that the captured wire stays on one row.
        .size(200, 24)
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                // `min 0 time 20` rather than an exact byte count: the drag
                // reports one motion per cell crossed, so the length depends
                // on the path.
                r"stty -icanon -echo min 0 time 20; printf '\033[?1003h\033[?1006h'; printf READY; ",
                r#"wire=$(dd bs=1 count=200 2>/dev/null | tr '\033' 'E'); printf '|%s|' "$wire"; read guard"#
            ),
        ])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("READY"))?;

    // Right-click: SGR button 2. Ctrl-click: 0 + 16. Wheel left: 66.
    t.click_with(termlens::MouseButton::Right, 10, 4)?;
    t.click_with(termlens::MouseButton::Left.ctrl(), 3, 1)?;
    t.scroll(5, 5, termlens::Scroll::Left)?;
    // Ctrl-wheel-up (64 + 16) and Shift-wheel-down (65 + 4): the modifiers
    // ride on the wheel's button code exactly as they do on a click.
    t.scroll_with(termlens::Scroll::Up.ctrl(), 5, 5)?;
    t.scroll_with(termlens::Scroll::Down.shift(), 5, 5)?;
    // Drag left button from (2,2) to (6,3): press, motion (0+32), release.
    t.drag(termlens::MouseButton::Left, (2, 2), (6, 3))?;

    t.wait_until(|s| s.row_text(0).contains("|"))?;
    let text = t.screen().row_text(0);
    for expected in [
        "[<2;11;5M",
        "[<2;11;5m", // right press + release
        "[<16;4;2M",
        "[<16;4;2m", // ctrl + left
        "[<66;6;6M", // horizontal wheel
        "[<80;6;6M", // ctrl + wheel up
        "[<69;6;6M", // shift + wheel down
        // Drag (2,2) -> (6,3): press, one motion per crossed cell, release.
        "[<0;3;3M",
        "[<32;4;3M",
        "[<32;5;4M",
        "[<32;6;4M",
        "[<32;7;4M",
        "[<0;7;4m",
    ] {
        assert!(text.contains(expected), "missing {expected} in:\n{text}");
    }
    Ok(())
}

/// A drag used to teleport: one motion report at the destination, however
/// many cells it crossed. Invisible to an application that only asks "where
/// did it start, where is it now", and wrong for every application that does
/// something *along* the path.
#[test]
fn a_drag_reports_one_motion_per_cell_crossed() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .size(200, 24)
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                r"stty -icanon -echo min 0 time 20; printf '\033[?1002h\033[?1006h'; printf READY; ",
                r#"wire=$(dd bs=1 count=300 2>/dev/null | tr '\033' 'E'); printf '|%s|' "$wire"; read guard"#
            ),
        ])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("READY"))?;

    // Seven cells crossed, from column 5 to column 12 on row 4.
    t.drag(termlens::MouseButton::Left, (5, 4), (12, 4))?;
    t.wait_until(|s| s.row_text(0).contains("|"))?;
    let text = t.screen().row_text(0);

    // One motion at each intervening column, 1-based on the wire.
    for col in 7..=13 {
        assert!(
            text.contains(&format!("[<32;{col};5M")),
            "missing motion at column {col} in:\n{text}"
        );
    }
    assert_eq!(
        text.matches("[<32;").count(),
        7,
        "seven cells crossed, seven motion reports:\n{text}"
    );
    // The press and release still bracket it, at the endpoints.
    assert!(text.contains("[<0;6;5M"), "{text}");
    assert!(text.contains("[<0;13;5m"), "{text}");
    Ok(())
}

/// The mode-aware refusals are untouched: under plain `?1000` the
/// application asked not to hear about motion, so it hears none.
#[test]
fn press_release_tracking_still_gets_no_motion_at_all() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .size(200, 24)
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                r"stty -icanon -echo min 0 time 20; printf '\033[?1000h\033[?1006h'; printf READY; ",
                r#"wire=$(dd bs=1 count=100 2>/dev/null | tr '\033' 'E'); printf '|%s|' "$wire"; read guard"#
            ),
        ])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("READY"))?;
    t.drag(termlens::MouseButton::Left, (5, 4), (12, 4))?;
    t.wait_until(|s| s.row_text(0).contains("|"))?;
    let text = t.screen().row_text(0);
    assert!(!text.contains("[<32;"), "no motion under ?1000:\n{text}");
    assert!(
        text.contains("[<0;6;5M") && text.contains("[<0;13;5m"),
        "{text}"
    );
    Ok(())
}

/// A drag is refused under X10, where there is no release to report —
/// the same standard `click` applies to "no tracking at all".
#[test]
fn drag_is_refused_when_the_mode_cannot_express_it() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args(["-c", r"printf '\033[?9h'; printf READY; read guard"])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("READY"))?;

    let err = t
        .drag(termlens::MouseButton::Left, (1, 1), (4, 4))
        .expect_err("X10 reports presses only");
    assert!(matches!(err, Error::Input(_)), "got: {err}");
    assert!(err.to_string().contains("X10"), "unhelpful: {err}");

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn arrows_follow_the_apps_cursor_key_mode() -> termlens::Result<()> {
    // The script enables DECCKM (CSI ?1 h), reads 3 bytes, reports them,
    // then disables it and reads again — one terminal, both modes.
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                r"stty -icanon -echo; printf '[?1hAPP '; ",
                r#"a=$(head -c 3 | tr '' 'E'); printf 'got:%s ' "$a"; "#,
                r"printf '[?1lNORM '; ",
                r#"b=$(head -c 3 | tr '' 'E'); printf 'got:%s' "$b"; read guard"#
            ),
        ])
        .spawn("/bin/sh")?;

    t.wait_until(|s| s.contains("APP"))?;
    t.send(Key::Up)?;
    t.wait_until(|s| s.contains("got:EOA"))?;

    t.wait_until(|s| s.contains("NORM"))?;
    t.send(Key::Up)?;
    t.wait_until(|s| s.contains("got:E[A"))?;

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn clicking_without_mouse_tracking_is_a_typed_error() {
    // hello-tui never enables mouse tracking.
    let mut t = Terminal::builder()
        .size(80, 24)
        .timeout(Duration::from_secs(10))
        .env_clear()
        .spawn(util::fixture_bin("hello-tui"))
        .unwrap();
    t.wait_until(|s| s.contains("╯")).unwrap();

    let err = t.click(1, 1).unwrap_err();
    assert!(matches!(err, Error::Input(_)), "got: {err}");
    assert!(err.to_string().contains("mouse tracking"), "{err}");

    t.send(Key::Char('q')).unwrap();
    assert!(t.wait_exit().unwrap().success());
}

/// A real terminal cannot report a click outside its window. Off-grid
/// coordinates used to encode and send anyway (and at `u16::MAX` the SGR
/// path wrapped or panicked). Refuse with the position and the grid size
/// at the time of the call — including after a shrink `resize`.
#[test]
fn mouse_events_outside_the_grid_are_refused() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .size(20, 5)
        .timeout(Duration::from_secs(10))
        .env_clear()
        .spawn(util::fixture_bin("form-echo"))?;
    t.wait_frame(|s| s.contains("form-echo ready"))?;

    // Bottom-right cell is in bounds; one past each edge is not.
    t.click(19, 4)?;
    t.wait_frame(|s| s.contains("last: mouse:up:19,4"))?;

    for (col, row) in [(20u16, 0), (0, 5), (20, 5), (9999, 9999)] {
        let err = t.click(col, row).expect_err("off-grid click");
        assert!(matches!(err, Error::Input(_)), "got: {err}");
        let msg = err.to_string();
        assert!(msg.contains(&format!("({col}, {row})")), "{msg}");
        assert!(msg.contains("20x5"), "size at call time: {msg}");
    }

    let err = t.scroll(20, 0, Scroll::Up).expect_err("off-grid scroll");
    assert!(matches!(err, Error::Input(_)), "got: {err}");
    assert!(err.to_string().contains("20x5"), "{err}");

    let err = t
        .drag(termlens::MouseButton::Left, (0, 0), (20, 0))
        .expect_err("off-grid drag endpoint");
    assert!(matches!(err, Error::Input(_)), "got: {err}");
    assert!(err.to_string().contains("(20, 0)"), "{err}");
    assert!(err.to_string().contains("20x5"), "{err}");

    // After a shrink, a coordinate that was valid earlier must name the
    // *new* size — otherwise the error reads as a mystery. Wide enough for
    // the fixture's acknowledgement to fit on its row, and still excluding
    // (19, 4) on both axes.
    t.resize(18, 4)?;
    // Wait for the application to have handled the SIGWINCH before sending
    // it anything: a keystroke arriving in the same instant as the resize
    // can be lost by crossterm's event reader — see `Terminal::resize`. The
    // stress workflow found this at 8 threads on its first iteration, and it
    // reproduced locally about one run in forty. `wait_frame` is the right
    // wait: a resize advances the frame cursor, so only a frame drawn after
    // it can satisfy this.
    t.wait_frame(|s| s.contains("last: resize:18x4"))?;
    let err = t.click(19, 4).expect_err("pre-resize coordinate");
    assert!(matches!(err, Error::Input(_)), "got: {err}");
    let msg = err.to_string();
    assert!(msg.contains("(19, 4)"), "{msg}");
    assert!(msg.contains("18x4"), "post-resize size: {msg}");
    assert!(!msg.contains("20x5"), "must not report the old size: {msg}");

    t.send(Key::Esc)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Both focus states, through crossterm's own event stream. Before this the
/// unfocused branch of a UI was not merely unasserted, it was **unreachable**
/// — no input existed that could enter it, so the code never ran.
#[test]
fn focus_events_reach_the_application_in_both_directions() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    // Assert on the frame the wait returned rather than waiting again: the
    // application has not repainted since, and a frame satisfies exactly one
    // wait.
    let ready = t.wait_frame(|s| s.contains("form-echo ready"))?;
    // The application asked for focus reporting, and a test can see that it
    // did before trying to deliver one.
    assert!(ready.focus_events(), "form-echo enables mode 1004");
    // A window starts focused, which is why the other branch needed an event.
    assert!(ready.contains("window: focused"), "{ready}");

    t.focus_out()?;
    t.wait_frame(|s| s.contains("window: unfocused") && s.contains("last: focus:lost"))?;

    t.focus_in()?;
    t.wait_frame(|s| s.contains("window: focused") && s.contains("last: focus:gained"))?;

    t.send(Key::Esc)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Focus events are refused when the application never asked for them, the
/// same contract `click` has for mouse tracking — feeding an application
/// events it did not request is not what a terminal does, and the bytes
/// would be misparsed as keys.
#[test]
fn focus_events_are_refused_without_mode_1004() -> termlens::Result<()> {
    // hello-tui enables the alternate screen and nothing else.
    let mut t = Terminal::builder()
        .size(80, 24)
        .timeout(Duration::from_secs(10))
        .env_clear()
        .spawn(util::fixture_bin("hello-tui"))?;
    t.wait_until(|s| s.contains("╯"))?;
    assert!(!t.screen().focus_events());

    for err in [t.focus_in().unwrap_err(), t.focus_out().unwrap_err()] {
        assert!(matches!(err, Error::Input(_)), "got: {err}");
        assert!(err.to_string().contains("focus reporting"), "{err}");
        assert!(err.to_string().contains("1004"), "{err}");
    }

    t.send(Key::Char('q'))?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Typed input to a child that is gone is an error the test can handle,
/// not a panic and not silence. `write_or_panic` used to make this the one
/// failure that could only reach a test by aborting it.
#[test]
fn typed_input_to_a_departed_child_is_a_typed_error() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args(["-c", "printf bye"])
        .spawn("/bin/sh")?;
    assert!(t.wait_exit()?.success());

    let err = t.send(Key::Enter).unwrap_err();
    assert!(matches!(err, Error::Write { .. }), "got: {err}");
    // The screen travels with it, like a timeout's.
    assert!(err.screen().is_some_and(|s| s.contains("bye")), "{err}");
    assert!(err.to_string().contains("Enter"), "{err}");

    // Same contract for the other two.
    assert!(matches!(t.send_str("x"), Err(Error::Write { .. })));
    assert!(matches!(t.paste("x"), Err(Error::Write { .. })));
    Ok(())
}

/// A mouse click at a departed child names the child, not the tracking
/// mode. The old order asked "did it enable mouse tracking?" first, which
/// a child that has exited necessarily has not — so the answer was always
/// technically true and never the reason.
#[test]
fn a_mouse_click_at_a_departed_child_blames_the_child() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args(["-c", "printf bye"])
        .spawn("/bin/sh")?;
    assert!(t.wait_exit()?.success());

    for err in [
        t.click(1, 1).unwrap_err(),
        t.scroll(1, 1, Scroll::Up).unwrap_err(),
        t.drag(termlens::MouseButton::Left, (1, 1), (2, 2))
            .unwrap_err(),
    ] {
        assert!(matches!(err, Error::Write { .. }), "got: {err}");
        assert!(
            !err.to_string().contains("mouse tracking"),
            "must not blame the tracking mode: {err}"
        );
        assert!(err.to_string().contains("the child is gone"), "{err}");
    }
    Ok(())
}

/// Refusing the write is ours, not the OS's, and that is the point: a raw
/// write to a closed terminal fails with EIO on macOS and *succeeds* on
/// Linux. This test failed on Linux and passed on macOS before `send`
/// gained the liveness check, which is exactly the split it exists to
/// close — a keystroke silently swallowed on one runner and reported on the
/// other.
#[test]
fn a_departed_child_is_refused_identically_on_every_platform() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args(["-c", "printf bye"])
        .spawn("/bin/sh")?;
    assert!(t.wait_exit()?.success());
    // Not "the write failed" — the terminal is closed, so there is nothing
    // to write to, and that verdict is reached before any syscall.
    let err = t.send_str("swallowed?").unwrap_err();
    assert!(
        err.to_string().contains("the terminal is closed"),
        "the refusal must name the closed terminal, not an OS errno: {err}"
    );
    Ok(())
}

/// The other half of the contract: a child that is *alive* but has not read
/// yet is not a failure. The bytes sit in the terminal's input queue
/// exactly as they would on a real terminal, and the write succeeded.
#[test]
fn typed_input_to_a_live_child_that_has_not_read_yet_succeeds() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            "printf READY; sleep 0.4; read line; printf ' got:%s' \"$line\"",
        ])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("READY"))?;

    // Sent while the child is sleeping, well before its `read`.
    t.send_str("pending\n")?;
    t.wait_until(|s| s.contains("got:pending"))?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// `send_after` delays, then sends — and the delay is real, which is the
/// whole mechanism: it exists to put this write and the previous one in
/// separate reads.
#[test]
fn send_after_delays_then_delivers() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    t.wait_frame(|s| s.contains("form-echo ready"))?;

    let at = Instant::now();
    t.send_after(Duration::from_millis(120), Key::Char('z'))?;
    let waited = at.elapsed();
    assert!(
        waited >= Duration::from_millis(120),
        "the delay must actually happen: {waited:?}"
    );
    t.wait_frame(|s| s.contains("input: z"))?;

    t.send(Key::Esc)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The remedy, on the case it was written for. With nothing following in the
/// same read, the `Esc` stands alone and is decoded as an `Esc` rather than
/// half an `Alt` chord — form-echo quits on it, which makes that
/// unmistakable. The follow-up key then has nowhere to go, and saying so is
/// exactly what a separated write means for a fixture that exits on `Esc`.
///
/// The byte-level identity behind the hazard is pinned deterministically in
/// `keys.rs`; the merge itself is a race, so nothing here asserts on it.
#[test]
fn a_separated_esc_is_decoded_as_an_esc() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    t.wait_frame(|s| s.contains("form-echo ready"))?;

    t.send(Key::Esc)?;
    assert!(
        t.wait_exit()?.success(),
        "an Esc with nothing behind it is an Esc"
    );

    // And the key that would have merged with it now reports that it could
    // not be delivered, rather than vanishing.
    assert!(
        matches!(
            t.send_after(Duration::from_millis(10), Key::Char('j')),
            Err(Error::Write { .. })
        ),
        "the follow-up key must not vanish silently"
    );
    Ok(())
}
