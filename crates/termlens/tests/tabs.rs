//! Tab stops: `HTS`, `TBC`, `CHT`, `CBT` and the plain `HT` they redefine.
//! All four capabilities sit in the terminfo entry termlens hands every
//! child (`hts`, `tbc`, `cbt`), so an application that lays a table out by
//! setting its own stops and tabbing between them is using what it was told
//! it had. Before this the stops were the backend's hardcoded eight and the
//! four escapes vanished, so every column landed in the wrong place — and
//! silently, since the characters were all still on the screen.

use std::time::Duration;

use termlens::{Key, Terminal};

fn sh(script: &str) -> termlens::Result<Terminal> {
    Terminal::builder()
        .size(24, 4)
        .timeout(Duration::from_secs(10))
        .args(["-c", script])
        .spawn("/bin/sh")
}

/// Where a needle sits, which is the only thing any of these assert.
/// `contains` is deliberately not enough: the failure this file exists for
/// leaves every character present and every column wrong, so a column is
/// what has to be checked.
fn col_of(screen: &termlens::Screen, needle: &str) -> Option<u16> {
    screen.find(needle).map(|(_, col)| col)
}

/// The first reproduction from the issue: a stop set with `HTS` and reached
/// with a plain `\t`. The tab is the point — `HT` used to be answered by the
/// backend's fixed eight, so a stop set here and a tab taken there would
/// have disagreed even with `CHT` working.
#[test]
fn a_tab_lands_on_a_stop_set_by_hts() -> termlens::Result<()> {
    let mut t = sh(r"printf '\033[4G\033H\033[1Ga\011b'; printf ' DONE'; read _")?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(col_of(&s, "a"), Some(0), "{s}");
    assert_eq!(col_of(&s, "b"), Some(3), "the stop set at column 4:\n{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The default stops are every eighth column, and setting one adds to them
/// rather than replacing them.
#[test]
fn the_default_stops_survive_a_custom_one() -> termlens::Result<()> {
    let mut t = sh(r"printf '\033[4G\033H\033[1Ga\011b\011c'; printf ' DONE'; read _")?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(col_of(&s, "b"), Some(3), "{s}");
    assert_eq!(col_of(&s, "c"), Some(8), "the default eighth column:\n{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// `CSI g` clears the stop under the cursor, and the tab that used to land
/// on it runs on to the next one.
#[test]
fn tbc_clears_the_stop_under_the_cursor() -> termlens::Result<()> {
    // Standing on the default stop at column 9 (one-based), clear it: the
    // tab from column 1 runs past it to the next, at column 17.
    let mut t = sh(r"printf '\033[9G\033[g\033[1Ga\011b'; printf ' DONE'; read _")?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(col_of(&s, "b"), Some(16), "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// `CSI 3 g` — the form terminfo's `tbc` is written as — clears the lot.
/// With no stops at all a tab runs to the last column and stays there, which
/// is what a second tab proves: `b` is written after two of them.
#[test]
fn csi_3_g_clears_every_stop() -> termlens::Result<()> {
    // `DONE` goes on the next row on purpose: the last column is where the
    // tabs end up, so anything printed after them on row 0 would overwrite
    // the very cell under test.
    let mut t = sh(r"printf '\033[3ga\011\011b\r\nDONE'; read _")?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(col_of(&s, "a"), Some(0), "{s}");
    assert_eq!(
        col_of(&s, "b"),
        Some(23),
        "the last column, and the second tab does not move on from it:\n{s}"
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// `CHT` moves forward by whole stops, with a count.
#[test]
fn cht_moves_forward_by_whole_stops() -> termlens::Result<()> {
    let mut t = sh(r"printf '\033[2Ia'; printf ' DONE'; read _")?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(col_of(&s, "a"), Some(16), "two stops forward:\n{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// `CBT` moves back by whole stops — and is what `Shift-Tab` sends, so an
/// application echoing it emits `CSI Z` on its output side.
#[test]
fn cbt_moves_back_by_whole_stops() -> termlens::Result<()> {
    // From a stop, back-tab reaches the one before it.
    let mut t = sh(r"printf '\011\011\033[1Zy'; printf ' DONE'; read _")?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(col_of(&s, "y"), Some(8), "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The issue's second reproduction, pinned with the answer this crate
/// gives — which is not the one the issue predicts.
///
/// `X` advances the cursor to column 17, and a back-tab goes to the nearest
/// stop *strictly* left of where it starts, so it returns to the stop at 16
/// that `X` is sitting just past. xterm and alacritty both do this. The
/// issue reads the reproduction as landing at column 8, which is what it
/// would do without the `X` in the way — the case above.
#[test]
fn a_back_tab_returns_to_the_stop_the_cursor_is_just_past() -> termlens::Result<()> {
    let mut t = sh(r"printf '\011\011X\033[1Zy'; printf ' DONE'; read _")?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(col_of(&s, "y"), Some(16), "{s}");
    assert!(!s.contains("X"), "y is written over X:\n{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// `RIS` puts the terminal back to power-on, and the stops with it.
#[test]
fn a_hard_reset_restores_the_default_stops() -> termlens::Result<()> {
    let mut t = sh(r"printf '\033[3g\033[4G\033H\033c\011x'; printf ' DONE'; read _")?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(col_of(&s, "x"), Some(8), "every eighth column again:\n{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// `DECSTR` restores them too — the soft reset a well-behaved TUI sends on
/// startup and teardown, which leaves the screen alone.
#[test]
fn a_soft_reset_restores_the_default_stops() -> termlens::Result<()> {
    let mut t = sh(r"printf '\033[3g\033[4G\033H\033[!p\033[1G\011x'; printf ' DONE'; read _")?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(col_of(&s, "x"), Some(8), "every eighth column again:\n{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A table drawn the way the capabilities are meant to be used: clear the
/// stops, set the column ones, then tab between them for every row. This is
/// the failure the issue describes — every character present, every column
/// wrong — so it is asserted by column rather than by `contains`.
///
/// The defaults are cleared first because a cell whose text reaches the next
/// default stop would otherwise tab past it: `name` ends exactly at column 8
/// where `ada` ends at 7, so the two rows would part company on the third
/// column and nothing about the escape handling would be at fault.
#[test]
fn a_table_laid_out_with_its_own_stops_lines_up() -> termlens::Result<()> {
    let mut t = sh(concat!(
        // Stops at columns 5 and 13, one-based, and nothing else.
        r"printf '\033[3g\033[5G\033H\033[13G\033H\033[1G'; ",
        r"printf 'id\011name\011role\r\n'; ",
        r"printf '7\011ada\011dev\r\n'; ",
        "printf DONE; read _"
    ))?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.row_text(0).trim_end(), "id  name    role", "{s}");
    assert_eq!(s.row_text(1).trim_end(), "7   ada     dev", "{s}");
    // The columns line up, which is the whole point of the capability.
    assert_eq!(col_of(&s, "name"), Some(4), "{s}");
    assert_eq!(col_of(&s, "ada"), Some(4), "{s}");
    assert_eq!(col_of(&s, "role"), Some(12), "{s}");
    assert_eq!(col_of(&s, "dev"), Some(12), "{s}");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
