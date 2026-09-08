//! `with_styles()`: style-only regressions become visible snapshot diffs.

use std::time::Duration;

use termlens::{Key, Terminal};

mod common;

/// The `emit` fixture; steps are documented in `fixtures/emit/src/main.rs`.
fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(Terminal::builder().timeout(Duration::from_secs(10)), steps)
}

/// A two-item list with `reverse` on the item given by `row`.
fn list_with_highlight(row: u16) -> [&'static str; 3] {
    let list = match row {
        0 => r"\e[7mitem one\e[0m\nitem two\n",
        _ => r"item one\n\e[7mitem two\e[0m\n",
    };
    ["--raw", list, "--wait"]
}

#[test]
fn moving_a_highlight_changes_the_styled_rendering_only() -> termlens::Result<()> {
    // Wait on the cursor as well as the text — rule 1, and the same idiom
    // `fixtures.rs` uses for the same reason. `contains("item two")` turns
    // true before the newline *after* it is processed, leaving the cursor at
    // (1, 8) instead of its resting (2, 0); that is state no predicate here
    // named but that `Display` renders, so comparing two whole snapshots
    // without pinning it is a race. Found by the stress gate at iteration
    // 17 of 100, with byte-identical grids and only the cursor differing.
    let settled = |s: &termlens::Screen| s.contains("item two") && s.cursor() == (2, 0, true);

    let mut first = emit(&list_with_highlight(0))?;
    first.wait_until(settled)?;
    let a = first.screen();
    first.send(Key::Enter)?;
    first.wait_exit()?;

    let mut second = emit(&list_with_highlight(1))?;
    second.wait_until(settled)?;
    let b = second.screen();
    second.send(Key::Enter)?;
    second.wait_exit()?;

    // The pinning scenario from the coverage study: identical text…
    assert_eq!(a.text(), b.text());
    // …identical plain snapshots…
    assert_eq!(a.to_string(), b.to_string());
    // …but the styled rendering sees the highlight move.
    assert_ne!(a.with_styles().to_string(), b.with_styles().to_string());
    assert!(a.with_styles().to_string().contains("0: 0-7 reverse"));
    assert!(b.with_styles().to_string().contains("1: 0-7 reverse"));
    Ok(())
}

