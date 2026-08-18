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
fn foreground_color_query_gets_the_configured_answer() -> termlens::Result<()> {
    // OSC 10 reply: ESC ] 1 0 ; rgb:cdcd/d6d6/f4f4 BEL = 24 bytes.
    let mut t = Terminal::builder()
        .timeout(Duration::from_secs(10))
        .foreground_rgb(0xcd, 0xd6, 0xf4)
        .args([
            "-c",
            concat!(
                r"stty -icanon -echo; printf '\033]10;?\007'; ",
                r#"reply=$(head -c 24 | tr '\033\007' 'EG'); "#,
                r#"printf 'unblocked:%s' "$reply"; read guard"#
            ),
        ])
        .spawn("/bin/sh")?;
    t.wait_until(|s| s.contains("unblocked:E]10;rgb:cdcd/d6d6/f4f4G"))?;
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

/// The diagnosis must not outlive the situation it describes. An app
/// that probes, is answered nothing, and carries on producing output was
/// plainly not blocked on that probe — a later, unrelated timeout must
/// not blame it.
#[test]
fn a_query_the_app_moved_past_is_context_not_a_cause() {
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(400))
        // Probes kitty (deliberately unanswered), does NOT block on a
        // reply, prints, then sits in a normal read. The pause forces the
        // output into a *later read* than the probe — output batched into
        // the same write is deliberately not treated as progress, since
        // the emulator stops at the query byte and consumes the rest of
        // that same chunk regardless of what the application is doing.
        .args([
            "-c",
            r"printf '\033[?u'; sleep 0.2; printf 'ready\n'; read guard",
        ])
        .spawn("/bin/sh")
        .unwrap();
    t.wait_until(|s| s.contains("ready")).unwrap();

    let err = t.wait_until(|s| s.contains("never-appears")).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("^[[?u"),
        "the query is still worth naming: {msg}"
    );
    assert!(
        !msg.contains("this is the cause"),
        "the app moved past the probe — no causal claim belongs here: {msg}"
    );
    assert!(
        msg.contains("produced output afterwards"),
        "the note should say why it is only context: {msg}"
    );
}

/// Every unanswered query is named, not just the most recent one.
#[test]
fn all_unanswered_queries_are_named() {
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(400))
        .args([
            "-c",
            r"printf '\033[?u\033[14t'; head -c 4 >/dev/null; echo never",
        ])
        .spawn("/bin/sh")
        .unwrap();
    let err = t.wait_until(|s| s.contains("never")).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("^[[?u"), "first query missing from: {msg}");
    assert!(msg.contains("^[[14t"), "second query missing from: {msg}");
}

/// `wait_frame` used to build its message from its own strings and never
/// surface the note — the worst place to withhold it, since an app
/// blocked on a probe never reaches its first repaint and the message
/// then blames the app for not emitting frames.
#[test]
fn wait_frame_timeouts_carry_the_query_note() {
    let mut t = Terminal::builder()
        .timeout(Duration::from_millis(400))
        .args(["-c", r"printf '\033[14t'; head -c 4 >/dev/null; echo never"])
        .spawn("/bin/sh")
        .unwrap();
    let err = t.wait_frame(|s| s.contains("never")).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("^[[14t") && msg.contains("received no answer"),
        "wait_frame withheld the diagnosis: {msg}"
    );
}

