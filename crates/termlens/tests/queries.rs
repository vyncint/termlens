//! The query responder: capability-probing apps get real answers instead
//! of hanging, and whatever stays unanswered is named in timeout errors.
//!
//! Each shell script here genuinely BLOCKS on the terminal's reply
//! (`head -c N` reads exactly the reply bytes), then prints a marker the
//! test waits for — the marker appearing proves the app was unblocked.

use std::time::Duration;

use termlens::{Error, Key, Terminal};

fn sh(script: &str) -> termlens::Result<Terminal> {
    Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args(["-c", script])
        .spawn("/bin/sh")
}

#[test]
fn cursor_position_reports_the_position_at_the_query() -> termlens::Result<()> {
    // After printing "abc" the cursor sits at row 1, col 4 (1-based on the
    // wire); the CPR reply is exactly 6 bytes: ESC [ 1 ; 4 R.
    let mut t = sh(concat!(
        r"stty -icanon -echo; printf 'abc\033[6n'; ",
        r#"reply=$(head -c 6 | tr '\033' 'E'); "#,
        r#"printf '\nunblocked:%s' "$reply"; read guard"#
    ))?;
    t.wait_until(|s| s.contains("unblocked:E[1;4R"))?;
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn device_attribute_probes_are_unblocked() -> termlens::Result<()> {
    // DA1 reply is ESC [ ? 6 2 ; 2 2 c = 9 bytes. This is also the exact
    // pattern kitty-protocol probes rely on: the DA1 answer arriving tells
    // the app "no kitty support", exactly like a real non-kitty terminal.
    let mut t = sh(concat!(
        r"stty -icanon -echo; printf '\033[c'; ",
        r#"reply=$(head -c 9 | tr '\033' 'E'); "#,
        r#"printf 'unblocked:%s' "$reply"; read guard"#
    ))?;
    t.wait_until(|s| s.contains("unblocked:E[?62;22c"))?;
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn background_color_query_gets_the_configured_answer() -> termlens::Result<()> {
    // OSC 11 reply: ESC ] 1 1 ; rgb:1e1e/1e1e/2e2e BEL = 24 bytes.
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .background_rgb(0x1e, 0x1e, 0x2e)
        .args([
            "-c",
            concat!(
                r"stty -icanon -echo; printf '\033]11;?\007'; ",
                r#"reply=$(head -c 24 | tr '\033\007' 'EG'); "#,
                r#"printf 'unblocked:%s' "$reply"; read guard"#
            ),
        ])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("unblocked:E]11;rgb:1e1e/1e1e/2e2eG"))?;
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn text_area_size_reports_the_real_grid() -> termlens::Result<()> {
    // XTWINOPS 18 reply: ESC [ 8 ; 24 ; 80 t = 10 bytes.
    let mut t = sh(concat!(
        r"stty -icanon -echo; printf '\033[18t'; ",
        r#"reply=$(head -c 10 | tr '\033' 'E'); "#,
        r#"printf 'unblocked:%s' "$reply"; read guard"#
    ))?;
    t.wait_until(|s| s.contains("unblocked:E[8;24;80t"))?;
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn unanswerable_queries_turn_timeouts_into_diagnoses() {
    // CSI 14 t (pixel size) is recognized as a question termlens cannot
    // answer; the app blocks, and the timeout error names the query.
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(500))
        .args(["-c", r"printf '\033[14t'; head -c 4 >/dev/null; echo never"])
        .spawn("/bin/sh")
        .unwrap();
    let err = t.wait_until(|s| s.contains("never")).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("^[[14t"), "query not named in: {msg}");
    assert!(msg.contains("received no answer"), "no diagnosis in: {msg}");
    // Drop kills the blocked child.
}

#[test]
fn the_responder_can_be_disabled_and_says_what_went_unanswered() {
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(500))
        .answer_queries(false)
        .args(["-c", r"printf '\033[6n'; head -c 6 >/dev/null; echo never"])
        .spawn("/bin/sh")
        .unwrap();
    let err = t.wait_until(|s| s.contains("never")).unwrap_err();
    assert!(matches!(err, Error::Timeout { .. }));
    let msg = err.to_string();
    assert!(msg.contains("^[[6n"), "query not named in: {msg}");
}

#[test]
fn replies_are_not_echoed_into_the_screen() -> termlens::Result<()> {
    // The reply travels the input path; unless the app prints it, it must
    // never appear in the grid. `stty -echo` keeps the line discipline
    // from echoing what the "terminal" typed back.
    let mut t = sh(concat!(
        r"stty -icanon -echo; printf 'before\033[5n'; ",
        r"head -c 4 >/dev/null; ",
        r"printf ' after'; read guard"
    ))?;
    t.wait_until(|s| s.contains("before after"))?;
    assert!(
        !t.screen().text().contains("[0n"),
        "reply leaked into the grid:\n{}",
        t.screen()
    );
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}
