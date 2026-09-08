//! The snapshot text format read back into a [`Screen`] (#255): the
//! `Display` rendering of `docs/DESIGN.md` §3, with or without the
//! `styles:` block `with_styles` adds, so a saved screen — an insta `.snap`,
//! a `TERMLENS_ARTIFACT_DIR` file, a block copied out of a CI log — can be
//! diffed and rendered outside the test run that produced it.
//!
//! What the text format does not carry cannot come back: the out-of-band
//! state is default, and an erased cell and a written blank are both a
//! blank. That is the format's contract, not a loss here — `Screen::diff`
//! and the renderings see the same picture either way.

use unicode_width::UnicodeWidthChar;

use super::{Cell, Color, Screen, Style, TermState};
use crate::{Error, Result};

impl Screen {
    /// Parse the snapshot text format back into a `Screen`.
    ///
    /// Accepts exactly what [`Display`](std::fmt::Display) and
    /// [`with_styles`](Self::with_styles) write: the `size:`/`cursor:`
    /// header, the grid one row per line with trailing blanks stripped
    /// (missing trailing rows and columns are blank), and optionally a blank
    /// line, `styles:` and its span lines (or `(none)`). Everything else is
    /// an [`Error::Parse`] naming the line.
    ///
    /// The round trip is exact for what the format carries: the parsed
    /// screen renders to the same text, with the same `styles:` block, and
    /// [`diff`](Self::diff)s empty against the original. It does not
    /// compare `==` to the original — the title, the modes, the counters
    /// and the history are not in the text and come back default.
    ///
    /// ```
    /// # fn main() -> termlens::Result<()> {
    /// # let mut t = termlens::Terminal::builder()
    /// #     .args(["-c", "printf '\\033[1;31mError:\\033[0m disk full'; read q"]).spawn("sh")?;
    /// # t.wait_until(|s| s.contains("disk full"))?;
    /// let saved = t.screen().with_styles().to_string();
    /// let parsed = termlens::Screen::parse(&saved)?;
    /// assert_eq!(parsed.with_styles().to_string(), saved);
    /// assert!(t.screen().diff(&parsed).is_empty());
    /// # t.send(termlens::Key::Enter); t.wait_exit()?; Ok(())
    /// # }
    /// ```
    pub fn parse(text: &str) -> Result<Screen> {
        let mut lines = text.lines();
        let header = lines
            .next()
            .ok_or_else(|| Error::Parse("empty input; expected a `size: …` header".into()))?;
        let (cols, rows, cursor) = parse_header(header)?;
        let body: Vec<&str> = lines.collect();

        // Where the grid ends: at the `styles:` marker when there is one,
        // else at the end. A grid row could read `styles:` itself, so the
        // marker is the one preceded by a blank line (or first) and followed
        // by nothing but span lines — the shape `with_styles` writes.
        let marker = body.iter().enumerate().position(|(i, line)| {
            line.trim_end() == "styles:"
                && (i == 0 || body[i - 1].trim().is_empty())
                && body[i + 1..].iter().all(|l| is_span_line(l))
        });
        let (grid, styles) = match marker {
            // The blank separator before the marker is not a grid row.
            Some(at) => (&body[..at.saturating_sub(1)], &body[at + 1..]),
            None => (&body[..], &body[body.len()..]),
        };

        // Fewer grid lines than rows is a grid whose trailing blank rows
        // were dropped; more is only tolerated when the extra lines are
        // blank, so a stray line is an error rather than a lost row.
        let mut cells = vec![
            Cell::new(String::new(), Style::default(), false, false);
            usize::from(cols) * usize::from(rows)
        ];
        for (index, line) in grid.iter().enumerate() {
            let number = index + 2;
            if index >= usize::from(rows) {
                if line.trim().is_empty() {
                    continue;
                }
                return Err(Error::Parse(format!(
                    "line {number}: expected `styles:` or the end of the text after the {rows}-row grid, got {line:?}"
                )));
            }
            parse_row(
                line,
                number,
                cols,
                &mut cells[index * usize::from(cols)..][..usize::from(cols)],
            )?;
        }

        let mut screen = Screen::from_parts(
            cols,
            rows,
            cursor.0,
            cursor.1,
            cursor.2,
            cells,
            TermState::default(),
        );
        let first_style_line = marker.map_or(body.len(), |at| at + 1) + 2;
        parse_styles(styles, first_style_line, &mut screen)?;
        Ok(screen)
    }
}

