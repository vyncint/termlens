//! Soft wraps: a line long enough to wrap is two rows on the grid, and the
//! backend records which rows wrapped. `contains` and `find` read the grid
//! row by row and say so; `logical_text` joins the rows back (#265).

use std::time::Duration;

use termlens::{Key, Terminal};

mod common;

fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(
        Terminal::builder()
            .size(20, 4)
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
