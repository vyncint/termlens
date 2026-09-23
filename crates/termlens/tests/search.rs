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

/// `rect_text` was the only region reader with no test (#455): every sibling
/// that takes the same range arguments has one above, and this is the one a
/// test author reaches for first — one pane of a split layout. Its rustdoc
/// makes four promises, and each is asserted here.
///
/// Row 1 is `ab中cd  `: `a` `b`, the wide `中` across columns 2–3, `c` `d`,
/// and trailing blanks — the row that carries both the wide-character rule
/// and the whitespace one.
#[test]
fn rect_text_reads_a_rectangle_clamps_and_keeps_a_cut_wide_character() -> termlens::Result<()> {
    let mut t = emit(&[
        "--raw",
        r"left | right\nab中cd  \nrow three\nDONE",
        "--wait",
    ])?;
    t.wait_until(|s| s.contains("DONE"))?;
    let s = t.screen();

    // Inside the grid: exactly its columns, exactly its rows.
    assert_eq!(s.rect_text(0..4, 0..1), "left");
    assert_eq!(s.rect_text(7..12, 0..1), "right");
    assert_eq!(s.rect_text(0..3, 2..4), "row\nDON");

    // Out-of-range bounds clamp rather than panic, on both axes: asking for
    // more screen than exists is allowed, and means the screen that exists.
    assert_eq!(s.rect_text(7..999, 0..1), s.rect_text(7.., 0..1));
    assert_eq!(s.rect_text(0..4, 0..999), s.rect_text(0..4, ..));
    assert_eq!(
        s.rect_text(999..1000, ..),
        "\n\n\n\n\n",
        "past the right edge: six empty rows"
    );

    // Trailing whitespace is stripped per row, as `Screen::text` does.
    assert_eq!(s.rect_text(4.., 1..2), "cd", "the row's trailing blanks go");
    assert_eq!(
        s.rect_text(0..3, 0..1),
        "lef",
        "no trailing space was invented"
    );

    // A wide character contributes where its *leading* cell sits, even when
    // the rectangle cuts it in half — and its continuation cell contributes
    // nothing at all, not even a space. The case a refactor breaks quietly.
    assert_eq!(
        s.rect_text(2..3, 1..2),
        "中",
        "leading cell alone: the whole glyph"
    );
    assert_eq!(s.rect_text(3..6, 1..2), "cd", "continuation alone: nothing");
    assert_eq!(
        s.rect_text(0..6, 1..2),
        "ab中cd",
        "both halves: once, not twice"
    );

    t.send(Key::Enter)?;
    assert!(t.wait_exit()?.success());
    Ok(())
}

/// A backwards range is a mistake in the calling source, not a fact about
/// the terminal, so it panics the way `&slice[3..0]` does — and the message
/// names the axis and quotes the caller's own numbers, taken *before*
/// clamping, so a swapped `(cols, rows)` is recognisable from it (#455).
///
/// The bounds are computed rather than written out: clippy's
/// `reversed_empty_ranges` already refuses a literal `5..2`, which is the
/// case the rustdoc says this panic exists to cover beyond it.
#[test]
#[should_panic(expected = "rect_text: column range starts at 5 but ends at 2")]
fn rect_text_panics_on_a_backwards_column_range() {
    let s = termlens::Screen::parse("size: 10x2  cursor: 0,0\nhello\n").expect("a saved screen");
    let (from, to) = std::hint::black_box((5u16, 2u16));
    let _ = s.rect_text(from..to, ..);
}

#[test]
#[should_panic(expected = "rect_text: row range starts at 5 but ends at 3")]
fn rect_text_panics_on_a_backwards_row_range() {
    let s = termlens::Screen::parse("size: 10x2  cursor: 0,0\nhello\n").expect("a saved screen");
    // Out of range *and* backwards, on a two-row screen. Clamped first,
    // `5..3` would become `2..2` — not inverted, so no panic, and an empty
    // string that reads as "this pane is empty". Checked first, as the
    // rustdoc promises, it panics and quotes the numbers that were written.
    let (from, to) = std::hint::black_box((5u16, 3u16));
    let _ = s.rect_text(.., from..to);
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

/// `mask_matching` is documented as matching "the way `find_all` matches",
/// and `find_all` spans rows. The mask ran its matcher one row at a time, so
/// a needle crossing a row boundary was reported and then left on screen —
/// the one failure mode a mask exists to prevent (#300).
#[test]
#[cfg_attr(
    windows,
    ignore = "ConPTY re-renders the grid, so the wide-character columns this asserts on are its own (#149)"
)]
fn a_mask_covers_a_needle_that_spans_rows() -> termlens::Result<()> {
    // Two occurrences, one of them crossing a wide character, and a row that
    // must survive untouched between them.
    // Two writes with a pause between them, the way a read boundary once
    // split the one write this used to be: the 0.11.3 release stress run
    // (ubuntu, 4 threads) snapshotted after `KEEP ME` with row 4 still
    // unpainted, and the needle spanning rows 3–4 had nothing to match.
    let mut t = emit(&[
        "--raw",
        r"abc\r\ndef\r\nKEEP ME\r\nab東\r\n",
        "--sleep",
        "300ms",
        "--raw",
        r"def\r\n",
        "--wait",
    ])?;
    // `KEEP ME` is the third line, not the last: the last thing painted is
    // the final `\r\n`, which parks the cursor on row 5 (DESIGN §2).
    t.wait_until(|s| s.cursor().0 == 5)?;
    let screen = t.screen();

    assert_eq!(screen.find_all("abc\ndef"), vec![(0, 0)]);
    let masked = screen.mask_matching("abc\ndef", '*');
    assert!(
        masked.find_all("abc\ndef").is_empty(),
        "the needle survived the mask:\n{masked}"
    );
    assert_eq!(masked.row_text(0).trim_end(), "***");
    assert_eq!(masked.row_text(1).trim_end(), "***");
    // Everything outside the match is untouched, styles included.
    assert_eq!(masked.row_text(2).trim_end(), "KEEP ME");
    assert_eq!(masked.row_text(3).trim_end(), "ab東");
    assert_eq!(masked.size(), screen.size());
    assert_eq!(masked.cursor(), screen.cursor());

    // A wide character under the match becomes two fill cells, so the row
    // keeps its width — the invariant every mask holds.
    let wide = screen.mask_matching("ab東\ndef", '#');
    assert_eq!(wide.row_text(3).trim_end(), "####");
    assert_eq!(wide.row_text(4).trim_end(), "###");
    assert_eq!(wide.row_text(2).trim_end(), "KEEP ME");
    assert!(wide.find_all("ab東\ndef").is_empty(), "{wide}");

    t.send(Key::Enter)?;
    t.wait_exit()?;
    Ok(())
}
