//! Soft wraps: a line long enough to wrap is two rows on the grid, and the
//! backend records which rows wrapped. `contains` and `find` read the grid
//! row by row and say so; `logical_text` joins the rows back (#265).

use std::time::Duration;

use termlens::{Key, Terminal};

mod common;

fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    emit_sized(20, 4, steps)
}

fn emit_sized(cols: u16, rows: u16, steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(
        Terminal::builder()
            .size(cols, rows)
            .timeout(Duration::from_secs(10)),
        steps,
    )
}

/// The reproduction from the issue: `brown fox jumps` is plainly on the
/// screen, straddling the wrap.
#[test]
fn a_needle_spanning_a_wrap_is_found_in_the_logical_text() -> termlens::Result<()> {
    let mut t = emit(&["the quick brown fox jumps\nDONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0), "the quick brown fox ", "{s}");
    assert_eq!(s.row_text(1).trim_end(), "jumps", "{s}");
    assert!(s.row_wrapped(0), "row 0 ended in a soft wrap: {s}");
    assert!(!s.row_wrapped(1), "row 1 ended in a line end: {s}");
    assert!(!s.row_wrapped(99), "off the screen is not wrapped");

    // The grid-shaped searches do not span the wrap, by contract…
    assert!(!s.contains("brown fox jumps"), "{s}");
    assert_eq!(s.find("brown fox jumps"), None, "{s}");
    // …and the accessor for that assertion does. Blank rows below stay
    // blank lines, exactly as in `text()`.
    assert_eq!(
        s.logical_text().trim_end(),
        "the quick brown fox jumps\nDONE",
        "{s}"
    );
    assert!(s.logical_text().contains("brown fox jumps"));
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A row that merely reaches the margin without spilling over is not a
/// wrap, and a hard line end never is.
#[test]
fn a_full_row_ended_by_a_newline_is_not_a_wrap() -> termlens::Result<()> {
    let mut t = emit(&["12345678901234567890\nnext\nDONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert!(!s.row_wrapped(0), "{s}");
    assert_eq!(
        s.logical_text().trim_end(),
        "12345678901234567890\nnext\nDONE",
        "{s}"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// With nothing wrapped there is nothing to undo, so the two accessors
/// agree on the whole grid — leading blank rows and an all-blank screen
/// included (#377).
#[test]
fn logical_text_is_text_verbatim_when_no_row_wraps() -> termlens::Result<()> {
    let mut t = emit_sized(20, 5, &["\n\nhello world\n", "--wait"])?;
    t.wait_until(|s| s.contains("hello world"))?;
    let s = t.screen();
    assert_eq!(s.text(), "\n\nhello world\n\n", "{s}");
    assert_eq!(s.logical_text(), s.text(), "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());

    // An all-blank screen is blank lines, not the empty string.
    let mut blank = emit_sized(20, 5, &["--wait"])?;
    let s = blank.screen();
    assert_eq!(s.text(), "\n\n\n\n", "{s}");
    assert_eq!(s.logical_text(), s.text(), "{s}");
    blank.send(Key::Enter)?;
    assert!(blank.wait_exit()?.success());
    Ok(())
}

/// A wrapped screen differs from `text()` by the joined wrap and nothing
/// else — the leading blank row survives (#377). This is the issue's 10x4
/// reproduction with `printf '\nabcdefghijklm\n'`.
#[test]
fn logical_text_differs_from_text_only_by_the_joined_wrap() -> termlens::Result<()> {
    let mut t = emit_sized(10, 4, &["\nabcdefghijklm\n", "--wait"])?;
    t.wait_until(|s| s.contains("klm"))?;
    let s = t.screen();
    assert!(s.row_wrapped(1), "row 1 ended in a soft wrap: {s}");
    assert_eq!(s.text(), "\nabcdefghij\nklm\n", "{s}");
    assert_eq!(s.logical_text(), "\nabcdefghijklm\n", "{s}");
    assert_eq!(
        s.logical_text(),
        s.text().replace("abcdefghij\nklm", "abcdefghijklm"),
        "the only difference is the joined wrap: {s}"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A right-aligned status line ending in an emoji with a presentation
/// selector, on the bottom row, then a redraw of the row above: the emoji
/// no longer fits in the last column and moves to the start of a new
/// bottom row, as xterm moves it. vt100 0.18.1 left it wide in the last
/// column of the row above, and the redraw panicked inside it, on the
/// reader thread, so the screen stopped advancing (#619).
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY lays out the emoji and re-renders the screen itself, so the bytes termlens reads are conhost's (#149)"
)]
fn a_selector_widened_emoji_in_the_bottom_right_cell_survives_a_redraw() -> termlens::Result<()> {
    let mut t = emit(&[
        "--csi",
        "4;1H",
        "               warn\u{26a0}\u{fe0f}",
        "--csi",
        "A",
        "\rredrawn             ",
        "--csi",
        "1;1HDONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(2).trim_end(), "redrawn", "{s}");
    assert_eq!(s.row_text(3).trim_end(), "\u{26a0}\u{fe0f}", "{s}");
    assert!(s.cell(3, 0).is_some_and(termlens::Cell::is_wide), "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
