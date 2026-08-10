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
    let mut first = sh(&list_with_highlight(0))?;
    first.wait_until(|s| s.contains("item two"))?;
    let a = first.screen();
    first.send(Key::Enter);
    first.wait_exit()?;

    let mut second = sh(&list_with_highlight(1))?;
    second.wait_until(|s| s.contains("item two"))?;
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
    t.wait_until(|s| s.contains("selected"))?;
    insta::assert_snapshot!(t.screen().with_styles());
    t.send(Key::Enter);
    assert!(t.wait_exit()?.success());
    Ok(())
}
