//! Typed input beyond plain keys: mouse (mode-aware), and — as the tier
//! progresses — modifier chords, bracketed paste, and cursor-key modes.

use std::time::Duration;

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

    t.send(Key::Esc);
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
        t.send(chord);
        t.wait_frame(|s| s.contains(name))?;
    }

    t.send(Key::Esc);
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn paste_is_one_event_under_bracketed_paste() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    t.wait_frame(|s| s.contains("form-echo ready"))?;

    // One paste event — not eleven key presses — proves the wrapper.
    t.paste("hello world");
    t.wait_frame(|s| s.contains("input: hello world") && s.contains("last: paste:11"))?;

    t.send(Key::Esc);
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
    t.paste("plain");
    t.wait_until(|s| s.contains("got:plain"))?;
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A paste marker inside the text must not end the paste early: the app
/// would see the remainder as ordinary key presses (paste injection).
#[test]
fn an_embedded_paste_marker_cannot_end_the_paste() -> termlens::Result<()> {
    let mut t = spawn_form_echo()?;
    t.wait_frame(|s| s.contains("form-echo ready"))?;

    t.paste("AB\x1b[201~CD");
    // One paste event carrying all four characters — the markers are
    // gone, so nothing arrives as key presses.
    t.wait_frame(|s| s.contains("input: ABCD") && s.contains("last: paste:4"))?;

    t.send(Key::Esc);
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

    t.paste("a\nb");
    t.wait_until(|s| s.contains("WIRE-EOF"))?;
    let row = t.screen().row_text(0);
    assert!(
        row.contains("610d62"),
        "expected 61 0d 62 (a CR b), got: {row}"
    );

    // ICRNL is off, so the guard `read` needs a literal newline — the
    // CR that Key::Enter sends is no longer translated for it.
    t.send_str("\n");
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
                r"head -c 7 | od -An -tx1 | tr -d ' \n'; printf ' WIRE-EOF'; read guard"
            ),
        ])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("READY"))?;

    // Column 100 is 0x85 as a bare byte; UTF-8 must send c2 85.
    t.click(100, 3)?;
    t.send_str("\n\n\n\n\n\n\n");
    t.wait_until(|s| s.contains("WIRE-EOF"))?;

    let wire = t.screen().row_text(0);
    assert!(
        wire.contains("1b5b4d20c28524"),
        "expected ESC [ M 0x20 c2 85 0x24 (UTF-8 column 100), got: {wire}"
    );
    t.send_str("\n");
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
                r"stty -icanon -echo; printf '\033[?1003h\033[?1006h'; printf READY; ",
                // 78 bytes: 20 (right press+release) + 20 (ctrl+left)
                // + 10 (wheel left) + 28 (drag press+motion+release).
                r#"wire=$(head -c 78 | tr '\033' 'E'); printf '|%s|' "$wire"; read guard"#
            ),
        ])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("READY"))?;

    // Right-click: SGR button 2. Ctrl-click: 0 + 16. Wheel left: 66.
    t.click_with(termlens::MouseButton::Right, 10, 4)?;
    t.click_with(termlens::MouseButton::Left.ctrl(), 3, 1)?;
    t.scroll(5, 5, termlens::Scroll::Left)?;
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
        "[<0;3;3M",
        "[<32;7;4M",
        "[<0;7;4m", // drag: press, motion, release
    ] {
        assert!(text.contains(expected), "missing {expected} in:\n{text}");
    }
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

    t.send(Key::Enter);
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
    t.send(Key::Up);
    t.wait_until(|s| s.contains("got:EOA"))?;

    t.wait_until(|s| s.contains("NORM"))?;
    t.send(Key::Up);
    t.wait_until(|s| s.contains("got:E[A"))?;

    t.send(Key::Enter);
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

    t.send(Key::Char('q'));
    assert!(t.wait_exit().unwrap().success());
}
