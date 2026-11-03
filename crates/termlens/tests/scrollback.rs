//! Retained scrollback: content that has scrolled off the top of the
//! screen stays assertable, and the bound on how much is pinned rather
//! than assumed.

use std::time::Duration;

use termlens::{Error, Key, Terminal};

mod common;

/// The `emit` fixture printing `count` numbered lines on a `rows`-row
/// screen, then parking so the terminal stays alive.
fn numbered(rows: u16, count: usize, scrollback: usize) -> termlens::Result<Terminal> {
    let lines: String = (1..=count).map(|i| format!("line-{i}\n")).collect();
    common::spawn_emit(
        Terminal::builder()
            .size(40, rows)
            .scrollback(scrollback)
            .timeout(Duration::from_secs(10)),
        &[&lines, "READY", "--wait"],
    )
}

/// The `emit` fixture on a screen small enough that six lines commit three
/// rows to history, with room for more.
fn reset_emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(
        Terminal::builder()
            .size(20, 4)
            .scrollback(20)
            .timeout(Duration::from_secs(10)),
        steps,
    )
}

#[test]
fn content_scrolled_off_the_top_is_still_assertable() -> termlens::Result<()> {
    let mut t = numbered(6, 40, 1000)?;
    t.wait_until(|s| s.contains("READY"))?;

    let s = t.screen();
    // Gone from the visible screen...
    assert!(
        !s.contains("line-1\n"),
        "line-1 should have scrolled off:\n{s}"
    );
    // ...and still there in the history.
    assert!(
        s.scrollback_text().contains("line-1\n"),
        "history:\n{}",
        s.scrollback_text()
    );
    assert!(s.full_text().contains("line-1\n"));
    assert!(s.full_text().contains("line-40"));
    assert!(s.scrollback_rows() >= 34, "rows: {}", s.scrollback_rows());

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The assertion an author writes when the application moves content
/// between the live region and native scrollback as it goes: "this
/// reached the terminal", without the test having to know which region it
/// currently sits in.
#[test]
fn full_text_answers_without_knowing_which_region_holds_it() -> termlens::Result<()> {
    // Ten lines on a twelve-row screen: everything is still visible.
    let mut t = numbered(12, 10, 1000)?;
    t.wait_until(|s| s.contains("READY"))?;
    let s = t.screen();
    assert_eq!(s.scrollback_rows(), 0, "nothing has scrolled yet");
    assert!(s.full_text().contains("line-1\n"));
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());

    // Same assertion, same content, now past the top of a smaller screen.
    let mut t = numbered(4, 10, 1000)?;
    t.wait_until(|s| s.contains("READY"))?;
    let s = t.screen();
    assert!(s.scrollback_rows() > 0, "some rows must have scrolled");
    assert!(s.full_text().contains("line-1\n"));
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The bound is real: past it, the oldest rows are gone. This is the
/// limit that replaces "scrolled-off output is unrecoverable" — the
/// feature exists, and what it cannot do is stated.
#[test]
fn the_retention_bound_drops_the_oldest_rows() -> termlens::Result<()> {
    let mut t = numbered(4, 60, 10)?;
    t.wait_until(|s| s.contains("READY"))?;

    let s = t.screen();
    assert_eq!(s.scrollback_rows(), 10, "bounded at the configured length");
    assert!(
        !s.full_text().contains("line-1\n"),
        "line-1 is far past the bound:\n{}",
        s.full_text()
    );
    // The newest retained rows are there; the visible screen holds the rest.
    assert!(s.full_text().contains("line-60"));

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn retention_can_be_switched_off() -> termlens::Result<()> {
    let mut t = numbered(4, 20, 0)?;
    t.wait_until(|s| s.contains("READY"))?;

    let s = t.screen();
    assert_eq!(s.scrollback_rows(), 0);
    assert_eq!(s.scrollback_text(), "");
    assert_eq!(s.full_text(), s.text(), "full_text is then just the screen");
    assert!(!s.full_text().contains("line-1\n"));

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// History is snapshot state, so it obeys snapshot rules — which makes it
/// usable in a wait predicate.
#[test]
fn history_is_observable_from_a_predicate() -> termlens::Result<()> {
    let mut t = common::spawn_emit(
        Terminal::builder()
            .size(40, 3)
            .scrollback(100)
            .timeout(Duration::from_secs(10)),
        &["committed-block\n", "a\nb\nc\nd\n", "--wait"],
    )?;

    // The block is asserted on *after* it has left the screen, from inside
    // the wait itself.
    t.wait_until(|s| s.scrollback_text().contains("committed-block"))?;
    assert!(!t.screen().contains("committed-block"));

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The most-copied line in the docs, on text that scrolled away in the same
/// burst: the wait can never succeed, and the screen in the error does not
/// show the text either — so it used to read as "the app never printed it".
/// The error now says that rows are off the top, and where they can be seen.
#[test]
fn a_wait_on_scrolled_off_text_names_the_history_in_its_error() -> termlens::Result<()> {
    let mut t = numbered(3, 10, 100)?;
    t.wait_until(|s| s.contains("READY"))?;
    let scrolled = t.screen().scrollback_rows();
    assert!(scrolled > 1, "the setup must have scrolled: {}", t.screen());

    // "line-1" is in history; `contains` reads the grid alone.
    let err = t
        .wait_until_for(|s| s.contains("line-1\n"), Duration::from_millis(300))
        .expect_err("the text is in history, not on the grid");
    assert!(matches!(err, Error::Timeout { .. }), "got: {err}");
    let msg = err.to_string();
    assert!(
        msg.contains(&format!("{scrolled} rows have scrolled off the top")),
        "{msg}"
    );
    assert!(msg.contains("full_text"), "the remedy is named: {msg}");
    // And the claim checks out against the very screen the error carries.
    assert!(err.screen().unwrap().full_text().contains("line-1\n"));

    // The EOF path carries the same note.
    t.send(Key::Enter)?;
    let err = t
        .wait_until(|s| s.contains("line-1\n"))
        .expect_err("the child has exited");
    assert!(matches!(err, Error::Eof { .. }), "got: {err}");
    assert!(err.to_string().contains("scrolled off the top"), "{err}");
    Ok(())
}

/// No history, no note: an application that owns its viewport is never told
/// about rows it does not have.
#[test]
fn a_wait_with_nothing_scrolled_carries_no_history_note() -> termlens::Result<()> {
    let mut t = numbered(12, 3, 100)?;
    t.wait_until(|s| s.contains("READY"))?;
    assert_eq!(t.screen().scrollback_rows(), 0);
    let err = t
        .wait_until_for(|s| s.contains("absent"), Duration::from_millis(200))
        .expect_err("never printed");
    assert!(!err.to_string().contains("scrolled off"), "{err}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// `ESC c` (RIS) rebuilds the backend's screen and empties its history, but
/// the rows termlens already committed must survive it and the rows that
/// scroll afterwards must still be committed. The high-water mark used to be
/// cleared only when the reset arrived in a read of its own, so which rows
/// went missing depended on how the PTY chunked the writes (#391). Each
/// chunking is pinned: the reset in a write of its own, the reset merged
/// with the rows that follow it, and the whole stream in one write. The
/// sleeps keep those writes apart for the reader; the assertion holds
/// whether or not the PTY happens to coalesce them anyway.
#[test]
fn a_hard_reset_neither_drops_the_history_before_it_nor_the_rows_after_it() -> termlens::Result<()>
{
    // Three lines scroll off before the reset and three more after, on a
    // 4-row screen; the order they were written in is the assertion.
    let expected = "1|\n2|\n3|\nA|\nB|\nC|";

    let shapes: [(&str, &[&str]); 3] = [
        (
            "reset in a write of its own",
            &[
                "1|\n2|\n3|\n4|\n5|\n6|\n",
                "--sleep",
                "100ms",
                "--raw",
                "\\ec",
                "--sleep",
                "100ms",
                "A|\nB|\nC|\nD|\nE|\nF|\n",
                "DONE",
                "--wait",
            ],
        ),
        (
            "reset merged with the rows after it",
            &[
                "1|\n2|\n3|\n4|\n5|\n6|\n",
                "--sleep",
                "100ms",
                "--raw",
                "\\ecA|\nB|\nC|\nD|\nE|\nF|\n",
                "DONE",
                "--wait",
            ],
        ),
        (
            "the whole stream in one write",
            &[
                "--raw",
                "1|\n2|\n3|\n4|\n5|\n6|\n\\ecA|\nB|\nC|\nD|\nE|\nF|\nDONE",
                "--wait",
            ],
        ),
    ];

    for (name, steps) in shapes {
        let mut t = reset_emit(steps)?;
        t.wait_until(|s| s.contains("DONE"))?;

        let s = t.screen();
        assert_eq!(s.scrollback_text(), expected, "{name}");
        assert!(
            s.full_text().starts_with(expected),
            "{name}: history and grid out of order:\n{}",
            s.full_text()
        );

        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success(), "{name}");
    }
    Ok(())
}

/// Lines that scroll off and the switch to the alternate screen in one
/// write, as a full-screen program that starts right after printing writes
/// them. The scrolled rows were copied only once the main screen came back,
/// and a reset before that lost them (#622). The same rows with the switch
/// in a write of its own are the control: history must not depend on where
/// the PTY split the output.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY carries out the alternate-screen switch and re-renders the screen itself, so the bytes termlens reads are not the ones the child wrote (#149)"
)]
fn rows_that_scroll_in_the_write_that_enters_the_alternate_screen_stay_in_history(
) -> termlens::Result<()> {
    let shapes: [(&str, &[&str]); 3] = [
        (
            "one write",
            &["--raw", "l1\nl2\nl3\nl4\nl5\\e[?1049hTUI", "--wait"],
        ),
        (
            "the switch in a write of its own",
            &[
                "--raw",
                "l1\nl2\nl3\nl4\nl5",
                "--sleep",
                "100ms",
                "--raw",
                "\\e[?1049hTUI",
                "--wait",
            ],
        ),
        (
            "a reset before the main screen is back",
            &["--raw", "l1\nl2\nl3\nl4\nl5\\e[?1049hTUI\\ecDONE", "--wait"],
        ),
    ];
    for (name, steps) in shapes {
        let mut t = common::spawn_emit(
            Terminal::builder()
                .size(10, 3)
                .scrollback(100)
                .timeout(Duration::from_secs(10)),
            steps,
        )?;
        let last = if name.starts_with("a reset") {
            "DONE"
        } else {
            "TUI"
        };
        t.wait_until(|s| s.contains(last))?;
        let s = t.screen();
        assert_eq!(s.scrollback_text(), "l1\nl2", "{name}: {s}");
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success(), "{name}");
    }
    Ok(())
}

/// Wider and shorter, then back, over a soft-wrapped line: the rows the
/// first resize pushed into history were widened when they were copied,
/// which cleared the wrap — in the primary grid only, so the second resize
/// pulled different rows back into it and into the attribute shadow, and a
/// debug build panicked on the next snapshot (#620).
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY applies a resize on its own thread and re-renders the screen itself, so the rows termlens holds after it are conhost's layout (#149)"
)]
fn a_resize_round_trip_keeps_a_soft_wrapped_line_wrapped() -> termlens::Result<()> {
    for styled in [false, true] {
        let mut t = common::spawn_emit(
            Terminal::builder()
                .size(10, 6)
                .scrollback_styles(styled)
                .timeout(Duration::from_secs(10)),
            &["l1\nl2\nAAAAAAAAAABBBBB\nl4\nREADY", "--wait"],
        )?;
        t.wait_until(|s| s.contains("READY"))?;
        assert!(t.screen().row_wrapped(2), "styled history {styled}");
        t.resize(20, 3)?;
        assert_eq!(
            t.screen().scrollback_text(),
            "l1\nl2\nAAAAAAAAAA",
            "styled history {styled}"
        );
        t.resize(10, 6)?;
        let s = t.screen();
        assert_eq!(
            s.text(),
            "l1\nl2\nAAAAAAAAAA\nBBBBB\nl4\nREADY",
            "styled history {styled}"
        );
        assert!(s.row_wrapped(2), "styled history {styled}: {s}");
        assert!(
            s.logical_text().contains("AAAAAAAAAABBBBB"),
            "styled history {styled}: {s}"
        );
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success(), "styled history {styled}");
    }
    Ok(())
}
