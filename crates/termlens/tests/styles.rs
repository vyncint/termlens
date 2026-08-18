//! `with_styles()`: style-only regressions become visible snapshot diffs.

use std::time::Duration;

use termlens::{Key, Terminal};

fn sh(script: &str) -> termlens::Result<Terminal> {
    Terminal::builder()
        .timeout(Duration::from_secs(10))
        .args(["-c", script])
        .spawn("/bin/sh")
}

/// Print a two-item list with `reverse` on the item given by $1.
fn list_with_highlight(row: u16) -> String {
    let (one, two) = match row {
        0 => (r"\033[7mitem one\033[0m", "item two"),
        _ => ("item one", r"\033[7mitem two\033[0m"),
    };
    format!(r"printf '{one}\n{two}\n'; read guard")
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

    let mut first = sh(&list_with_highlight(0))?;
    first.wait_until(settled)?;
    let a = first.screen();
    first.send(Key::Enter);
    first.wait_exit()?;

    let mut second = sh(&list_with_highlight(1))?;
    second.wait_until(settled)?;
    let b = second.screen();
    second.send(Key::Enter);
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
    let mut t = sh(concat!(
        r"printf '\033[1;31mERROR\033[0m plain \033[4;34munderlined\033[0m\n'; ",
        r"printf 'second row \033[7mselected\033[0m\n'; read guard"
    ))?;
    // Same trailing-newline race as above, and a snapshot embeds the cursor:
    // caught by the stress gate at iteration 46 of 100 as `cursor: 1,19`
    // against the recorded `cursor: 2,0`.
    t.wait_until(|s| s.contains("selected") && s.cursor() == (2, 0, true))?;
    insta::assert_snapshot!(t.screen().with_styles());
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The trap this exists to close. A test asserting that a password field is
/// masked used to pass just as happily against an application that printed
/// the secret in clear, because `SGR 8` reached nothing and the two
/// renderings were identical in the grid — the one failure mode where a
/// green test certifies the bug it was written to catch.
#[test]
fn a_masked_field_is_distinguishable_from_clear_text() -> termlens::Result<()> {
    // The cursor is pinned for the same reason, plus one specific to this
    // test: the styled comparison below asserts a *difference*, so an
    // incidental cursor difference would let it pass without the styles
    // differing at all — passing for the wrong reason.
    let settled = |s: &termlens::Screen| s.contains("pw: hunter2|") && s.cursor() == (0, 12, true);

    let mut masked = sh(r"printf 'pw: \033[8mhunter2\033[28m|'; read guard")?;
    masked.wait_until(settled)?;
    let a = masked.screen();
    masked.send(Key::Enter);
    masked.wait_exit()?;

    let mut clear = sh(r"printf 'pw: hunter2|'; read guard")?;
    clear.wait_until(settled)?;
    let b = clear.screen();
    clear.send(Key::Enter);
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
    let mut t = sh(concat!(
        r"printf 'done \033[9mship it\033[29m ",
        r"\033[5;31moverdue\033[0m plain'; read guard"
    ))?;
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

    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}
