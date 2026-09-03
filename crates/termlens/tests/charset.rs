//! DEC Special Graphics: the line-drawing character set ncurses borders are
//! made of. On an xterm terminfo `smacs`/`rmacs` are `ESC ( 0` / `ESC ( B`;
//! on a vt100 one the set is designated into G1 once and invoked with
//! `SO`/`SI`. Either way a user sees `┌───┐`, and so must the grid — before
//! this, a snapshot blessed `lqqqk` and went on passing while the border was
//! broken, because the letters were what it had recorded.

use std::time::Duration;

use termlens::{Color, Key, Terminal};

fn sh(script: &str) -> termlens::Result<Terminal> {
    Terminal::builder()
        .size(40, 6)
        .timeout(Duration::from_secs(10))
        .args(["-c", script])
        .spawn("/bin/sh")
}

/// The reproduction from the issue, as a whole frame.
#[test]
fn an_ncurses_style_border_reads_as_box_drawing() -> termlens::Result<()> {
    let mut t = sh(concat!(
        r"printf '\033(0lqqqk\033(B\n'; ",
        r"printf '\033(0x\033(B in \033(0x\033(B\n'; ",
        r"printf '\033(0mqqqj\033(B\n'; ",
        "printf DONE; read _"
    ))?;
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
    let mut t = sh(r"printf '\033)0\016lqk\017 lqk \016x\017'; printf DONE; read _")?;
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
    let mut t = sh(r"printf '\033(0\033[31mqqq\033[0m\033(B end'; printf DONE; read _")?;
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
/// letter again.
#[test]
fn a_hard_reset_returns_to_ascii() -> termlens::Result<()> {
    let mut t = sh(r"printf '\033(0q\033cq'; printf DONE; read _")?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "qDONE", "{s}");
    assert!(!s.contains("─"), "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// SS2/SS3 invoke G2/G3 for one character, then the locking shift resumes.
/// `|` is itself a Special Graphics byte (`≠`); a shift that stuck would
/// translate it, which is worse than never shifting.
#[test]
fn ss2_and_ss3_invoke_g2_g3_for_one_character() -> termlens::Result<()> {
    let mut t = sh(concat!(
        r"printf '\033*0\033Nl\033(B|\n'; ",
        r"printf '\033+0\033Ol\033(B|\n'; ",
        "printf DONE; read _"
    ))?;
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
    let mut t = sh(r"printf '\033)0\033*B\016\033Nlqk\017'; printf DONE; read _")?;
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
    let mut t = sh(r"printf '\033*0\033N\033c\033*0l'; printf DONE; read _")?;
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
    let mut t = sh(concat!(
        r"printf '\033*0\033N汉l\n'; ",
        r"printf '\033*0\033N🦀l\n'; ",
        r"printf '\033*0\033Nél\n'; ",
        "printf DONE; read _"
    ))?;
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
