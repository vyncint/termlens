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
fn the_default_working_directory_is_the_test_process_s() -> termlens::Result<()> {
    // Without `current_dir` the child used to start in $HOME — the PTY
    // layer's fallback — while `current_dir`'s rustdoc said it inherited the
    // test runner's (#215). Pinned here so the default cannot move quietly.
    let mut t = Terminal::builder().spawn("/bin/pwd")?;
    assert!(t.wait_exit()?.success());
    let reported = std::path::PathBuf::from(t.screen().row_text(0).trim());
    let expected = std::env::current_dir()?;
    assert_eq!(
        reported.canonicalize()?,
        expected.canonicalize()?,
        "child ran in {} rather than the test process's {}",
        reported.display(),
        expected.display()
    );
    Ok(())
}

#[test]
fn a_bare_program_name_under_env_clear_is_refused_with_the_remedies() {
    // env_clear drops PATH, and the PTY layer's only diagnostic for the bare
    // name that then cannot resolve was "Unable to resolve the PATH" (#222).
    let err = Terminal::builder().env_clear().spawn("sh").unwrap_err();
    assert!(matches!(err, Error::Spawn { .. }), "{err}");
    let msg = err.to_string();
    for needed in ["`sh`", "env_clear", "PATH", "absolute path"] {
        assert!(msg.contains(needed), "missing {needed:?} in: {msg}");
    }
    // Either remedy works: a PATH of the test's own…
    let with_path = Terminal::builder()
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["-c", "true"])
        .spawn("sh");
    assert!(with_path.is_ok(), "{:?}", with_path.err());
    // …or an absolute path, which never needed one.
    let absolute = Terminal::builder()
        .env_clear()
        .args(["-c", "true"])
        .spawn("/bin/sh");
    assert!(absolute.is_ok(), "{:?}", absolute.err());
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

/// One column panics the emulator on a double-width character, and one row
/// panics it on a line that wraps — on the reader thread, in both profiles,
/// where the symptom is a frozen screen and a wait that runs to its
/// deadline while cargo prints `test result: ok` (#211). The guard against
/// zero was one value too low on each axis.
#[test]
fn a_single_row_or_column_is_refused_like_a_zero() {
    for (cols, rows) in [(1u16, 8u16), (80, 1), (1, 1), (2, 1), (1, 2)] {
        let err = Terminal::builder()
            .size(cols, rows)
            .args(["-c", "true"])
            .spawn("/bin/sh")
            .expect_err(&format!("{cols}x{rows} must be refused"));
        assert!(matches!(err, Error::Size(_)), "{cols}x{rows}: got {err}");
        assert!(
            err.to_string().contains("at least 2"),
            "the message must name the floor: {err}"
        );
    }
}

