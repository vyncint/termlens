//! The query responder: capability-probing apps get real answers instead
//! of hanging, and whatever stays unanswered is named in timeout errors.
//!
//! Each program here genuinely BLOCKS on the terminal's reply — `--read N`
//! reads exactly the reply bytes, in raw mode so the line discipline
//! neither echoes them nor holds them for a newline — then prints a marker
//! the test waits for. The marker appearing proves the app was unblocked,
//! and the reply is on the grid with ESC drawn as `E` and BEL as `G`.

use std::time::Duration;

use termlens::{Error, Key, Terminal};

mod common;

/// The `emit` fixture with the timeout the test names; steps are documented
/// in `fixtures/emit/src/main.rs`.
fn emit(timeout: Duration, steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(Terminal::builder().timeout(timeout), steps)
}

/// A program that asks and reads the answer, ten seconds to do it in.
fn probe(steps: &[&str]) -> termlens::Result<Terminal> {
    emit(Duration::from_secs(10), steps)
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn cursor_position_reports_the_position_at_the_query() -> termlens::Result<()> {
    // After printing "abc" the cursor sits at row 1, col 4 (1-based on the
    // wire); the CPR reply is exactly 6 bytes: ESC [ 1 ; 4 R.
    let mut t = probe(&[
        "--raw-mode",
        "abc",
        "--csi",
        "6n",
        "\nunblocked:",
        "--read",
        "6",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("unblocked:E[1;4R"))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn device_attribute_probes_are_unblocked() -> termlens::Result<()> {
    // DA1 reply is ESC [ ? 6 2 ; 2 2 c = 9 bytes. This is also the exact
    // pattern kitty-protocol probes rely on: the DA1 answer arriving tells
    // the app "no kitty support", exactly like a real non-kitty terminal.
    let mut t = probe(&[
        "--raw-mode",
        "--csi",
        "c",
        "unblocked:",
        "--read",
        "9",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("unblocked:E[?62;22c"))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn background_color_query_gets_the_configured_answer() -> termlens::Result<()> {
    // OSC 11 reply: ESC ] 1 1 ; rgb:1e1e/1e1e/2e2e BEL = 24 bytes.
    let mut t = common::spawn_emit(
        Terminal::builder()
            .timeout(Duration::from_secs(10))
            .background_rgb(0x1e, 0x1e, 0x2e),
        &[
            "--raw-mode",
            "--raw",
            r"\e]11;?\a",
            "unblocked:",
            "--read",
            "24",
            "--wait",
        ],
    )?;
    t.wait_until(|s| s.contains("unblocked:E]11;rgb:1e1e/1e1e/2e2eG"))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn foreground_color_query_gets_the_configured_answer() -> termlens::Result<()> {
    // OSC 10 reply: ESC ] 1 0 ; rgb:cdcd/d6d6/f4f4 BEL = 24 bytes.
    let mut t = common::spawn_emit(
        Terminal::builder()
            .timeout(Duration::from_secs(10))
            .foreground_rgb(0xcd, 0xd6, 0xf4),
        &[
            "--raw-mode",
            "--raw",
            r"\e]10;?\a",
            "unblocked:",
            "--read",
            "24",
            "--wait",
        ],
    )?;
    t.wait_until(|s| s.contains("unblocked:E]10;rgb:cdcd/d6d6/f4f4G"))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn text_area_size_reports_the_real_grid() -> termlens::Result<()> {
    // XTWINOPS 18 reply: ESC [ 8 ; 24 ; 80 t = 10 bytes.
    let mut t = probe(&[
        "--raw-mode",
        "--csi",
        "18t",
        "unblocked:",
        "--read",
        "10",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("unblocked:E[8;24;80t"))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn unanswerable_queries_turn_timeouts_into_diagnoses() {
    // CSI 14 t (pixel size) is recognized as a question termlens cannot
    // answer; the app blocks, and the timeout error names the query.
    let mut t = emit(
        Duration::from_millis(500),
        &["--csi", "14t", "--wait", "never"],
    )
    .unwrap();
    let err = t.wait_until(|s| s.contains("never")).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("^[[14t"), "query not named in: {msg}");
    assert!(msg.contains("received no answer"), "no diagnosis in: {msg}");
    // Drop kills the blocked child.
}

#[test]
fn the_responder_can_be_disabled_and_says_what_went_unanswered() {
    let mut t = common::spawn_emit(
        Terminal::builder()
            .timeout(Duration::from_millis(500))
            .answer_queries(false),
        &["--csi", "6n", "--wait", "never"],
    )
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
    let mut t = emit(
        Duration::from_millis(400),
        // Probes kitty (deliberately unanswered), does NOT block on a
        // reply, prints, then sits in a normal read. The pause forces the
        // output into a *later read* than the probe — output batched into
        // the same write is deliberately not treated as progress, since
        // the emulator stops at the query byte and consumes the rest of
        // that same chunk regardless of what the application is doing.
        &["--csi", "?u", "--sleep", "200ms", "ready\n", "--wait"],
    )
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
    let mut t = emit(
        Duration::from_millis(400),
        &["--raw", r"\e[?u\e[14t", "--wait", "never"],
    )
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
    let mut t = emit(
        Duration::from_millis(400),
        &["--csi", "14t", "--wait", "never"],
    )
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
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn an_app_that_probes_for_synchronized_output_gets_it() -> termlens::Result<()> {
    let mut t = probe(&[
        // Ask "is mode 2026 supported?" and put the DECRPM reply on row 1,
        // where the frame's `CSI H` will not paint over it.
        "--raw-mode",
        "--csi",
        "?2026$p",
        "\nreply:",
        "--read",
        "11",
        // Then bracket the repaint, exactly as a real app would.
        "--raw",
        r"\e[?2026h\e[HPROBED FRAME\e[?2026l",
        "--wait",
    ])?;

    // A terminal that does not recognize the mode answers `;0$y`; the
    // frame is asserted together with the reply that licensed it, so a
    // regression to "unrecognized" fails here rather than passing quietly.
    t.wait_frame(|s| s.contains("PROBED FRAME") && s.contains("reply:E[?2026;2$y"))?;
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The reply must be truthful, not merely present: a mode we do not
/// track exactly is reported as "not recognized" rather than guessed.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn mode_reports_are_truthful() -> termlens::Result<()> {
    let mut t = probe(&[
        "--raw-mode",
        "--csi",
        "?2004h",
        // 2004 was just set -> `;1$y`; 1 (DECCKM) is untouched -> `;2$y`;
        // 12 (cursor blink) is not tracked at all -> `;0$y`.
        "--raw",
        r"\e[?2004$p\e[?1$p\e[?12$p",
        // The three replies are 11 + 8 + 9 = 28 bytes:
        // ESC[?2004;1$y  ESC[?1;2$y  ESC[?12;0$y
        "got:",
        "--read",
        "28",
        " DONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let row = t.screen().row_text(0);
    assert!(row.contains("E[?2004;1$y"), "2004 should be set: {row}");
    assert!(row.contains("E[?1;2$y"), "DECCKM should be reset: {row}");
    assert!(row.contains("E[?12;0$y"), "12 is not tracked: {row}");

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The families we recognize but cannot answer are now named in the
/// timeout instead of hanging silently.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn decrqss_and_palette_queries_are_named() {
    for (label, query, shape) in [
        ("DECRQSS", r"\eP$qm\e\\", "^[P$qm"),
        ("OSC 4", r"\e]4;1;?\a", "^[]4;1;?"),
    ] {
        let mut t = emit(
            Duration::from_millis(400),
            &["--raw", query, "--wait", "never"],
        )
        .unwrap();
        let err = t.wait_until(|s| s.contains("never")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(shape), "{label} not named in: {msg}");
    }
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn replies_are_not_echoed_into_the_screen() -> termlens::Result<()> {
    // The reply travels the input path; unless the app prints it, it must
    // never appear in the grid. Raw mode keeps the line discipline from
    // echoing what the "terminal" typed back, and `--skip` reads the reply
    // without printing it.
    let mut t = probe(&[
        "--raw-mode",
        "before",
        "--csi",
        "5n",
        "--skip",
        "4",
        " after",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("before after"))?;
    assert!(
        !t.screen().text().contains("[0n"),
        "reply leaked into the grid:\n{}",
        t.screen()
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn a_probe_then_enable_application_gets_its_mouse() -> termlens::Result<()> {
    // The loop this closes on itself: the application probes `?1000$p`, is
    // told "not recognized", concludes the terminal has no mouse and never
    // sends `CSI ?1000h` — and `click` then refuses, blaming the
    // application for a decision termlens caused.
    //
    // The program prints the reply it got before it enables tracking, and
    // the wait below asserts the reply *and* the marker together: a
    // regression to "unrecognized" shows up as `;0$y` on the grid and the
    // wait fails, rather than passing quietly.
    let mut t = probe(&[
        "--raw-mode",
        "--csi",
        "?1000$p",
        "MOUSE-ON:",
        "--read",
        "11",
        "|",
        "--raw",
        r"\e[?1000h\e[?1006h",
        "CLICK:",
        "--read",
        "20",
        "--wait",
    ])?;
    // `;2$y` = implemented and currently reset. The application proceeds —
    // and the wait covers the enable that follows the marker, since the
    // click below is refused until the terminal has seen `CSI ?1000 h`.
    t.wait_until(|s| {
        s.contains("MOUSE-ON:E[?1000;2$y|") && s.mouse_mode() != termlens::MouseMode::None
    })?;

    t.click(9, 4)?;
    // Press and release, SGR-encoded, 1-based on the wire.
    t.wait_until(|s| s.contains("CLICK:E[<0;10;5ME[<0;10;5m"))?;

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Each mouse tracking mode is answered on its own evidence now that the
/// tracker keeps the set the application asked for (#151). Before, a probe
/// for a member other than the last one enabled — which is every probe
/// after crossterm's three-at-once enable — was answered "not recognized".
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn decrqm_answers_each_mouse_tracking_mode_on_its_own() -> termlens::Result<()> {
    // Reply values: 1 = set, 2 = reset, 0 = not recognized. The reply is
    // `ESC [ ? <mode> ; <value> $ y`, so its length follows the mode's.
    for (sequence, reply_len, expect, label) in [
        (
            r"\e[?1000h\e[?1002h\e[?1003h\e[?1002$p",
            "11",
            "[?1002;1$y",
            "a member other than the last enabled is set",
        ),
        (
            r"\e[?1000h\e[?1002h\e[?1003h\e[?9$p",
            "8",
            "[?9;2$y",
            "a member never asked for is reset, not unrecognized",
        ),
        (
            r"\e[?1000h\e[?1002h\e[?1003h\e[?1003l\e[?1003$p",
            "11",
            "[?1003;2$y",
            "a released member is reset while the others stay",
        ),
        (
            r"\e[?1000h\e[?1002h\e[?1003h\e[?1003l\e[?1002$p",
            "11",
            "[?1002;1$y",
            "and the others do stay set",
        ),
    ] {
        let mut t = common::spawn_emit(
            Terminal::builder()
                .size(80, 6)
                .timeout(Duration::from_secs(10)),
            &[
                "--raw-mode",
                "--raw",
                sequence,
                "--read",
                reply_len,
                " DONE",
                "--wait",
            ],
        )?;
        t.wait_until(|s| s.contains("DONE"))?;
        let row = t.screen().row_text(0);
        assert!(row.contains(expect), "{label}: got {row:?}");
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success());
    }
    Ok(())
}

/// Mode 1004 is now answerable, because termlens tracks it exactly — the
/// honesty rule's precondition. Before, an application probing for focus
/// support was told "not recognized" even right after enabling it.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn decrqm_answers_for_focus_reporting() -> termlens::Result<()> {
    // Reply values: 1 = set, 2 = reset, 0 = not recognized.
    for (sequence, expect, label) in [
        (r"\e[?1004$p", ";2$y", "reset before the app enables it"),
        (r"\e[?1004h\e[?1004$p", ";1$y", "set after enabling"),
        (
            r"\e[?1004h\e[?1004l\e[?1004$p",
            ";2$y",
            "reset again after disabling",
        ),
    ] {
        let mut t = common::spawn_emit(
            Terminal::builder()
                .size(80, 6)
                .timeout(Duration::from_secs(10)),
            &[
                "--raw-mode",
                "--raw",
                sequence,
                "--read",
                "11",
                " DONE",
                "--wait",
            ],
        )?;
        t.wait_until(|s| s.contains("DONE"))?;
        let row = t.screen().row_text(0);
        assert!(row.contains(expect), "{label}: got {row:?}");
        assert!(
            !row.contains(";0$y"),
            "{label}: must not report unrecognized"
        );
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success());
    }
    Ok(())
}

/// Pixel geometry: unset it stays unanswered and named, set it makes the
/// two escape replies and `TIOCGWINSZ` agree instead of contradicting.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn cell_size_answers_the_pixel_reports_and_the_ioctl() -> termlens::Result<()> {
    // Unset: no reply, and the query is named in the next timeout.
    let mut mute = common::spawn_emit(
        Terminal::builder()
            .size(80, 6)
            .timeout(Duration::from_millis(700)),
        &["--csi", "16t", "MARK", "--wait"],
    )?;
    let err = mute
        .wait_until(|s| s.contains("NEVER"))
        .expect_err("must time out");
    assert!(err.to_string().contains("^[[16t"), "named: {err}");
    mute.send(Key::Enter)?;

    // Declared: both reports answer from it. `--read-quiet` returns on a
    // 2s timer instead of blocking on a byte count guessed wrong.
    let mut t = common::spawn_emit(
        Terminal::builder()
            .size(80, 24)
            .cell_size(10, 20)
            .timeout(Duration::from_secs(10)),
        &[
            "--raw-mode",
            "--csi",
            "16t",
            "cell[",
            "--read-quiet",
            "32",
            "] win[",
            "--csi",
            "14t",
            "--read-quiet",
            "32",
            "] DONE",
            "--wait",
        ],
    )?;
    t.wait_until(|s| s.contains("DONE"))?;
    let row = t.screen().row_text(0);
    // CSI 6 ; height ; width t   and   CSI 4 ; rows*h ; cols*w t
    assert!(row.contains("cell[E[6;20;10t]"), "{row:?}");
    assert!(row.contains("win[E[4;480;800t]"), "{row:?}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The ioctl must agree with the escape replies, and a resize must move
/// both — otherwise an application gets two different answers to the same
/// question depending on how it asks.
#[test]
#[cfg_attr(windows, ignore = "TIOCGWINSZ is a Unix ioctl")]
fn tiocgwinsz_agrees_with_the_declared_cell_size() -> termlens::Result<()> {
    let mut t = common::spawn_emit(
        Terminal::builder()
            .size(80, 24)
            .cell_size(10, 20)
            .timeout(Duration::from_secs(20)),
        &[
            "--winsize",
            "NL",
            "--wait",
            "--winsize",
            "NL",
            "DONE",
            "--wait",
        ],
    )?;
    t.wait_until(|s| s.contains("px"))?;
    assert!(
        t.screen().contains("80x24 px 800x480"),
        "the ioctl must carry the declared geometry:\n{}",
        t.screen()
    );

    t.resize(40, 12)?;
    t.send(Key::Enter)?;
    t.wait_until(|s| s.contains("DONE"))?;
    assert!(
        t.screen().contains("40x12 px 400x240"),
        "a resize must recompute it:\n{}",
        t.screen()
    );

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Declaring graphics support is how an application that probes first can
/// reach its pixel path at all. The default claims nothing, which is what
/// makes the declaration meaningful.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn declared_graphics_support_reaches_the_probe() -> termlens::Result<()> {
    // Default: DA1 has no `4`, and the kitty probe goes unanswered.
    let mut plain = common::spawn_emit(
        Terminal::builder()
            .size(80, 6)
            .timeout(Duration::from_secs(10)),
        &["--raw-mode", "--csi", "c", "--read", "9", " DONE", "--wait"],
    )?;
    plain.wait_until(|s| s.contains("DONE"))?;
    let row = plain.screen().row_text(0);
    assert!(
        row.contains("[?62;22c"),
        "nothing claimed by default: {row:?}"
    );
    assert!(!row.contains(";4;"), "no sixel by default: {row:?}");
    plain.send(Key::Enter)?;

    // Sixel declared: DA1 gains `4`, so a probing application sees it.
    let mut sixel = common::spawn_emit(
        Terminal::builder()
            .size(80, 6)
            .graphics(termlens::Graphics::Sixel)
            .timeout(Duration::from_secs(10)),
        &[
            "--raw-mode",
            "--csi",
            "c",
            "--read",
            "11",
            " DONE",
            "--wait",
        ],
    )?;
    sixel.wait_until(|s| s.contains("DONE"))?;
    assert!(
        sixel.screen().row_text(0).contains("[?62;4;22c"),
        "{:?}",
        sixel.screen().row_text(0)
    );
    sixel.send(Key::Enter)?;

    // Kitty declared: the a=q probe is answered OK, echoing the id.
    let mut kitty = common::spawn_emit(
        Terminal::builder()
            .size(80, 6)
            .graphics(termlens::Graphics::Kitty)
            .timeout(Duration::from_secs(10)),
        &[
            "--raw-mode",
            "--raw",
            r"\e_Gi=7,a=q;\e\\",
            "--read",
            "9",
            " DONE",
            "--wait",
        ],
    )?;
    kitty.wait_until(|s| s.contains("DONE"))?;
    assert!(
        kitty.screen().row_text(0).contains("_Gi=7;OK"),
        "the reply echoes the id the probe named: {:?}",
        kitty.screen().row_text(0)
    );
    kitty.send(Key::Enter)?;
    Ok(())
}

/// XTGETTCAP was the last of the common startup probes with no reply. Both
/// halves matter: a known capability is answered truthfully, and an unknown
/// one is *explicitly* declined — which is what turns a hang into a decision.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn xtgettcap_answers_what_it_knows_and_declines_the_rest() -> termlens::Result<()> {
    // TN=544e, colors=636f6c6f7273, and a made-up name that must be refused.
    // Wide enough that the three replies stay on one row.
    let mut t = common::spawn_emit(
        Terminal::builder()
            .size(200, 6)
            .timeout(Duration::from_secs(10)),
        &[
            "--raw-mode",
            "--raw",
            r"\eP+q544e;636f6c6f7273;7a7a7a7a\e\\",
            "--read-quiet",
            "200",
            " DONE",
            "--wait",
        ],
    )?;
    t.wait_until(|s| s.contains("DONE"))?;
    let text = t.screen().text();

    // TN -> "xterm-256color", hex 787465726d2d323536636f6c6f72
    assert!(
        text.contains("P1+r544e=787465726d2d323536636f6c6f72"),
        "TN must report the TERM the child was given:\n{text}"
    );
    // colors -> "256", hex 323536
    assert!(
        text.contains("P1+r636f6c6f7273=323536"),
        "colors must be answered:\n{text}"
    );
    // An unknown capability is declined with status 0, not ignored.
    assert!(
        text.contains("P0+r7a7a7a7a"),
        "an unknown capability must be explicitly refused:\n{text}"
    );

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// `TN` reports whatever `TERM` the child was actually given, so an
/// application cannot get two different answers to "which terminal is this?".
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn xtgettcap_tn_follows_the_configured_term() -> termlens::Result<()> {
    let mut t = common::spawn_emit(
        Terminal::builder()
            .size(120, 6)
            .env("TERM", "xterm")
            .timeout(Duration::from_secs(10)),
        &[
            "--raw-mode",
            "--raw",
            r"\eP+q544e\e\\",
            "--read-quiet",
            "80",
            " term=",
            "--env",
            "TERM",
            " DONE",
            "--wait",
        ],
    )?;
    t.wait_until(|s| s.contains("DONE"))?;
    let text = t.screen().text();
    // "xterm" is hex 787465726d
    assert!(text.contains("P1+r544e=787465726d"), "{text}");
    assert!(text.contains("term=xterm"), "{text}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A key capability must be the bytes termlens actually sends, or an
/// application that reads it and then matches input against it will not
/// match what arrives.
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY answers or eats the child's queries itself, so they never reach the responder (#149)"
)]
fn xtgettcap_key_capabilities_match_what_send_emits() -> termlens::Result<()> {
    // kcuu1 = 6b63757531; the value must be ESC [ A = 1b5b41.
    let mut t = common::spawn_emit(
        Terminal::builder()
            .size(120, 6)
            .timeout(Duration::from_secs(10)),
        &[
            "--raw-mode",
            "--raw",
            r"\eP+q6b63757531\e\\",
            "--read-quiet",
            "80",
            " DONE",
            "--wait",
        ],
    )?;
    t.wait_until(|s| s.contains("DONE"))?;
    let text = t.screen().text();
    assert!(text.contains("P1+r6b63757531=1b5b41"), "{text}");
    // And that is exactly what Key::Up encodes to in default mode.
    assert_eq!(termlens::Key::Up.encode(), b"\x1b[A");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
