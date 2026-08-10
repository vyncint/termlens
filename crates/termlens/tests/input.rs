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
