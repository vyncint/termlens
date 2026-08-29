//! Retained scrollback: content that has scrolled off the top of the
//! screen stays assertable, and the bound on how much is pinned rather
//! than assumed.

use std::time::Duration;

use termlens::{Error, Key, Terminal};

/// `sh` printing `count` numbered lines on a `rows`-row screen, then
/// parking so the terminal stays alive.
fn numbered(rows: u16, count: usize, scrollback: usize) -> termlens::Result<Terminal> {
    Terminal::builder()
        .size(40, rows)
        .scrollback(scrollback)
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            &format!(
                "i=1; while [ $i -le {count} ]; do printf 'line-%d\\n' $i; \
                 i=$((i+1)); done; printf 'READY'; read guard"
            ),
        ])
        .spawn("/bin/sh")
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
    let mut t = Terminal::builder()
        .size(40, 3)
        .scrollback(100)
        .timeout(Duration::from_secs(10))
        .args([
            "-c",
            r"printf 'committed-block\n'; printf 'a\nb\nc\nd\n'; read guard",
        ])
        .spawn("/bin/sh")?;

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
