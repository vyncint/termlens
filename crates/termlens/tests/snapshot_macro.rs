//! `assert_screen_snapshot!` makes the three decisions a TUI snapshot needs
//! (#253): settle, styles on, one instant — and still takes a `Screen`.

#![cfg(feature = "insta")]

use std::time::Duration;

use termlens::{Key, Terminal};

mod common;

fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(
        Terminal::builder()
            .size(30, 3)
            .timeout(Duration::from_secs(10)),
        steps,
    )
}

#[test]
fn a_terminal_is_settled_and_snapshotted_with_styles() -> termlens::Result<()> {
    let mut t = emit(&["--raw", r"\e[1mReady\e[0m later", "--wait"])?;
    termlens::assert_screen_snapshot!(t, after = |s| s.contains("later"));
    termlens::assert_screen_snapshot!(t, after = |s| s.contains("later"), styles = false);
    termlens::assert_screen_snapshot!(t);
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn a_screen_is_recorded_as_it_is_and_refuses_a_wait() -> termlens::Result<()> {
    let mut t = emit(&["fixed", "--wait"])?;
    t.wait_until(|s| s.contains("fixed"))?;
    let screen = t.screen();
    termlens::assert_screen_snapshot!(screen);
    termlens::assert_screen_snapshot!(t.screen(), styles = false);
    let refused: termlens::Result<()> = (|| {
        termlens::assert_screen_snapshot!(screen, after = |s| s.contains("fixed"));
        Ok(())
    })();
    let err = refused.expect_err("a Screen is one instant already");
    assert!(
        err.to_string().contains("pass the Terminal instead"),
        "{err}"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
