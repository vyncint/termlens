//! DEC Special Graphics: the line-drawing character set ncurses borders are
//! made of. On an xterm terminfo `smacs`/`rmacs` are `ESC ( 0` / `ESC ( B`;
//! on a vt100 one the set is designated into G1 once and invoked with
//! `SO`/`SI`. Either way a user sees `┌───┐`, and so must the grid — before
//! this, a snapshot blessed `lqqqk` and went on passing while the border was
//! broken, because the letters were what it had recorded.

use std::time::Duration;

use termlens::{Color, Key, Terminal};

mod common;

/// The `emit` fixture on a 40x6 terminal. One `--raw` per line the test
/// draws, with `\e` for ESC, `\x0e`/`\x0f` for SO/SI; steps are documented
/// in `fixtures/emit/src/main.rs`.
fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(
        Terminal::builder()
            .size(40, 6)
            .timeout(Duration::from_secs(10)),
        steps,
    )
}

/// The reproduction from the issue, as a whole frame.
#[test]
fn an_ncurses_style_border_reads_as_box_drawing() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e(0lqqqk\e(B\n",
        "--raw",
        r"\e(0x\e(B in \e(0x\e(B\n",
        "--raw",
        r"\e(0mqqqj\e(B\n",
        "DONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "┌───┐", "{s}");
    assert_eq!(s.row_text(1).trim_end(), "│ in │", "{s}");
    assert_eq!(s.row_text(2).trim_end(), "└───┘", "{s}");
    // One cell per glyph, so coordinates are the user's.
    assert_eq!(s.find("┐"), Some((0, 4)));
    assert!(
        !s.contains("lqqqk"),
        "the letters must not reach the grid:\n{s}"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The vt100-terminfo shape: `enacs` designates the set into G1 once, then
/// `smacs`/`rmacs` are the locking shifts SO and SI.
#[test]
fn shift_out_and_shift_in_select_the_designated_set() -> termlens::Result<()> {
    let mut t = emit(&["--raw", r"\e)0\x0elqk\x0f lqk \x0ex\x0f", "DONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "┌─┐ lqk │DONE", "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A colour set inside the graphics set travels with the glyph. The
/// attribute shadow follows the same translated stream, so its
/// correspondence check — run on every snapshot — is what proves the two
/// grids stayed the same shape through the rewrite.
#[test]
fn a_styled_border_keeps_its_style() -> termlens::Result<()> {
    let mut t = emit(&["--raw", r"\e(0\e[31mqqq\e[0m\e(B end", "DONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "─── endDONE", "{s}");
    for col in 0..3 {
        let cell = s.cell(0, col).expect("on screen");
        assert_eq!(cell.contents(), "─");
        assert_eq!(cell.style().fg, Color::Indexed(1), "col {col}: {s}");
    }
    assert_eq!(s.cell(0, 4).unwrap().style().fg, Color::Default);
    assert!(
        s.with_styles().to_string().contains("0: 0-2 fg=1"),
        "{}",
        s.with_styles()
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A hard reset returns both sets to ASCII — and clears the screen, as RIS
/// does, so the glyph drawn before it is gone and the byte after it is a
/// letter again. It clears the `DECSC` slot too: a `DECRC` after the reset
/// must not resurrect a designation from before it (#232).
#[test]
fn a_hard_reset_returns_to_ascii() -> termlens::Result<()> {
    let mut t = emit(&["--raw", r"\e(0q\e7\ec\e8q", "DONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "qDONE", "{s}");
    assert!(!s.contains("─"), "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A soft reset (`DECSTR`, `CSI ! p`) returns the sets to ASCII the way
/// `RIS` does — and unlike `RIS` clears nothing, so the glyph drawn before
/// it stays. It used to parse cleanly and do nothing, so text printed after
/// an application's teardown reset kept rendering as box drawing (#233).
#[test]
fn a_soft_reset_returns_to_ascii_without_clearing_the_screen() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e(0q\e[!pq\n",
        "--raw",
        r"\e(0\e7\e[!p\e8lqk\n",
        "DONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "─q", "{s}");
    // Row 1 draws after a DECRC: the soft reset emptied the saved slot, so
    // the restore returns to ASCII rather than to the graphics set saved
    // before it. (The cursor it restores is the row's start, where it was.)
    assert_eq!(s.row_text(1).trim_end(), "lqk", "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The save/jump/draw/restore idiom: `DECSC` saves the designated sets and
/// the locking shift with the cursor, `DECRC` brings them back. The
/// restore used to leave whatever was designated in between, so a border
/// drawn after it read `lqk` (#232). Row 1 restores an `SO` state, since
/// the shift is part of what is saved.
#[test]
fn decsc_and_decrc_save_and_restore_the_charset_state() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e(0\e7\e(B\e8lqk\e(B\n",
        "--raw",
        r"\e)0\x0e\e7\x0f\e8lqk\x0f\n",
        "DONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "┌─┐", "{s}");
    assert_eq!(s.row_text(1).trim_end(), "┌─┐", "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// `DECRC` with nothing saved restores the defaults, as xterm does — G0 at
/// ASCII — rather than leaving whatever was last designated.
#[test]
fn decrc_with_nothing_saved_returns_to_ascii() -> termlens::Result<()> {
    let mut t = emit(&["--raw", r"\e(0\e8lqk", "DONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "lqkDONE", "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// SS2/SS3 invoke G2/G3 for one character, then the locking shift resumes.
/// `|` is itself a Special Graphics byte (`≠`); a shift that stuck would
/// translate it, which is worse than never shifting.
#[test]
fn ss2_and_ss3_invoke_g2_g3_for_one_character() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e*0\eNl\e(B|\n",
        "--raw",
        r"\e+0\eOl\e(B|\n",
        "DONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "┌|", "{s}");
    assert_eq!(s.row_text(1).trim_end(), "┌|", "{s}");
    assert!(
        !s.contains("l|") && !s.contains("≠"),
        "the letter and a stuck shift's ≠ must not reach the grid:\n{s}"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A single shift overrides SO for one character without leaving G1.
#[test]
fn a_single_shift_overrides_so_for_one_character() -> termlens::Result<()> {
    let mut t = emit(&["--raw", r"\e)0\e*B\x0e\eNlqk\x0f", "DONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "l─┐DONE", "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A single shift that is followed by RIS does not survive it. RIS also
/// returns G2 to ASCII, so redesignating without a new shift stays a letter.
#[test]
fn a_pending_single_shift_does_not_survive_ris() -> termlens::Result<()> {
    let mut t = emit(&["--raw", r"\e*0\eN\ec\e*0l", "DONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "lDONE", "{s}");
    assert!(!s.contains("┌"), "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A single shift lasts for one *character*: CJK, emoji and `é` consume
/// it, so a graphics byte after them stays a letter. The old GL-only
/// consume left the shift pending and turned that `l` into `┌`.
#[test]
fn a_multibyte_character_consumes_a_single_shift() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e*0\eN汉l\n",
        "--raw",
        r"\e*0\eN🦀l\n",
        "--raw",
        r"\e*0\eNél\n",
        "DONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "汉l", "{s}");
    assert_eq!(s.row_text(1).trim_end(), "🦀l", "{s}");
    assert_eq!(s.row_text(2).trim_end(), "él", "{s}");
    assert!(
        !s.contains("┌"),
        "the shift must not survive the multi-byte character:\n{s}"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The UK set (`ESC ( A`) differs from ASCII in one position: `#` draws
/// `£`. The designation used to be consumed and the byte drawn as itself,
/// so a price in the UK set read `#42` and a test asserting `£42` failed
/// against an application that was correct (#234). `SO`/`SI` select it the
/// way they select the graphics set.
#[test]
fn the_uk_set_draws_a_pound_sign_at_hash() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e(A#42 a-z\e(B#\n",
        "--raw",
        r"\e)A\x0e#\x0f#\n",
        "DONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "£42 a-z#", "{s}");
    assert_eq!(s.row_text(1).trim_end(), "£#", "{s}");
    assert_eq!(s.find("£"), Some((0, 0)), "one cell, one column: {s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
