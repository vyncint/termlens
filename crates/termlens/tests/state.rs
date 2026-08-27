//! Out-of-band terminal state on `Screen`: title, alternate screen, the
//! input modes and the `OSC 52` clipboard — asserted directly instead of
//! inferred from the grid.

use std::time::Duration;

use termlens::{CursorShape, Key, MouseMode, Screen, Terminal};

/// One script walks the whole state surface: set everything, assert, then
/// unwind everything and assert the way back.
#[test]
fn screen_reports_title_alternate_screen_and_input_modes() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                r"printf '\033]0;termlens state\007'; ",
                r"printf '\033[?1049h\033[?2004h\033[?1h\033[?1002h'; ",
                r"printf 'modes: on'; ",
                r"read _; ",
                r"printf '\033]2;phase two\033\\'; ",
                r"printf '\033[?1002l\033[?1l\033[?2004l\033[?1049l'; ",
                r"printf 'modes: off'; ",
                r"read _",
            ),
        ])
        .spawn("sh")?;

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
/// the same `Screen`. The script walks all three states an editor moves
/// through: never asked, switched, switched back.
#[test]
fn screen_reports_the_cursor_shape_the_application_asked_for() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                r"printf 'ready'; read _; ",
                // DECSCUSR 5: a blinking bar, the insert-mode cursor.
                r"printf '\033[5 q'; printf ' insert'; read _; ",
                // DECSCUSR 2: a steady block, the way back.
                r"printf '\033[2 q'; printf ' normal'; read _",
            ),
        ])
        .spawn("sh")?;

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
    fn run(script: &str) -> termlens::Result<Screen> {
        let mut t = Terminal::builder()
            .timeout(Duration::from_secs(10))
            .args(["-c", script])
            .spawn("sh")?;
        t.wait_until(|s| s.contains("see docs here"))?;
        let screen = t.screen();
        assert!(t.wait_exit()?.success());
        Ok(screen)
    }

    let linked =
        run(r"printf 'see \033]8;;https://example.invalid/a\033\\docs\033]8;;\033\\ here\n'")?;
    let plain = run(r"printf 'see docs here\n'")?;

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
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                // Open a span and leave it open across the pause.
                r"printf '\033]8;;http://a/\033\\LABEL one\n'; read _; ",
                // Close it, then open a second one.
                r"printf '\033]8;;\033\\\033]8;;http://b/\033\\X two\n'; read _",
            ),
        ])
        .spawn("sh")?;

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

#[test]
fn mouse_mode_reports_the_exact_tracking_mode() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            concat!(
                r"printf '\033[?9h9\n'; read _; ",
                r"printf '\033[?9l\033[?1000h1000\n'; read _; ",
                r"printf '\033[?1000l\033[?1003h1003\n'; read _",
            ),
        ])
        .spawn("sh")?;

    t.wait_until(|s| s.contains("9") && s.mouse_mode() == MouseMode::Press)?;
    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("1000") && s.mouse_mode() == MouseMode::PressRelease)?;
    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("1003") && s.mouse_mode() == MouseMode::AnyMotion)?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn a_clipboard_write_is_observable_with_its_payload() -> termlens::Result<()> {
    // The taskboard case from the coverage study: `y` copies the selected
    // title and paints a toast. The toast proves the code path ran; the
    // payload is the behaviour under test.
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(5))
        .args([
            "-c",
            concat!(
                r"printf '\033]52;c;V2lyZSB1cCB0aGUgUFRZIHJlYWRlcg==\007'; ",
                r"printf 'copied to clipboard'; read guard"
            ),
        ])
        .spawn("/bin/sh")?;

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
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(5))
        .args([
            "-c",
            r"printf '\033]52;p;not~valid~base64\007'; printf 'done'; read guard",
        ])
        .spawn("/bin/sh")?;

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
