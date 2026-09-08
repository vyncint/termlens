//! Out-of-band terminal state on `Screen`: title, alternate screen, the
//! input modes and the `OSC 52` clipboard — asserted directly instead of
//! inferred from the grid.

use std::time::Duration;

use termlens::{CursorShape, Key, MouseMode, MouseModes, Screen, Terminal};

mod common;

/// The `emit` fixture; steps are documented in `fixtures/emit/src/main.rs`.
fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(Terminal::builder().timeout(Duration::from_secs(10)), steps)
}

/// One program walks the whole state surface: set everything, assert, then
/// unwind everything and assert the way back.
#[test]
fn screen_reports_title_alternate_screen_and_input_modes() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e]0;termlens state\x07",
        "--raw",
        r"\e[?1049h\e[?2004h\e[?1h\e[?1002h",
        "modes: on",
        "--wait",
        "--raw",
        r"\e]2;phase two\e\\",
        "--raw",
        r"\e[?1002l\e[?1l\e[?2004l\e[?1049l",
        "modes: off",
        "--wait",
    ])?;

    // State assertions are ordinary predicates — waitable like any text.
    t.wait_until(|s| {
        s.contains("modes: on")
            && s.title() == "termlens state"
            && s.alternate_screen()
            && s.bracketed_paste()
            && s.application_cursor()
            && s.mouse_mode() == MouseMode::ButtonMotion
    })?;

    t.send(Key::Enter)?;
    t.wait_until(|s| {
        s.contains("modes: off")
            && s.title() == "phase two" // OSC 2, ST-terminated
            && !s.alternate_screen()
            && !s.bracketed_paste()
            && !s.application_cursor()
            && s.mouse_mode() == MouseMode::None
    })?;

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The tracking mode the app enabled is reported by name, not collapsed.
/// `DECSCUSR` leaves the grid identical, so without an accessor a screen
/// where the application asked for a bar and one where it never asked are
/// the same `Screen`. The program walks all three states an editor moves
/// through: never asked, switched, switched back.
#[test]
fn screen_reports_the_cursor_shape_the_application_asked_for() -> termlens::Result<()> {
    let mut t = emit(&[
        "ready", "--wait", // DECSCUSR 5: a blinking bar, the insert-mode cursor.
        "--csi", "5 q", " insert", "--wait",
        // DECSCUSR 2: a steady block, the way back.
        "--csi", "2 q", " normal", "--wait",
    ])?;

    // Never asked. Distinct from a block, which is what most terminals
    // happen to draw by default — the point is that the program did not say.
    t.wait_until(|s| s.contains("ready"))?;
    let before = t.screen();
    assert_eq!(before.cursor_shape(), CursorShape::Default);
    assert_eq!(before.cursor_blink(), None);

    t.send(Key::Enter)?;
    t.wait_until(|s| {
        s.contains("insert")
            && s.cursor_shape() == CursorShape::Bar
            && s.cursor_blink() == Some(true)
    })?;

    // The restore, which is the half that ships broken.
    t.send(Key::Enter)?;
    t.wait_until(|s| {
        s.contains("normal")
            && s.cursor_shape() == CursorShape::Block
            && s.cursor_blink() == Some(false)
    })?;

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());

    // After exit is where this matters: a program that switches the cursor
    // and never switches back leaves the user's terminal wrong, and the
    // final screen is the evidence.
    let after = t.screen();
    assert_eq!(after.cursor_shape(), CursorShape::Block);
    assert_eq!(after.cursor_blink(), Some(false));
    Ok(())
}

/// The failure the issue is built on, stated as the comparison it is: two
/// applications whose grids are byte-identical, one of which linked and one
/// of which did not. Before `Screen::links` no assertion could tell them
/// apart, so a test for "it linked the docs" passed against the one that
/// emitted nothing.
#[test]
fn an_osc8_hyperlink_is_observable_and_a_missing_one_is_not() -> termlens::Result<()> {
    fn run(steps: &[&str]) -> termlens::Result<Screen> {
        let mut t = emit(steps)?;
        t.wait_until(|s| s.contains("see docs here"))?;
        let screen = t.screen();
        assert!(t.wait_exit()?.success());
        Ok(screen)
    }

    let linked = run(&[
        "--raw",
        r"see \e]8;;https://example.invalid/a\e\\docs\e]8;;\e\\ here\n",
    ])?;
    let plain = run(&["see docs here\n"])?;

    // The grids agree exactly — this was never a rendering bug, and the URL
    // must not leak into the cells.
    assert_eq!(linked.text(), plain.text());
    assert_eq!(linked.row_text(0).trim_end(), "see docs here");
    assert!(!linked.text().contains("example.invalid"));

    // And now they are distinguishable.
    assert!(plain.links().is_empty());
    let link = &linked.links()[0];
    assert_eq!(linked.links().len(), 1);
    assert_eq!(link.uri(), "https://example.invalid/a");
    assert_eq!(link.label(), Some("docs"));
    assert!(link.closed());
    assert_eq!(link.id(), None);

    // A wrong target fails a test the right one passes, which is the whole
    // point of capturing it.
    assert_ne!(link.uri(), "https://example.invalid/b");
    Ok(())
}

