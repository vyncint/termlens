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
fn a_missing_working_directory_fails_instead_of_running_elsewhere() {
    let missing = std::env::temp_dir().join("termlens-no-such-dir-xyz");
    assert!(!missing.is_dir(), "test precondition");

    let err = Terminal::builder()
        .timeout(Duration::from_secs(2))
        .current_dir(&missing)
        .args(["-c", "pwd"])
        .spawn("/bin/sh")
        .expect_err("the requested directory does not exist");

    assert!(matches!(err, Error::Spawn { .. }), "got: {err}");
    let message = err.to_string();
    assert!(
        message.contains(missing.to_str().expect("utf-8 path")),
        "the path belongs in the message: {message}"
    );
}

#[test]
fn a_file_is_not_a_working_directory() {
    // An existing non-directory must be rejected too — portable-pty's
    // is_dir() fallback treats it exactly like a missing path.
    let file = std::env::temp_dir().join("termlens-cwd-probe-file");
    std::fs::write(&file, b"x").expect("write probe file");

    let err = Terminal::builder()
        .timeout(Duration::from_secs(2))
        .current_dir(&file)
        .args(["-c", "pwd"])
        .spawn("/bin/sh")
        .expect_err("a file is not a directory");
    assert!(matches!(err, Error::Spawn { .. }), "got: {err}");

    let _ = std::fs::remove_file(&file);
}

/// A zero dimension used to reach vt100, which panicked on an
/// overflowing subtraction in debug builds and — worse — panicked on the
/// *reader thread* in release builds, killing the drain silently.
#[test]
fn a_zero_dimension_is_rejected_before_it_reaches_the_emulator() {
    for (cols, rows) in [(0u16, 24u16), (80, 0), (0, 0)] {
        let err = Terminal::builder()
            .size(cols, rows)
            .timeout(Duration::from_secs(2))
            .args(["-c", "read x"])
            .spawn("/bin/sh")
            .expect_err("a terminal cannot have a zero dimension");
        assert!(matches!(err, Error::Input(_)), "{cols}x{rows}: got {err}");
        assert!(
            err.to_string().contains(&format!("{cols}x{rows}")),
            "the offending size belongs in the message: {err}"
        );
    }
}

#[test]
fn resize_to_zero_is_refused_without_touching_the_pty_or_the_grid() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .size(80, 24)
        .timeout(Duration::from_secs(5))
        .args(["-c", "printf ready; read x"])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("ready"))?;

    for (cols, rows) in [(0u16, 24u16), (80, 0), (0, 0)] {
        let err = t
            .resize(cols, rows)
            .expect_err("a terminal cannot have a zero dimension");
        assert!(matches!(err, Error::Input(_)), "{cols}x{rows}: got {err}");
    }

    // The grid is untouched and the terminal still works.
    assert_eq!(t.screen().size(), (80, 24));
    t.resize(70, 20)?;
    assert_eq!(t.screen().size(), (70, 20));
    t.send(termlens::Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
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