/// The two shapes that used to kill the emulator thread, at the smallest
/// size still allowed. Both must render — the floor is the smallest guard
/// that closes the panics, not a retreat from small terminals.
#[test]
fn the_narrowest_allowed_terminal_renders_both_trigger_shapes() -> termlens::Result<()> {
    // Trigger A was one column meeting a double-width character.
    let mut wide = Terminal::builder()
        .size(2, 8)
        .timeout(Duration::from_secs(5))
        .args(["-c", r"printf '\346\261\211'; read guard"])
        .spawn("/bin/sh")?;
    wide.wait_until(|s| s.contains("汉"))?;
    assert_eq!(wide.screen().row_text(0).trim_end(), "汉");
    wide.send(termlens::Key::Enter)?;
    wide.wait_exit()?;

    // Trigger B was one row meeting a line that wraps.
    let mut wrap = Terminal::builder()
        .size(2, 2)
        .timeout(Duration::from_secs(5))
        .args(["-c", r"printf 'abcZ'; read guard"])
        .spawn("/bin/sh")?;
    wrap.wait_until(|s| s.contains("Z"))?;
    let screen = wrap.screen();
    assert_eq!(screen.row_text(0).trim_end(), "ab", "{screen}");
    assert_eq!(screen.row_text(1).trim_end(), "cZ", "{screen}");
    wrap.send(termlens::Key::Enter)?;
    wrap.wait_exit()?;
    Ok(())
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
        assert!(matches!(err, Error::Size(_)), "{cols}x{rows}: got {err}");
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

    // Zero, and — the value the guard used to let through — one, on each
    // axis (#211). `resize(cols - 1, …)` walked down in a loop reaches both
    // by ordinary arithmetic.
    for (cols, rows) in [(0u16, 24u16), (80, 0), (0, 0), (1, 24), (80, 1), (1, 1)] {
        let err = t
            .resize(cols, rows)
            .expect_err("a terminal needs at least two columns and two rows");
        assert!(matches!(err, Error::Size(_)), "{cols}x{rows}: got {err}");
    }
    // The grid is untouched and the terminal still works.
    assert_eq!(t.screen().size(), (80, 24));
    t.resize(70, 20)?;
    assert_eq!(t.screen().size(), (70, 20));
    // …and the floor itself is a legal size to resize to.
    t.resize(2, 2)?;
    assert_eq!(t.screen().size(), (2, 2));
    t.send(termlens::Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// `send` and `signal` refuse once the child is gone; `resize` used to
/// succeed and report a geometry no application ever rendered at, with the
/// dead child's last frame underneath it. Refused now, on the same evidence
/// `send` uses — EOF, so nothing is left to receive the SIGWINCH.
#[test]
fn resize_after_the_child_exits_is_refused_like_send() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .size(20, 4)
        .timeout(Duration::from_secs(10))
        .args(["-c", "printf ready; read _"])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("ready"))?;
    t.send(termlens::Key::Enter)?;
    assert!(t.wait_exit()?.success());
    let before = t.screen();
    assert_eq!(before.size(), (20, 4));

    let err = t
        .resize(60, 10)
        .expect_err("nothing is left to receive a SIGWINCH");
    assert!(matches!(err, Error::Write { .. }), "got: {err}");
    let msg = err.to_string();
    assert!(msg.contains("resize"), "{msg}");
    assert!(msg.contains("the child is gone"), "{msg}");
    // The final screen is untouched: still the size the child exited with,
    // and the very same observation.
    assert_eq!(t.screen().size(), (20, 4));
    assert_eq!(
        t.screen(),
        before,
        "a refused resize must not move the snapshot"
    );
    // A size that cannot work is still the size error, and is checked first.
    assert!(matches!(t.resize(0, 10), Err(Error::Size(_))));
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

/// An implausible size used to be accepted, and the consequence appeared
/// nowhere near the cause: 5000x5000 spawned fine, then the first wait
/// spent 16 seconds building snapshots before timing out with a message
/// about the predicate.
#[test]
fn an_implausible_size_is_refused_with_the_limit_named() {
    for (cols, rows) in [(5000, 5000), (1001, 24), (80, 1001), (u16::MAX, u16::MAX)] {
        let err = Terminal::builder()
            .size(cols, rows)
            .args(["-c", "true"])
            .spawn("/bin/sh")
            .expect_err("a terminal this large is refused");
        assert!(matches!(err, Error::Size(_)), "{cols}x{rows}: got {err}");
        let msg = err.to_string();
        assert!(
            msg.contains("1000"),
            "the limit belongs in the message: {msg}"
        );
        assert!(
            msg.contains(&format!("{cols}x{rows}")),
            "the offending size belongs in the message: {msg}"
        );
    }
}

/// The boundary itself is legal, and a resize is held to the same limit —
/// `resize` is where a computed dimension is most likely to go wrong.
#[test]
fn the_limit_is_inclusive_and_resize_honours_it() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .size(1000, 1000)
        .timeout(Duration::from_secs(20))
        .args(["-c", "printf READY; read guard"])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("READY"))?;
    assert_eq!(t.screen().size(), (1000, 1000));

    let err = t.resize(1001, 1000).expect_err("past the limit");
    assert!(matches!(err, Error::Size(_)), "{err}");
    assert!(err.to_string().contains("1000"), "{err}");
    // Refused without disturbing the grid, like the zero case.
    assert_eq!(t.screen().size(), (1000, 1000));

    t.send_str("\n")?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