/// A line the `styles:` block may hold: `(none)`, blank, or `ROW: …`.
fn is_span_line(line: &str) -> bool {
    let line = line.trim_end();
    line.is_empty()
        || line == "(none)"
        || line
            .split_once(": ")
            .is_some_and(|(row, _)| !row.is_empty() && row.bytes().all(|b| b.is_ascii_digit()))
}

/// `size: <cols>x<rows>  cursor: <row>,<col>` or `cursor: hidden`.
fn parse_header(line: &str) -> Result<(u16, u16, (u16, u16, bool))> {
    let bad = || {
        Error::Parse(format!("line 1: expected `size: COLSxROWS  cursor: ROW,COL` (or `cursor: hidden`), got {line:?}"))
    };
    let rest = line.strip_prefix("size: ").ok_or_else(bad)?;
    let (size, cursor) = rest.split_once("cursor:").ok_or_else(bad)?;
    let (cols, rows) = size.trim().split_once('x').ok_or_else(bad)?;
    let cols: u16 = cols.parse().map_err(|_| bad())?;
    let rows: u16 = rows.parse().map_err(|_| bad())?;
    if cols == 0 || rows == 0 {
        return Err(Error::Parse(format!(
            "line 1: a screen has at least one column and one row, got {cols}x{rows}"
        )));
    }
    let cursor = cursor.trim();
    let cursor = if cursor == "hidden" {
        (0, 0, false)
    } else {
        let (r, c) = cursor.split_once(',').ok_or_else(bad)?;
        let r: u16 = r.parse().map_err(|_| bad())?;
        let c: u16 = c.parse().map_err(|_| bad())?;
        if r >= rows || c >= cols {
            return Err(Error::Parse(format!(
                "line 1: cursor {r},{c} is outside the {cols}x{rows} grid"
            )));
        }
        (r, c, true)
    };
    Ok((cols, rows, cursor))
}

/// One grid line into `row`, a slice of exactly `cols` cells. Zero-width
/// characters join the cell before them; a wide character takes two.
fn parse_row(line: &str, number: usize, cols: u16, row: &mut [Cell]) -> Result<()> {
    let mut col = 0usize;
    for ch in line.chars() {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            match col.checked_sub(1).map(|c| &mut row[c]) {
                Some(cell) if !cell.contents.is_empty() => cell.contents.push(ch),
                _ => {
                    return Err(Error::Parse(format!(
                        "line {number}: {ch:?} has no character to combine with"
                    )));
                }
            }
            continue;
        }
        if col + width > usize::from(cols) {
            return Err(Error::Parse(format!(
                "line {number}: the row is wider than the {cols} columns the header declares"
            )));
        }
        row[col] = Cell::new(ch.to_string(), Style::default(), width == 2, false);
        if width == 2 {
            row[col + 1] = Cell::new(String::new(), Style::default(), false, true);
        }
        col += width;
    }
    Ok(())
}

/// The span lines after `styles:` — `(none)`, or `<row>: <spans>` — applied
/// onto `screen`'s cells. `first_line` is the 1-based number of the first,
/// for the errors.
fn parse_styles(lines: &[&str], first_line: usize, screen: &mut Screen) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let cols = screen.cols;
    let mut cells: Vec<Cell> = screen.cells.to_vec();
    for (offset, line) in lines.iter().enumerate() {
        let number = first_line + offset;
        let line = line.trim_end();
        if line.is_empty() || line == "(none)" {
            continue;
        }
        let bad = |what: &str| Error::Parse(format!("line {number}: {what} in {line:?}"));
        let (row, spans) = line
            .split_once(": ")
            .ok_or_else(|| bad("expected `ROW: SPANS`"))?;
        let row: u16 = row.trim().parse().map_err(|_| bad("bad row number"))?;
        if row >= screen.rows {
            return Err(bad("row is outside the grid"));
        }
        for span in spans.split("; ") {
            let mut tokens = span.split_whitespace();
            let range = tokens.next().ok_or_else(|| bad("empty span"))?;
            let column = |text: &str| text.parse::<u16>().map_err(|_| bad("bad column"));
            let (start, end) = match range.split_once('-') {
                Some((s, e)) => (column(s)?, column(e)?),
                None => {
                    let c = column(range)?;
                    (c, c)
                }
            };
            if end < start || end >= cols {
                return Err(bad("span is outside the grid"));
            }
            let mut style = Style::default();
            for token in tokens {
                match token {
                    "bold" => style.bold = true,
                    "dim" => style.dim = true,
                    "italic" => style.italic = true,
                    "underline" => style.underline = true,
                    "blink" => style.blink = true,
                    "reverse" => style.reverse = true,
                    "conceal" => style.conceal = true,
                    "strikethrough" => style.strikethrough = true,
                    _ => {
                        if let Some(color) = token.strip_prefix("fg=") {
                            style.fg = parse_color(color).ok_or_else(|| bad("bad colour"))?;
                        } else if let Some(color) = token.strip_prefix("bg=") {
                            style.bg = parse_color(color).ok_or_else(|| bad("bad colour"))?;
                        } else {
                            return Err(bad("unknown style token"));
                        }
                    }
                }
            }
            for col in start..=end {
                cells[usize::from(row) * usize::from(cols) + usize::from(col)].style = style;
            }
        }
    }
    screen.cells = cells.into();
    Ok(())
}

