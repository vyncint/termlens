//! Bytes that are not UTF-8 reach the grid as U+FFFD, and the columns after
//! them stay where a terminal puts them (#217).

use std::time::Duration;

use termlens::{Key, Terminal};

mod common;

/// The `emit` fixture; `--raw` is what carries a byte that is not UTF-8.
/// Steps are documented in `fixtures/emit/src/main.rs`.
fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(Terminal::builder().timeout(Duration::from_secs(5)), steps)
}

#[test]
fn an_invalid_byte_is_a_replacement_character_and_the_columns_hold() -> termlens::Result<()> {
    // A Latin-1 `é` in a file name, a corrupted log line: the byte used to
    // be deleted from the grid, so `done` sat one column too far left and
    // nothing on the Screen said a byte had gone.
    let mut t = emit(&["--raw", r"raw: caf\xe9 done", "--wait"])?;
    t.wait_until(|s| s.contains("done"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "raw: caf\u{FFFD} done");
    assert_eq!(s.cell(0, 8).unwrap().contents(), "\u{FFFD}");
    assert_eq!(s.find("done"), Some((0, 10)));
    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

#[test]
fn a_character_split_across_two_writes_is_still_one_character() -> termlens::Result<()> {
    // 汉 is E6 B1 89. The lead byte arrives in one read and the rest in
    // another; the sanitizer must carry it, not replace it.
    let mut t = emit(&[
        "--raw",
        r"ab\xe6",
        "--sleep",
        "200ms",
        "--raw",
        r"\xb1\x89cd",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("cd"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "ab汉cd");
    assert_eq!(s.find("cd"), Some((0, 4)));
    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

#[test]
fn wait_idle_does_not_call_a_half_written_character_silence() -> termlens::Result<()> {
    // The application stops for 600ms in the middle of a character. That is
    // not idleness — the stream ends mid-character — so `wait_idle` must
    // hold until the character completes, and the screen it returns to must
    // show it whole.
    let mut t = emit(&[
        "--raw",
        r"ab\xe6",
        "--sleep",
        "600ms",
        "--raw",
        r"\xb1\x89cd",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("ab"))?;
    t.wait_idle_for(Duration::from_millis(150), Duration::from_secs(5))?;
    let s = t.screen();
    assert_eq!(
        s.row_text(0).trim_end(),
        "ab汉cd",
        "wait_idle returned on a half-written character:\n{s}"
    );
    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}
