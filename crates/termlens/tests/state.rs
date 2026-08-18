//! Out-of-band terminal state on `Screen`: title, alternate screen, the
//! input modes and the `OSC 52` clipboard — asserted directly instead of
//! inferred from the grid.

use std::time::Duration;

use termlens::{Key, MouseMode, Terminal};

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

    t.send(Key::Enter);
    t.wait_until(|s| {
        s.contains("modes: off")
            && s.title() == "phase two" // OSC 2, ST-terminated
            && !s.alternate_screen()
            && !s.bracketed_paste()
            && !s.application_cursor()
            && s.mouse_mode() == MouseMode::None
    })?;

    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The tracking mode the app enabled is reported by name, not collapsed.
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
    t.send(Key::Enter);
    t.wait_until(|s| s.contains("1000") && s.mouse_mode() == MouseMode::PressRelease)?;
    t.send(Key::Enter);
    t.wait_until(|s| s.contains("1003") && s.mouse_mode() == MouseMode::AnyMotion)?;
    t.send(Key::Enter);
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

    t.send(Key::Enter);
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

    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}
