//! The builder rejects configurations that cannot produce a working
//! terminal, on its own terms — before the PTY layer turns them into
//! something much harder to read.

use std::time::Duration;

use termlens::{Error, Terminal};

#[test]
fn an_empty_program_name_is_a_short_typed_error() {
    let err = Terminal::builder()
        .timeout(Duration::from_secs(2))
        .spawn("")
        .expect_err("an empty program name cannot run");

    assert!(matches!(err, Error::Spawn { .. }), "got: {err}");
    let message = err.to_string();
    assert!(
        message.contains("no program name given"),
        "unhelpful message: {message}"
    );
    // The point of the guard: the failure is one line, not the PTY
    // layer's entire PATH search.
    assert!(
        message.lines().count() == 1,
        "expected a one-line error, got {} lines:\n{message}",
        message.lines().count()
    );
}

#[test]
fn a_missing_program_still_reports_the_underlying_search_failure() {
    let err = Terminal::builder()
        .timeout(Duration::from_secs(2))
        .spawn("termlens-definitely-not-installed-xyz")
        .expect_err("no such program");

    assert!(matches!(err, Error::Spawn { .. }), "got: {err}");
    let message = err.to_string();
    assert!(
        message.contains("termlens-definitely-not-installed-xyz"),
        "the program name belongs in the message: {message}"
    );
    // The underlying diagnosis is genuinely useful here — keep it.
    assert!(
        !message.contains("no program name given"),
        "the empty-name guard must not swallow real search failures: {message}"
    );
}