/// A `Screen` is an immutable snapshot, and the link log is the first piece
/// of out-of-band state that is *mutated in place* after it is recorded — a
/// span is pushed when it opens and completed when it closes. So the
/// copy-on-write has to hold: a snapshot taken mid-span must go on reporting
/// the span as open, with no label, however the stream continues.
///
/// The emulator relies on this for the graphics log too, where it is only
/// asserted in a comment.
#[test]
fn a_snapshot_keeps_its_own_view_of_the_links() -> termlens::Result<()> {
    let mut t = emit(&[
        // Open a span and leave it open across the pause.
        "--raw",
        r"\e]8;;http://a/\e\\LABEL one\n",
        "--wait",
        // Close it, then open a second one.
        "--raw",
        r"\e]8;;\e\\\e]8;;http://b/\e\\X two\n",
        "--wait",
    ])?;

    t.wait_until(|s| s.contains("one"))?;
    let early = t.screen();
    assert_eq!(early.links().len(), 1);
    assert!(
        !early.links()[0].closed(),
        "the span is open at this instant"
    );
    assert_eq!(early.links()[0].label(), None, "and has no final label yet");

    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("two") && s.links().len() == 2)?;
    let later = t.screen();
    assert!(later.links()[0].closed());
    assert_eq!(later.links()[0].uri(), "http://a/");
    assert_eq!(later.links()[1].uri(), "http://b/");

    // The whole point: the earlier snapshot did not move.
    assert_eq!(early.links().len(), 1, "an earlier snapshot grew a link");
    assert!(
        !early.links()[0].closed(),
        "an earlier snapshot saw the span close after the fact"
    );
    assert_eq!(early.links()[0].label(), None);

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// `DECSTR` (`CSI ! p`) is the polite reset — no screen clear — that a
/// well-behaved TUI sends on teardown, and it used to have no effect at
/// all (#233). What it resets here is the list a test can check on a
/// `Screen`: cursor keys, bracketed paste, mouse tracking, focus reporting,
/// the cursor's visibility and shape, and the character sets (covered in
/// `charset.rs`). Attributes, margins, origin and insert modes and the
/// keypad are not replayed — nothing observes them, so nothing could catch
/// a wrong replay — and the alternate screen is left alone, as specified.
#[test]
fn a_soft_reset_returns_the_modes_a_screen_can_observe() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e[?1049h\e[?1h\e[?2004h\e[?1000h\e[?1006h\e[?1004h\e[?25l\e[5 q",
        "set",
        "--wait",
        "--csi",
        "!p",
        " reset",
        "--wait",
    ])?;

    t.wait_until(|s| {
        s.contains("set")
            && s.alternate_screen()
            && s.application_cursor()
            && s.bracketed_paste()
            && s.mouse_mode() == MouseMode::PressRelease
            && s.focus_events()
            && !s.cursor().2
            && s.cursor_shape() == CursorShape::Bar
    })?;

    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("reset"))?;
    let s = t.screen();
    assert!(
        s.alternate_screen(),
        "the alternate screen is left alone: {s}"
    );
    assert!(!s.application_cursor(), "{s}");
    assert!(!s.bracketed_paste(), "{s}");
    assert_eq!(s.mouse_mode(), MouseMode::None, "{s}");
    assert!(!s.focus_events(), "{s}");
    assert!(s.cursor().2, "DECTCEM: the cursor is visible again: {s}");
    assert_eq!(s.cursor_shape(), CursorShape::Default, "{s}");
    assert_eq!(s.cursor_blink(), None, "{s}");

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn mouse_mode_reports_the_exact_tracking_mode() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e[?9h9\n",
        "--wait",
        "--raw",
        r"\e[?9l\e[?1000h1000\n",
        "--wait",
        "--raw",
        r"\e[?1000l\e[?1003h1003\n",
        "--wait",
    ])?;

    t.wait_until(|s| s.contains("9") && s.mouse_mode() == MouseMode::Press)?;
    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("1000") && s.mouse_mode() == MouseMode::PressRelease)?;
    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("1003") && s.mouse_mode() == MouseMode::AnyMotion)?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The set an application asked for is a different fact from the protocol
/// the terminal reports in, and only the latter was observable: crossterm
/// enables 1000, 1002 and 1003 together, the backend keeps the last, and an
/// application downgraded from any-motion to button-motion — losing hover
/// entirely — was indistinguishable from one that never had it (#151). The
/// input path keeps the collapsed value; the set is reported beside it.
#[test]
fn mouse_modes_reports_the_set_the_application_asked_for() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e[?1000h\e[?1002h\e[?1003h\e[?1006h",
        "all three\n",
        "--wait",
        "--csi",
        "?1003l",
        "minus 1003\n",
        "--wait",
        "--raw",
        r"\e[?1002l\e[?1000l",
        "none\n",
        "--wait",
    ])?;

    let set = |modes: &[MouseMode]| -> Vec<MouseMode> { modes.to_vec() };
    t.wait_until(|s| {
        s.contains("all three")
            && s.mouse_mode() == MouseMode::AnyMotion
            && s.mouse_modes().iter().collect::<Vec<_>>()
                == set(&[
                    MouseMode::PressRelease,
                    MouseMode::ButtonMotion,
                    MouseMode::AnyMotion,
                ])
    })?;
    let s = t.screen();
    assert!(
        s.mouse_modes().contains(MouseMode::ButtonMotion),
        "{:?}",
        s.mouse_modes()
    );
    assert!(
        !s.mouse_modes().contains(MouseMode::Press),
        "{:?}",
        s.mouse_modes()
    );
    assert_eq!(s.mouse_modes().len(), 3);

    // Releasing 1003 alone: the set still holds the other two, while the
    // protocol collapses to none — as xterm does, and as `click` needs.
    t.send(Key::Enter)?;
    t.wait_until(|s| {
        s.contains("minus 1003")
            && s.mouse_mode() == MouseMode::None
            && s.mouse_modes().iter().collect::<Vec<_>>()
                == set(&[MouseMode::PressRelease, MouseMode::ButtonMotion])
    })?;

    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("none") && s.mouse_modes().is_empty())?;
    assert_eq!(t.screen().mouse_modes(), MouseModes::default());
    assert!(t.screen().mouse_modes().contains(MouseMode::None));

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn a_clipboard_write_is_observable_with_its_payload() -> termlens::Result<()> {
    // The taskboard case from the coverage study: `y` copies the selected
    // title and paints a toast. The toast proves the code path ran; the
    // payload is the behaviour under test.
    let mut t = common::spawn_emit(
        Terminal::builder().timeout(Duration::from_secs(5)),
        &[
            "--raw",
            r"\e]52;c;V2lyZSB1cCB0aGUgUFRZIHJlYWRlcg==\x07",
            "copied to clipboard",
            "--wait",
        ],
    )?;

    // Assertable in a predicate, because it is snapshot state.
    t.wait_until(|s| {
        s.clipboard()
            .is_some_and(|c| c.text() == Some("Wire up the PTY reader"))
    })?;

    let s = t.screen();
    let clip = s.clipboard().expect("the write was captured");
    assert_eq!(clip.text(), Some("Wire up the PTY reader"));
    assert_eq!(clip.targets(), "c");
    // And the base64 never reached the grid.
    assert!(
        !s.contains("V2lyZSB1cCB0"),
        "the escape leaked into the grid:\n{s}"
    );

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn an_unreadable_clipboard_payload_is_reported_as_such() -> termlens::Result<()> {
    let mut t = common::spawn_emit(
        Terminal::builder().timeout(Duration::from_secs(5)),
        &["--raw", r"\e]52;p;not~valid~base64\x07", "done", "--wait"],
    )?;

    t.wait_until(|s| s.contains("done"))?;
    let s = t.screen();
    let clip = s.clipboard().expect("a write was still observed");
    // Distinguishable from an empty clipboard, which is the point.
    assert_eq!(clip.text(), None);
    assert_eq!(clip.targets(), "p");

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