#[test]
fn styled_screen_snapshot() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e[1;31mERROR\e[0m plain \e[4;34munderlined\e[0m\n",
        "--raw",
        r"second row \e[7mselected\e[0m\n",
        "--wait",
    ])?;
    // Same trailing-newline race as above, and a snapshot embeds the cursor:
    // caught by the stress gate at iteration 46 of 100 as `cursor: 1,19`
    // against the recorded `cursor: 2,0`.
    t.wait_until(|s| s.contains("selected") && s.cursor() == (2, 0, true))?;
    insta::assert_snapshot!(t.screen().with_styles());
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The trap this exists to close. A test asserting that a password field is
/// masked used to pass just as happily against an application that printed
/// the secret in clear, because `SGR 8` reached nothing and the two
/// renderings were identical in the grid — the one failure mode where a
/// green test certifies the bug it was written to catch.
#[test]
fn a_highlight_over_a_wide_character_is_one_span() -> termlens::Result<()> {
    // A wide character's continuation column used to snapshot unstyled, so
    // a bar over CJK or emoji rendered as two spans with a hole (#218).
    let mut t = emit(&["--raw", r"\e[48;2;30;30;46mab汉cd\e[0m", "--wait"])?;
    t.wait_until(|s| s.contains("cd"))?;
    let s = t.screen();
    let bar = termlens::Color::Rgb(30, 30, 46);
    let styled = s.with_styles().to_string();
    assert!(styled.contains("0: 0-5 bg=#1e1e2e"), "{styled}");
    assert_eq!(
        s.cell(0, 3).unwrap().style().bg,
        bar,
        "the continuation column"
    );
    assert_eq!(s.find_by(|c| c.style().bg == bar), Some((0, 0)));
    t.send(termlens::Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}

#[test]
fn a_masked_field_is_distinguishable_from_clear_text() -> termlens::Result<()> {
    // The cursor is pinned for the same reason, plus one specific to this
    // test: the styled comparison below asserts a *difference*, so an
    // incidental cursor difference would let it pass without the styles
    // differing at all — passing for the wrong reason.
    let settled = |s: &termlens::Screen| s.contains("pw: hunter2|") && s.cursor() == (0, 12, true);

    let mut masked = emit(&["--raw", r"pw: \e[8mhunter2\e[28m|", "--wait"])?;
    masked.wait_until(settled)?;
    let a = masked.screen();
    masked.send(Key::Enter)?;
    masked.wait_exit()?;

    let mut clear = emit(&["pw: hunter2|", "--wait"])?;
    clear.wait_until(settled)?;
    let b = clear.screen();
    clear.send(Key::Enter)?;
    clear.wait_exit()?;

    // Identical text — a real terminal holds the characters either way, and
    // so does termlens. That is why `text()` cannot tell them apart.
    assert_eq!(a.text(), b.text());

    // The assertion a test author actually wants, and could not write:
    let secret_is_masked =
        |s: &termlens::Screen| (4..11).all(|col| s.cell(0, col).is_some_and(|c| c.style().conceal));
    assert!(
        secret_is_masked(&a),
        "the field is masked:\n{}",
        a.with_styles()
    );
    assert!(
        !secret_is_masked(&b),
        "and clear text must fail the same assertion:\n{}",
        b.with_styles()
    );

    // The styled rendering separates them too, which is what makes a
    // snapshot test catch this.
    assert_ne!(a.with_styles().to_string(), b.with_styles().to_string());
    Ok(())
}

#[test]
fn strikethrough_and_blink_appear_in_the_styled_rendering() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"done \e[9mship it\e[29m \e[5;31moverdue\e[0m plain",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("plain"))?;
    let s = t.screen();
    let styled = s.with_styles().to_string();

    // Tokens are emitted in SGR order, so the existing ones keep their
    // places and the new ones slot in around `reverse`.
    assert!(styled.contains("strikethrough"), "{styled}");
    assert!(styled.contains("blink"), "{styled}");
    assert!(styled.contains("fg=1"), "{styled}");

    // A blinking red badge is no longer indistinguishable from a plain red
    // one — the tie `with_styles()` could not break.
    let overdue = s.find("overdue").expect("painted");
    let badge = *s.cell(overdue.0, overdue.1).unwrap().style();
    assert!(badge.blink && badge.fg == termlens::Color::Indexed(1));

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn colon_form_rgb_colours_match_semicolon_form() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e[38;2;10;20;30mA\e[0m\e[38:2::10:20:30mB\e[0m",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("AB"))?;
    let s = t.screen();

    let semicolon = s.find("A").expect("semicolon-form colour");
    let colon = s.find("B").expect("colon-form colour");
    assert_eq!(
        s.cell(semicolon.0, semicolon.1).unwrap().style().fg,
        termlens::Color::Rgb(10, 20, 30)
    );
    assert_eq!(
        s.cell(colon.0, colon.1).unwrap().style().fg,
        termlens::Color::Rgb(10, 20, 30)
    );

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn dim_and_italic_appear_without_shadow_collisions() -> termlens::Result<()> {
    let mut t = emit(&["--raw", r"\e[2mfaint\e[0m \e[3mslanted\e[0m\n", "--wait"])?;
    t.wait_until(|s| s.contains("slanted") && s.cursor() == (1, 0, true))?;
    let s = t.screen();

    let faint = s.find("faint").expect("dim run");
    let dim = *s.cell(faint.0, faint.1).unwrap().style();
    assert!(dim.dim && !dim.italic);

    let slanted = s.find("slanted").expect("italic run");
    let italic = *s.cell(slanted.0, slanted.1).unwrap().style();
    assert!(italic.italic && !italic.dim && !italic.conceal);

    // These words deliberately do not name the attributes, so the style
    // rendering cannot satisfy either assertion with visible text alone.
    let styled = s.with_styles().to_string();
    assert!(styled.contains("0: 0-4 dim"), "{styled}");
    assert!(styled.contains("6-12 italic"), "{styled}");

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