/// `4` (indexed) or `#rrggbb`.
fn parse_color(text: &str) -> Option<Color> {
    if let Some(hex) = text.strip_prefix('#') {
        if hex.len() != 6 {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        return Some(Color::Rgb(channel(0)?, channel(2)?, channel(4)?));
    }
    text.parse().ok().map(Color::Indexed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grid_with_styles_round_trips() {
        let saved = "size: 6x2  cursor: 1,2\nab 東\n\n\nstyles:\n0: 0-1 fg=1 bold; 3-4 bg=#1e1e2e\n1: 5 reverse";
        let screen = Screen::parse(saved).unwrap();
        assert_eq!(screen.size(), (6, 2));
        assert_eq!(screen.cursor(), (1, 2, true));
        assert!(screen.cell(0, 3).unwrap().is_wide());
        assert!(screen.cell(0, 4).unwrap().is_wide_continuation());
        assert_eq!(screen.cell(0, 0).unwrap().style().fg, Color::Indexed(1));
        assert_eq!(
            screen.cell(0, 4).unwrap().style().bg,
            Color::Rgb(0x1e, 0x1e, 0x2e)
        );
        assert!(screen.cell(1, 5).unwrap().style().reverse);
        assert_eq!(screen.with_styles().to_string(), saved);
    }

    #[test]
    fn a_short_grid_is_padded_and_none_is_accepted() {
        let screen = Screen::parse("size: 4x3  cursor: hidden\nhi\n\nstyles:\n(none)").unwrap();
        assert_eq!(screen.row_text(0), "hi  ");
        assert_eq!(screen.row_text(2), "    ");
        assert_eq!(screen.cursor(), (0, 0, false));
        let plain = Screen::parse("size: 4x3  cursor: hidden\nhi").unwrap();
        assert!(screen.diff(&plain).is_empty());
    }

    #[test]
    fn combining_marks_join_the_cell_before_them() {
        let screen = Screen::parse("size: 3x1  cursor: 0,0\ne\u{301}x").unwrap();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "e\u{301}");
        assert_eq!(screen.cell(0, 1).unwrap().contents(), "x");
    }

    #[test]
    fn errors_name_the_line() {
        let long = Screen::parse("size: 2x1  cursor: 0,0\nabc")
            .unwrap_err()
            .to_string();
        assert!(long.contains("line 2") && long.contains("wider"), "{long}");
        let header = Screen::parse("80x24").unwrap_err().to_string();
        assert!(header.contains("line 1"), "{header}");
        let cursor = Screen::parse("size: 2x1  cursor: 5,0")
            .unwrap_err()
            .to_string();
        assert!(cursor.contains("outside"), "{cursor}");
        let token = Screen::parse("size: 2x1  cursor: 0,0\nab\n\nstyles:\n0: 0 shiny")
            .unwrap_err()
            .to_string();
        assert!(
            token.contains("line 5") && token.contains("unknown style token"),
            "{token}"
        );
        let junk = Screen::parse("size: 2x1  cursor: 0,0\nab\nextra\n")
            .unwrap_err()
            .to_string();
        assert!(junk.contains("expected `styles:`"), "{junk}");
    }
}
