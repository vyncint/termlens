//! `find_all`, the `regex` feature's matches and waits, and the grid-aware
//! masks (#264, #245, #250).

use std::time::Duration;

use termlens::{Key, Terminal};

mod common;

fn emit(steps: &[&str]) -> termlens::Result<Terminal> {
    common::spawn_emit(
        Terminal::builder()
            .size(40, 6)
            .timeout(Duration::from_secs(10)),
        steps,
    )
}

#[test]
fn find_all_returns_every_match_in_reading_order() -> termlens::Result<()> {
    let mut t = emit(&["item one\nitem two\n  item three\nDONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.find_all("item"), [(0, 0), (1, 0), (2, 2)], "{s}");
    assert_eq!(
        s.find("item"),
        Some((0, 0)),
        "find is the first of the same scan"
    );
    assert_eq!(s.find_all("item").len(), 3);
    assert!(s.find_all("nowhere").is_empty());
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// Matches do not overlap, a match after a wide character reports the real
/// column, and a multi-row needle is found wherever `contains` is true.
#[test]
fn find_all_does_not_overlap_and_keeps_real_columns() -> termlens::Result<()> {
    let mut t = emit(&["aaaa 汉ab ab\nx\nab\nx\nDONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    assert_eq!(s.find_all("aa"), [(0, 0), (0, 2)], "non-overlapping: {s}");
    // `ab` twice on row 0 — the first straight after the two-column 汉 —
    // then once on row 2.
    assert_eq!(s.find_all("ab"), [(0, 7), (0, 10), (2, 0)], "{s}");
    assert_eq!(
        s.find_all("ab\nx"),
        [(0, 10), (2, 0)],
        "multi-row, twice: {s}"
    );
    assert!(s.contains("ab\nx"));
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn mask_rect_blanks_the_rectangle_and_keeps_everything_else() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"\e[1mmyapp\e[0m         12:34:56\nrow two\nDONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    let masked = s.mask_rect(14.., ..1);
    assert_eq!(masked.row_text(0).trim_end(), "myapp", "{masked}");
    assert_eq!(
        masked.row_text(1).trim_end(),
        "row two",
        "the other rows are untouched: {masked}"
    );
    assert_eq!(masked.size(), s.size());
    assert_eq!(masked.cursor(), s.cursor());
    assert!(
        masked.cell(0, 0).unwrap().style().bold,
        "styles survive: {}",
        masked.with_styles()
    );
    assert_eq!(masked.title(), s.title(), "out-of-band state is shared");
    assert!(s.contains("12:34:56"), "the original is not mutated");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// The point of a grid-aware mask: the width never changes.
#[test]
fn mask_matching_keeps_the_width_and_masks_both_columns_of_a_wide_character() -> termlens::Result<()>
{
    let mut t = emit(&["--raw", r"at 12:34:56 in 汉字 town\nDONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    let masked = s.mask_matching("12:34:56", '▒');
    assert_eq!(
        masked.row_text(0).trim_end(),
        "at ▒▒▒▒▒▒▒▒ in 汉字 town",
        "{masked}"
    );
    assert_eq!(
        masked.row_text(0).chars().count(),
        s.row_text(0).chars().count(),
        "same width"
    );
    assert_eq!(
        masked.find("in"),
        s.find("in"),
        "nothing after the field moved"
    );

    let wide = s.mask_matching("汉", '#');
    assert_eq!(
        wide.row_text(0).trim_end(),
        "at 12:34:56 in ##字 town",
        "{wide}"
    );
    assert!(
        !wide.cell(0, 15).unwrap().is_wide(),
        "two narrow fill cells now"
    );
    assert_eq!(wide.find("town"), s.find("town"), "the tail did not move");
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[test]
fn mask_cells_blanks_by_predicate() -> termlens::Result<()> {
    let mut t = emit(&["--raw", r"keep \e[2mfaint\e[0m keep\nDONE", "--wait"])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();
    let masked = s.mask_cells(|c| c.style().dim);
    assert_eq!(masked.row_text(0).trim_end(), "keep       keep", "{masked}");
    assert!(
        masked.with_styles().to_string().contains("0: 5-9 dim"),
        "the style run is kept so a dim field stays visibly a field:\n{}",
        masked.with_styles()
    );
    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

#[cfg(feature = "regex")]
mod patterns {
    use super::*;
    use regex::Regex;

    #[test]
    fn a_pattern_matches_within_a_row_and_reports_cell_columns() -> termlens::Result<()> {
        let mut t = emit(&["--raw", r"汉 myapp v1.42.0 ready\nbuild 7\nDONE", "--wait"])?;
        t.wait_until(|s| s.contains("DONE"))?;
        let s = t.screen();
        let version = Regex::new(r"v\d+\.\d+\.\d+").unwrap();
        assert_eq!(
            s.find_match(&version),
            Some((0, 9, "v1.42.0".to_owned())),
            "the column is a cell column, after the two-column 汉: {s}"
        );
        assert!(s.matches(&version));
        let digits = Regex::new(r"\d+").unwrap();
        assert_eq!(
            s.find_all_matches(&digits),
            [
                (0, 10, "1".to_owned()),
                (0, 12, "42".to_owned()),
                (0, 15, "0".to_owned()),
                (1, 6, "7".to_owned()),
            ],
            "{s}"
        );
        // A pattern never spans rows.
        assert!(!s.matches(&Regex::new(r"ready\nbuild").unwrap()));
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success());
        Ok(())
    }

    #[test]
    fn wait_until_matches_returns_the_screen_it_matched() -> termlens::Result<()> {
        let mut t = emit(&["counting: ", "--sleep", "200ms", "42", "--wait"])?;
        let prompt = Regex::new(r"counting: \d+$").unwrap();
        let matched = t.wait_until_matches(&prompt)?;
        assert!(matched.matches(&prompt), "{matched}");
        assert_eq!(matched.find_match(&prompt).unwrap().2, "counting: 42");
        // And a pattern that never appears is the ordinary timeout.
        let never = Regex::new(r"never \d+").unwrap();
        let err = t
            .wait_until_matches_for(&never, Duration::from_millis(200))
            .unwrap_err();
        assert!(matches!(err, termlens::Error::Timeout { .. }), "{err}");
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success());
        Ok(())
    }

    /// A volatile field — this process's id — masked by shape, then
    /// snapshotted: the snapshot is stable across runs because the mask
    /// keeps the width whatever the number was.
    #[test]
    fn a_masked_volatile_field_snapshots_stably() -> termlens::Result<()> {
        let mut t = emit(&["pid ", "--pid", " running\nDONE", "--wait"])?;
        t.wait_until(|s| s.contains("DONE"))?;
        let s = t.screen();
        let digits = Regex::new(r"\d+").unwrap();
        let masked = s.mask_matches(&digits, '#');
        assert!(
            masked.find("running").is_some(),
            "the word after the field is still there: {masked}"
        );
        // The width follows the pid, so the stable assertion is about
        // shape, not about the exact string.
        let row = masked.row_text(0);
        let field: String = row.chars().filter(|&c| c == '#').collect();
        assert!(!field.is_empty(), "{masked}");
        assert!(row.trim_end().starts_with("pid #"), "{masked}");
        assert!(row.trim_end().ends_with("# running"), "{masked}");
        t.send(Key::Enter)?;
        assert!(t.wait_exit()?.success());
        Ok(())
    }
}