/// The payoff of answering DECRQM: an application that *probes* before
/// using synchronized output can turn it on against termlens — so
/// `wait_frame` works against a program nobody modified for us.
#[test]
fn an_app_that_probes_for_synchronized_output_gets_it() -> termlens::Result<()> {
    let mut t = sh(concat!(
        // Ask "is mode 2026 supported?" and read the DECRPM reply.
        r"stty -icanon -echo; printf '\033[?2026$p'; ",
        r#"reply=$(head -c 10 | tr '\033' 'E'); "#,
        // A terminal that does not recognize the mode answers `;0$y`.
        r#"case "$reply" in *';0$y') printf 'unsupported'; read guard; exit 0 ;; esac; "#,
        // Recognized: bracket the repaint, exactly as a real app would.
        r"printf '\033[?2026h\033[HPROBED FRAME\033[?2026l'; read guard"
    ))?;

    t.wait_frame(|s| s.contains("PROBED FRAME"))?;
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The reply must be truthful, not merely present: a mode we do not
/// track exactly is reported as "not recognized" rather than guessed.
#[test]
fn mode_reports_are_truthful() -> termlens::Result<()> {
    let mut t = sh(concat!(
        r"stty -icanon -echo; printf '\033[?2004h'; ",
        // 2004 was just set -> `;1$y`; 1 (DECCKM) is untouched -> `;2$y`;
        // 12 (cursor blink) is not tracked at all -> `;0$y`.
        r"printf '\033[?2004$p\033[?1$p\033[?12$p'; ",
        // The three replies are 11 + 8 + 9 = 28 bytes:
        // ESC[?2004;1$y  ESC[?1;2$y  ESC[?12;0$y
        r#"reply=$(head -c 28 | tr '\033' 'E'); "#,
        r#"printf 'got:%s' "$reply"; read guard"#
    ))?;
    t.wait_until(|s| s.contains("got:"))?;
    let row = t.screen().row_text(0);
    assert!(row.contains("E[?2004;1$y"), "2004 should be set: {row}");
    assert!(row.contains("E[?1;2$y"), "DECCKM should be reset: {row}");
    assert!(row.contains("E[?12;0$y"), "12 is not tracked: {row}");

    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The families we recognize but cannot answer are now named in the
/// timeout instead of hanging silently.
#[test]
fn decrqss_and_palette_queries_are_named() {
    for (label, script, shape) in [
        (
            "DECRQSS",
            r#"printf '\033P$qm\033\\'; head -c 4 >/dev/null; echo never"#,
            "^[P$qm",
        ),
        (
            "OSC 4",
            r#"printf '\033]4;1;?\007'; head -c 4 >/dev/null; echo never"#,
            "^[]4;1;?",
        ),
    ] {
        let mut t = Terminal::builder()
            .timeout(Duration::from_millis(400))
            .args(["-c", script])
            .spawn("/bin/sh")
            .unwrap();
        let err = t.wait_until(|s| s.contains("never")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(shape), "{label} not named in: {msg}");
    }
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

#[test]
fn a_probe_then_enable_application_gets_its_mouse() -> termlens::Result<()> {
    // The loop this closes on itself: the application probes `?1000$p`, is
    // told "not recognized", concludes the terminal has no mouse and never
    // sends `CSI ?1000h` — and `click` then refuses, blaming the
    // application for a decision termlens caused.
    //
    // The script is written to *prove* the decision: if the reply says the
    // mode is unrecognized it prints REFUSED and never enables tracking,
    // so a regression fails on the wait below rather than passing quietly.
    let mut t = sh(concat!(
        r"stty -icanon -echo; ",
        r#"printf '\033[?1000$p'; "#,
        r#"reply=$(head -c 11 | tr '\033' 'E'); "#,
        r#"case "$reply" in *';0$y') printf 'REFUSED:%s' "$reply"; read g; exit 0;; esac; "#,
        r#"printf '\033[?1000h\033[?1006h'; printf 'MOUSE-ON:%s|' "$reply"; "#,
        r#"click=$(head -c 20 | tr '\033' 'E'); printf 'CLICK:%s' "$click"; read g"#
    ))?;
    // `;2$y` = implemented and currently reset. The application proceeds.
    t.wait_until(|s| s.contains("MOUSE-ON:E[?1000;2$y|"))?;

    t.click(9, 4)?;
    // Press and release, SGR-encoded, 1-based on the wire.
    t.wait_until(|s| s.contains("CLICK:E[<0;10;5ME[<0;10;5m"))?;

    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}
