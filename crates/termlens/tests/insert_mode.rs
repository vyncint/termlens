//! Insert mode (`IRM`, `CSI 4 h` / `CSI 4 l`): a printed character pushes
//! the rest of its row right instead of overwriting the cell under the
//! cursor. `smir`/`rmir` are in the terminfo entry every child is handed,
//! and ncurses uses the mode for `insch`; before this the mode was parsed
//! and dropped, so an inserted character ate the tail of the line (#261).

use std::time::Duration;

use termlens::{Key, Terminal};

mod common;

/// The `emit` fixture on the 20x2 terminal the issue's reproductions use.
fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(
        Terminal::builder()
            .size(20, 2)
            .timeout(Duration::from_secs(10)),
        steps,
    )
}

/// The reproduction from the issue.
#[test]
fn an_insert_pushes_the_rest_of_the_row_right() -> termlens::Result<()> {
    let mut t = emit(&[
        "abcd", "--csi", "1G", "--csi", "4h", "ZZ", "\nDONE", "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "ZZabcd", "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The mode itself is observable: "the application put the terminal in
/// insert mode and left it there" is the same shape of assertion as
/// `alternate_screen()`.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY does not forward the mode sets an application sends, so insert mode is never observed — it performs the insert itself (#149)"
)]
fn the_mode_is_reported_on_the_screen() -> termlens::Result<()> {
    let mut t = emit(&["--csi", "4h", "DONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert!(s.insert_mode(), "the application left insert mode on: {s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn csi_4_l_returns_to_overwriting() -> termlens::Result<()> {
    let mut t = emit(&[
        "abcd", "--csi", "1G", "--csi", "4h", "ZZ", "--csi", "4l", "--csi", "1G", "YY", "\nDONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "YYabcd", "{s}");
    assert!(!s.insert_mode(), "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Both resets clear the mode: `RIS` (which also clears the screen) and
/// `DECSTR` (which clears nothing).
#[test]
fn ris_and_decstr_clear_the_mode() -> termlens::Result<()> {
    for reset in [&["--esc", "c"][..], &["--csi", "!p"]] {
        let mut steps = vec!["--csi", "4h"];
        steps.extend_from_slice(reset);
        steps.extend(["abcd", "--csi", "1G", "XX", "\nDONE", "--wait"]);
        let mut t = emit(&steps)?;
        t.wait_until(|s| s.contains("DONE"))?;
        let s = t.screen();
        assert_eq!(
            s.row_text(0).trim_end(),
            "XXcd",
            "the mode must not survive {reset:?}:\n{s}"
        );
        assert!(!s.insert_mode(), "{s}");
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success());
    }
    Ok(())
}

/// A run that reaches the right margin reserves no room past it: the
/// characters pushed off the end are gone, and nothing wraps.
#[test]
fn an_insert_at_the_right_margin_does_not_reserve_past_it() -> termlens::Result<()> {
    let mut t = emit(&[
        "12345678901234567890",
        "--csi",
        "19G",
        "--csi",
        "4h",
        "ab",
        "\nDONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0), "123456789012345678ab", "{s}");
    assert_eq!(s.find("DONE"), Some((1, 0)), "nothing wrapped: {s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The run length is in columns: a wide character reserves two.
#[test]
fn a_wide_character_reserves_two_columns() -> termlens::Result<()> {
    let mut t = emit(&[
        "abcd", "--csi", "1G", "--csi", "4h", "汉", "\nDONE", "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "汉abcd", "{s}");
    assert_eq!(
        s.find("abcd"),
        Some((0, 2)),
        "the tail moved by two columns: {s}"
    );
    assert!(s.cell(0, 0).unwrap().is_wide());
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
