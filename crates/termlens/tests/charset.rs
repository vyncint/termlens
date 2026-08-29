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
