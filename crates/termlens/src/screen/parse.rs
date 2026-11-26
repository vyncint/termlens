//! The snapshot text format read back into a [`Screen`] (#255): the
//! `Display` rendering of `docs/DESIGN.md` §3, with or without the
//! `styles:` block `with_styles` adds, so a saved screen — an insta `.snap`,
//! a `TERMLENS_ARTIFACT_DIR` file, a block copied out of a CI log — can be
//! diffed and rendered outside the test run that produced it.
//!
//! What the text format does not carry cannot come back: the out-of-band
//! state is default, an erased cell and a written blank are both a blank,
//! and so is any other whitespace drawn at the end of a row, which the
//! format strips with the padding. That is the format's contract, not a
//! loss here — `Screen::diff` and the renderings see the same picture
//! either way.

use unicode_width::UnicodeWidthChar;

use super::{Cell, Color, Screen, Style, TermState};
use crate::terminal::MAX_DIMENSION;
use crate::{Error, Result};

impl Screen {
    /// Parse the snapshot text format back into a `Screen`.
    ///
    /// Accepts what [`Display`](std::fmt::Display) and
    /// [`with_styles`](Self::with_styles) write: the `size:`/`cursor:`
    /// header, the grid one row per line with trailing blanks stripped
    /// (missing trailing rows and columns are blank), and optionally a blank
    /// line, `styles:` and its span lines (or `(none)`). It also reads a few
    /// shapes they never write, each into the screen its canonical form
    /// gives: a `+` before a number in the header, one space or none before
    /// `cursor:`, a row's spans split over several lines, blank lines
    /// between span lines, `(none)` after spans, a one-column range written
    /// `0-0`, a token repeated or out of order, and a span with no token
    /// (the default style). Any other line is an [`Error::Parse`] naming
    /// it. A file that parses is therefore not proof that termlens wrote
    /// it; rendering it back gives the canonical form.
    ///
    /// **The header's row count decides where the grid ends**, so a grid may
    /// contain the words `styles:` or `(none)` as ordinary content. The one
    /// ambiguous input is a snapshot whose trailing blank rows were trimmed
    /// by hand *and* which carries a styles block: its block falls inside
    /// the declared row count and is read as content. Content wins, on
    /// purpose — a wrong row of text shows up in a diff, a silently dropped
    /// one does not.
    ///
    /// A hidden cursor's position is not in the text format, so it comes
    /// back at `0,0`. That is not a difference anyone can see, and
    /// [`diff`](Self::diff) does not report it as one.
    ///
    /// The round trip is exact for what the format carries: the parsed
    /// screen renders to the same text, with the same `styles:` block, and
    /// [`diff`](Self::diff)s empty against the original. It does not
    /// compare `==` to the original — the title, the modes, the counters
    /// and the history are not in the text and come back default.
    ///
    /// Whitespace at the end of a row is not carried, whichever whitespace
    /// it is. The format strips a no-break space (U+00A0) or an ideographic
    /// space (U+3000) an application drew there together with the blank
    /// padding, so the cell comes back blank, and U+3000, which is two
    /// columns wide, as two blank cells. A terminal shows a blank for each,
    /// and [`diff`](Self::diff) does not report one as a difference. The
    /// JSON (`serde` feature) keeps the character.
    ///
    /// An emoji with a presentation selector (U+FE0F, `⚠️`) is read two
    /// columns wide, as releases since 0.11.7 write it. Earlier releases
    /// gave it one column, and the text does not say which layout a file
    /// is in. A row that only fits at one column is read that way; a row
    /// with room after the emoji is read at two, so for a file an earlier
    /// release wrote every cell after the emoji is one column right of
    /// where its writer drew it, while the `styles:` spans and the cursor
    /// keep their column numbers and so land one character left of what
    /// they marked. The file still renders back byte for byte, but its
    /// [`diff`](Self::diff) against the screen its JSON twin holds is not
    /// empty. Keep the JSON where an old file's columns matter (DESIGN §3,
    /// rule 4).
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
    /// # t.send(termlens::Key::Enter)?; t.wait_exit()?; Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// [`Error::Parse`] when `text` is not the snapshot text format — a
    /// header that is not `size: COLSxROWS  cursor: ROW,COL`, a row wider than
    /// the header declares or text after the grid that is not a `styles:`
    /// block, a control character, a malformed span — with the number of the
    /// line it failed on.
    pub fn parse(text: &str) -> Result<Screen> {
        let mut lines = text.lines();
        let header = lines
            .next()
            .ok_or_else(|| Error::Parse("empty input; expected a `size: …` header".into()))?;
        let (cols, rows, cursor) = parse_header(header)?;

        // The grid is the number of lines the header declares, and nothing
        // else decides that. `Display` writes every row — blank ones
        // included — before `with_styles` adds its block, so counting is
        // unambiguous for text this crate produced. Deliberately *not* a
        // search for the `styles:` marker: a grid can contain that word as
        // ordinary content, and reading content as metadata silently
        // deleted it (#296).
        //
        // Fewer lines than rows is a snapshot whose trailing blank rows were
        // trimmed; the remainder pads out blank. The cost of the rule is at
        // the other end: in a *trimmed* snapshot that also carries a styles
        // block, the block's lines are within the declared row count and are
        // read as grid content. Content wins ties, on purpose — a wrong row
        // of text is visible in a diff, a silently dropped one is not.
        //
        // The lines are walked, never gathered: a 1x1 screen followed by
        // 200 MB of blank lines kept a pointer for each, 17 times the text,
        // and aborted `termlens render` (#779). What the parse holds is the
        // grid the header declares.
        let mut cells = vec![
            Cell::new(String::new(), Style::default(), false, false);
            usize::from(cols) * usize::from(rows)
        ];
        let mut grid = 0;
        for (index, line) in lines.by_ref().take(usize::from(rows)).enumerate() {
            parse_row(
                line,
                index + 2,
                cols,
                &mut cells[index * usize::from(cols)..][..usize::from(cols)],
            )?;
            grid = index + 1;
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

        // After the grid: blank separator lines, then `styles:` and its
        // spans, or nothing at all. Each line with its 1-based number.
        let mut rest = lines
            .enumerate()
            .map(|(offset, line)| (grid + offset + 2, line));
        if let Some((number, line)) = rest.find(|(_, line)| !line.trim().is_empty()) {
            if line.trim_end() != "styles:" {
                return Err(Error::Parse(format!(
                    "line {number}: expected `styles:` or the end of the text after the {rows}-row grid, got {line:?}"
                )));
            }
            parse_styles(rest, &mut screen)?;
        }
        Ok(screen)
    }
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
    // The grid is allocated from this header before any line is read, so a
    // declared size no terminal can have is refused here (#583).
    if cols > MAX_DIMENSION || rows > MAX_DIMENSION {
        return Err(Error::Parse(format!(
            "line 1: {cols}x{rows} is past the {MAX_DIMENSION}-per-axis limit of a terminal, \
             so no termlens wrote this screen"
        )));
    }
    let cursor = cursor.trim();
    let cursor = if cursor == "hidden" {
        (0, 0, false)
    } else {
        let (r, c) = cursor.split_once(',').ok_or_else(bad)?;
        let r: u16 = r.parse().map_err(|_| bad())?;
        let c: u16 = c.parse().map_err(|_| bad())?;
        // `c == cols` is the pending-wrap column a terminal parks at after
        // a write fills the last cell. Releases up to 0.11.1 wrote it (#401
        // stopped the emulator producing it), so it is clamped onto the
        // last cell, as the JSON reader does (#375), rather than refused:
        // the format promise is that every file a release wrote still
        // reads (#643). Any other column off the grid is a file nothing
        // wrote.
        let c = if c == cols { cols - 1 } else { c };
        if r >= rows || c >= cols {
            return Err(Error::Parse(format!(
                "line 1: cursor {r},{c} is outside the {cols}x{rows} grid"
            )));
        }
        (r, c, true)
    };
    Ok((cols, rows, cursor))
}

/// The columns `ch` takes in a cell, or why no saved screen holds it.
///
/// `unicode_width` answers `None` for a control character — C0, DEL and C1
/// — and `Some(0)` for a zero-width mark. Reading them alike folded an
/// `ESC` into the cell before it, and the snapshot renderings then carried
/// the raw byte out (#376). The JSON reader asks this of every character in
/// every cell, so the two readers refuse the same ones (#582).
pub(super) fn cell_width(ch: char) -> std::result::Result<usize, String> {
    ch.width().ok_or_else(|| {
        format!("{ch:?} is a control character; the snapshot format never writes one")
    })
}

/// One grid line into `row`, a slice of exactly `cols` cells. A zero-width
/// combining mark joins the cell before it; a wide character takes two; a
/// control character is an error.
///
/// An emoji-presentation selector (U+FE0F) makes the emoji it follows two
/// columns wide, as the emulator lays it out since 0.11.7 (vt100 0.18), so
/// a row written with one reads back with the same columns. Earlier
/// releases gave such a cell one column, and the text does not say which
/// layout wrote it; a row that only fits at one column per such cell can
/// only be one of those, and is read that way, so no file an earlier
/// release wrote stops parsing.
/// A row that fits both ways is read wide, which for an earlier
/// release's row with room after the emoji is a column off from what it
/// drew, its `styles:` spans and cursor included (#723): the price of
/// reading what 0.11.7 and later write as written, since nothing in the
/// text tells the two apart.
///
/// A row that fits neither way but would at one column for some of its
/// wide characters is the other shape releases up to 0.11.6 wrote: vt100
/// 0.16 narrowed a row by truncating it, which stranded a wide character
/// without its second half, and a program that redrew after the screen
/// widened again put text in the column that half had held (#683). As
/// many wide characters as the row needs are read as those stranded
/// cells, as the JSON reader holds them (`check_row`): the leftmost
/// first, but one in the first column last. No resize strands that one,
/// a terminal being two columns wide at least, and taking it first moved
/// every cell, span and cursor after it a column left of what the writer
/// drew when the stranded character was a later one (#782). It is still
/// taken when nothing else fits, so no row that read stops reading.
/// A row that fits is read as fitting: the text does not say a wide
/// character lost its second half, and two columns is what every capture
/// writes.
fn parse_row(line: &str, number: usize, cols: u16, row: &mut [Cell]) -> Result<()> {
    let blank =
        |row: &mut [Cell]| row.fill(Cell::new(String::new(), Style::default(), false, false));
    let whole = lay_out_row(line, number, cols, row, true, 0, false).or_else(|_| {
        blank(row);
        lay_out_row(line, number, cols, row, false, 0, false)
    });
    let Err(err) = whole else {
        return Ok(());
    };
    // Each stranded character gives back one column, and a wide character
    // that lands in the last column strands there by itself, so the row
    // fits at `over - 1` strands or at `over`, or at no count.
    let over = natural_width(line).saturating_sub(usize::from(cols));
    for first in [false, true] {
        for strand in [over.saturating_sub(1), over] {
            if strand == 0 {
                continue;
            }
            blank(row);
            if lay_out_row(line, number, cols, row, false, strand, first).is_ok() {
                return Ok(());
            }
        }
    }
    Err(err)
}

/// The columns `line` takes laid out with no selector widening and nothing
/// stranded: the width [`parse_row`] measures the overflow by. A character
/// the layout refuses counts nothing here; the layout names it.
fn natural_width(line: &str) -> usize {
    line.chars()
        .map(|ch| cell_width(ch).map_or(0, |width| width.min(2)))
        .sum()
}

/// [`parse_row`] for one layout: `widen` is whether the selector widens,
/// `strand` how many wide characters, the leftmost first, are laid out as
/// stranded one-column cells, and `first` whether one in the first column
/// may be among them.
fn lay_out_row(
    line: &str,
    number: usize,
    cols: u16,
    row: &mut [Cell],
    widen: bool,
    mut strand: usize,
    first: bool,
) -> Result<()> {
    let too_wide = || {
        Error::Parse(format!(
            "line {number}: the row is wider than the {cols} columns the header declares"
        ))
    };
    let mut col = 0usize;
    for ch in line.chars() {
        let width = cell_width(ch).map_err(|why| Error::Parse(format!("line {number}: {why}")))?;
        if width == 0 {
            // The cell that owns the glyph before this mark. A wide
            // character advanced the column by two, so one step back lands
            // on its continuation half, which holds no text — the mark
            // belongs to the leading cell another step back (#297).
            let owner = col.checked_sub(1).map(|c| {
                if row[c].wide_continuation {
                    c.saturating_sub(1)
                } else {
                    c
                }
            });
            let Some(owner) = owner.filter(|&c| !row[c].contents.is_empty()) else {
                return Err(Error::Parse(format!(
                    "line {number}: {ch:?} has no character to combine with"
                )));
            };
            row[owner].contents.push(ch);
            // The selector widens the cell it joins when the cluster is
            // wide — only a narrow cell just written can be widened, since
            // the column after it is the one it grows into.
            if widen
                && ch == crate::width::VS16
                && owner + 1 == col
                && !row[owner].wide
                && crate::width::cell_is_wide(&row[owner].contents)
            {
                if col >= usize::from(cols) {
                    return Err(too_wide());
                }
                row[owner].wide = true;
                row[col] = Cell::new(String::new(), Style::default(), false, true);
                col += 1;
            }
            continue;
        }
        // At most two columns, as the emulator draws them.
        let width = width.min(2);
        if width == 2 && strand > 0 && col < usize::from(cols) && (first || col > 0) {
            // A wide character stranded before other text (#683): one
            // cell, wide, with no continuation, as below.
            strand -= 1;
            row[col] = Cell::new(ch.to_string(), Style::default(), true, false);
            col += 1;
            continue;
        }
        if width == 2 && col + 1 == usize::from(cols) {
            // A wide character in the last column, with no room for its
            // second half: a narrowing resize strands one there (DESIGN
            // §3), and releases on vt100 0.16 wrote the row that way. It
            // is kept wide with no continuation cell, as the JSON reader
            // keeps it (`check_row`), so the file reads and renders back
            // as written (#643). The column count stops at the edge, so a
            // mark after it still finds its cell and anything wider is
            // refused below.
            row[col] = Cell::new(ch.to_string(), Style::default(), true, false);
            col += 1;
            continue;
        }
        if col + width > usize::from(cols) {
            return Err(too_wide());
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
/// onto `screen`'s cells, each with its 1-based number for the errors.
fn parse_styles<'a>(
    lines: impl Iterator<Item = (usize, &'a str)>,
    screen: &mut Screen,
) -> Result<()> {
    let cols = screen.cols;
    // Copied at the first span, so a block of `(none)` copies nothing.
    let mut styled: Option<Vec<Cell>> = None;
    for (number, line) in lines {
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
            let cells = styled.get_or_insert_with(|| screen.cells.to_vec());
            for col in start..=end {
                cells[usize::from(row) * usize::from(cols) + usize::from(col)].style = style;
            }
        }
    }
    if let Some(cells) = styled {
        screen.cells = cells.into();
    }
    Ok(())
}

/// `4` (indexed) or `#rrggbb`.
///
/// Six *bytes* of hex is not six hex digits: `#a\u{20ac}bc` is six bytes and
/// slicing it in pairs lands inside a character, which used to panic where
/// the whole point of this module is to turn bad text into `Error::Parse`
/// (#299). Every byte is checked before any of them is read as a digit.
fn parse_color(text: &str) -> Option<Color> {
    if let Some(hex) = text.strip_prefix('#') {
        let hex = hex.as_bytes();
        if hex.len() != 6 || !hex.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        let digit = |i: usize| char::from(hex[i]).to_digit(16).map(|d| d as u8);
        let channel = |i: usize| Some(digit(i)? * 16 + digit(i + 1)?);
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
        // `lines()` drops the empty piece after a trailing newline, so a
        // full Display of a screen ending in blank rows is already "short".
        let screen = Screen::parse("size: 4x3  cursor: hidden\nhi").unwrap();
        assert_eq!(screen.row_text(0), "hi  ");
        assert_eq!(screen.row_text(2), "    ");
        assert_eq!(screen.cursor(), (0, 0, false));
        let padded = Screen::parse("size: 4x3  cursor: hidden\nhi\n\n").unwrap();
        assert!(screen.diff(&padded).is_empty());
        // `(none)` after a full-height grid is the styles block saying the
        // screen carries no styles at all.
        let none = Screen::parse("size: 4x3  cursor: hidden\nhi\n\n\n\nstyles:\n(none)").unwrap();
        assert!(screen.diff(&none).is_empty());
    }

    /// Shapes `Display` and `with_styles` never write, which the reader
    /// takes as the canonical form beside them (#849). Each reads to the
    /// screen its canonical form gives, and writes back as that form.
    #[test]
    fn the_reader_accepts_these_shapes_beside_the_canonical_ones() {
        for (written, canonical) in [
            // A leading `+` on the size and the cursor.
            ("size: +4x+1  cursor: 0,0\nhi", "size: 4x1  cursor: 0,0\nhi"),
            ("size: 4x1  cursor: +0,+0\nhi", "size: 4x1  cursor: 0,0\nhi"),
            // One space before `cursor:`, or none.
            ("size: 4x1 cursor: 0,0\nhi", "size: 4x1  cursor: 0,0\nhi"),
            ("size: 4x1cursor: 0,0\nhi", "size: 4x1  cursor: 0,0\nhi"),
            // A row listed twice, its spans merged.
            (
                "size: 4x1  cursor: 0,0\nhi\n\nstyles:\n0: 0 bold\n0: 1 italic",
                "size: 4x1  cursor: 0,0\nhi\n\nstyles:\n0: 0 bold; 1 italic",
            ),
            // `(none)` after spans, and blank lines between span lines.
            (
                "size: 4x1  cursor: 0,0\nhi\n\nstyles:\n0: 0 bold\n(none)",
                "size: 4x1  cursor: 0,0\nhi\n\nstyles:\n0: 0 bold",
            ),
            (
                "size: 4x2  cursor: 0,0\nhi\nho\n\nstyles:\n0: 0 bold\n\n1: 1 italic",
                "size: 4x2  cursor: 0,0\nhi\nho\n\nstyles:\n0: 0 bold\n1: 1 italic",
            ),
            // A one-column range, a repeated token, and a run's tokens in
            // another order.
            (
                "size: 4x1  cursor: 0,0\nhi\n\nstyles:\n0: 0-0 bold",
                "size: 4x1  cursor: 0,0\nhi\n\nstyles:\n0: 0 bold",
            ),
            (
                "size: 4x1  cursor: 0,0\nhi\n\nstyles:\n0: 0 bold bold",
                "size: 4x1  cursor: 0,0\nhi\n\nstyles:\n0: 0 bold",
            ),
            (
                "size: 4x1  cursor: 0,0\nhi\n\nstyles:\n0: 0 bold fg=1",
                "size: 4x1  cursor: 0,0\nhi\n\nstyles:\n0: 0 fg=1 bold",
            ),
            // A span with no token is the default style.
            (
                "size: 4x1  cursor: 0,0\nhi\n\nstyles:\n0: 0",
                "size: 4x1  cursor: 0,0\nhi",
            ),
        ] {
            let screen = Screen::parse(written).unwrap_or_else(|e| panic!("{written:?}: {e}"));
            let expected = Screen::parse(canonical).unwrap();
            assert!(screen.diff(&expected).is_empty(), "{written:?}");
            assert_eq!(
                screen.with_styles().to_string(),
                expected.with_styles().to_string(),
                "{written:?}"
            );
        }
    }

    /// The header's row count decides where the grid ends, so these words
    /// are content when they fall inside it (#296).
    #[test]
    fn a_grid_may_contain_the_words_of_a_styles_block() {
        let saved = "size: 8x3  cursor: 0,0\n\nstyles:\n(none)";
        let screen = Screen::parse(saved).unwrap();
        assert_eq!(screen.row_text(1).trim_end(), "styles:");
        assert_eq!(screen.row_text(2).trim_end(), "(none)");
        assert_eq!(screen.to_string(), saved);
        // Even a row shaped like a style span is text when it is in the grid.
        let spans = "size: 12x2  cursor: 0,0\n0: 0-1 bold\n1: 5 reverse";
        let screen = Screen::parse(spans).unwrap();
        assert_eq!(screen.row_text(0).trim_end(), "0: 0-1 bold");
        assert!(screen.cell(0, 0).unwrap().style().is_default());
        assert_eq!(screen.to_string(), spans);
    }

    /// The one input the row-count rule cannot read both ways: a grid whose
    /// trailing blank rows were trimmed by hand *and* which carries a styles
    /// block. Content wins, and the leftover line is named rather than
    /// silently swallowed.
    #[test]
    fn a_trimmed_grid_with_a_styles_block_is_read_as_content() {
        // Wide enough to hold the word: `styles:` becomes row 2, and the
        // span line after it has nowhere left to belong.
        let err = Screen::parse("size: 12x3  cursor: hidden\nhi\n\nstyles:\n(none)")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("line 5") && err.contains("expected `styles:`"),
            "{err}"
        );
        // Too narrow to hold it, and the same reading fails one line
        // earlier — on the row itself rather than on what follows it.
        let narrow = Screen::parse("size: 4x3  cursor: hidden\nhi\n\nstyles:\n(none)")
            .unwrap_err()
            .to_string();
        assert!(
            narrow.contains("line 4") && narrow.contains("wider"),
            "{narrow}"
        );
    }

    /// Six bytes of hex is not six hex digits (#299).
    #[test]
    fn a_malformed_colour_is_an_error_not_a_panic() {
        for token in [
            "fg=#a\u{20ac}bc",
            "bg=#a\u{20ac}bc",
            "fg=#12345",
            "fg=#zzzzzz",
            "fg=300",
        ] {
            let input = format!("size: 2x2  cursor: 0,0\nx\n\nstyles:\n0: 0 {token}");
            let err = Screen::parse(&input).unwrap_err().to_string();
            assert!(err.contains("line 5"), "{token}: {err}");
        }
        // The valid forms still parse.
        let ok =
            Screen::parse("size: 2x2  cursor: 0,0\nxy\n\n\nstyles:\n0: 0 fg=4 bg=#1e1e2e").unwrap();
        assert_eq!(ok.cell(0, 0).unwrap().style().fg, Color::Indexed(4));
        assert_eq!(
            ok.cell(0, 0).unwrap().style().bg,
            Color::Rgb(0x1e, 0x1e, 0x2e)
        );
    }

    /// A combining mark after a wide character belongs to the cell that owns
    /// the glyph, not to its continuation half (#297).
    #[test]
    fn a_combining_mark_follows_a_wide_character() {
        let screen = Screen::parse("size: 6x1  cursor: 0,0\n\u{6771}\u{301}X").unwrap();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "\u{6771}\u{301}");
        assert!(screen.cell(0, 0).unwrap().is_wide());
        assert!(screen.cell(0, 1).unwrap().is_wide_continuation());
        assert_eq!(screen.cell(0, 2).unwrap().contents(), "X");
        // Several marks in a row, and a wide character at the right margin.
        let many = Screen::parse("size: 4x1  cursor: 0,0\nab\u{6771}\u{301}\u{302}").unwrap();
        assert_eq!(
            many.cell(0, 2).unwrap().contents(),
            "\u{6771}\u{301}\u{302}"
        );
        assert!(many.cell(0, 3).unwrap().is_wide_continuation());
    }

    /// A narrowing resize strands a wide character in the last column with
    /// no second half, and releases on vt100 0.16 saved the row that way
    /// (#643). It reads as that cell; a mark after it joins it; a character
    /// after it is past the edge.
    #[test]
    fn a_wide_character_stranded_in_the_last_column_reads() {
        let screen = Screen::parse("size: 3x1  cursor: 0,0\nab\u{6771}").unwrap();
        let last = screen.cell(0, 2).unwrap();
        assert_eq!(last.contents(), "\u{6771}");
        assert!(last.is_wide() && !last.is_wide_continuation());
        assert_eq!(screen.to_string(), "size: 3x1  cursor: 0,0\nab\u{6771}");
        let marked = Screen::parse("size: 3x1  cursor: 0,0\nab\u{6771}\u{301}").unwrap();
        assert_eq!(marked.cell(0, 2).unwrap().contents(), "\u{6771}\u{301}");
        for past in ["ab\u{6771}x", "ab\u{6771}\u{6771}", "abc\u{6771}"] {
            let err = Screen::parse(&format!("size: 3x1  cursor: 0,0\n{past}")).unwrap_err();
            assert!(
                err.to_string().contains("wider than the 3 columns"),
                "{past}: {err}"
            );
        }
    }

    /// A stranded wide character that is *not* in the last column: 0.11.6
    /// widened the row again and the program redrew after it (#683). Laid
    /// out two columns wide the row is too long, so it reads with the
    /// character as one stranded cell, as the JSON twin holds it.
    #[test]
    fn a_wide_character_stranded_before_other_text_reads_when_the_row_needs_it() {
        let text = "size: 8x2  cursor: 0,7\nabcd\u{6771}Zyw\nz";
        let screen = Screen::parse(text).unwrap();
        let stranded = screen.cell(0, 4).unwrap();
        assert_eq!(stranded.contents(), "\u{6771}");
        assert!(stranded.is_wide() && !stranded.is_wide_continuation());
        assert_eq!(screen.cell(0, 5).unwrap().contents(), "Z");
        assert_eq!(screen.cell(0, 7).unwrap().contents(), "w");
        assert_eq!(screen.to_string(), text);
        assert_eq!(
            screen.with_styles().to_string(),
            format!("{text}\n\nstyles:\n(none)")
        );
        // A mark after it joins it.
        let marked = Screen::parse("size: 5x1  cursor: 0,0\nab\u{6771}\u{301}cd").unwrap();
        assert_eq!(marked.cell(0, 2).unwrap().contents(), "\u{6771}\u{301}");
        assert_eq!(marked.cell(0, 3).unwrap().contents(), "c");
        // As many as the row needs, the leftmost first but one in the first
        // column last, since no resize strands it (#782); the rest keep
        // their second column, and one landing in the last column strands
        // there.
        let two = Screen::parse("size: 7x1  cursor: 0,0\n\u{6771}a\u{6771}b\u{6771}c").unwrap();
        assert_eq!(two.row_text(0), "\u{6771}a\u{6771}b\u{6771}c");
        assert!(two.cell(0, 1).unwrap().is_wide_continuation());
        assert!(!two.cell(0, 4).unwrap().is_wide_continuation());
        assert!(!two.cell(0, 6).unwrap().is_wide_continuation());
        assert_eq!(two.cell(0, 6).unwrap().contents(), "c");
        let edge = Screen::parse("size: 6x1  cursor: 0,0\n\u{6771}ab\u{6771}\u{6771}").unwrap();
        assert!(edge.cell(0, 1).unwrap().is_wide_continuation());
        assert_eq!(edge.cell(0, 4).unwrap().contents(), "\u{6771}");
        assert!(!edge.cell(0, 5).unwrap().is_wide_continuation());
        assert_eq!(edge.cell(0, 5).unwrap().contents(), "\u{6771}");
        // The first column's is stranded when nothing else fits, as before.
        let only = Screen::parse("size: 4x1  cursor: 0,0\n\u{6771}abc").unwrap();
        assert!(only.cell(0, 0).unwrap().is_wide());
        assert_eq!(only.cell(0, 1).unwrap().contents(), "a");
        // Five fit eight columns, one stranded and one in the last column.
        let five = "\u{6771}".repeat(5);
        let five = Screen::parse(&format!("size: 8x1  cursor: 0,0\n{five}")).unwrap();
        assert_eq!(five.row_text(0), "\u{6771}".repeat(5));
        // Too long even with every wide character stranded, or too long with
        // none to strand: still refused.
        let nine = "\u{6771}".repeat(9);
        for past in ["abcd\u{6771}Zywx", "abcdefghi", &nine] {
            let err = Screen::parse(&format!("size: 8x1  cursor: 0,0\n{past}")).unwrap_err();
            assert!(
                err.to_string().contains("wider than the 8 columns"),
                "{past}: {err}"
            );
        }
    }

    /// The same 0.11.6 write on a 10-column screen leaves a row that fits
    /// at two columns per wide character, and that is how it reads: the
    /// text does not say `東` lost its second half, and the same line is
    /// what every capture of an unbroken `東` before `Zyw` writes. So it
    /// renders back byte for byte while its JSON twin, which carries the
    /// flags, has `Z` one column further left (#683).
    #[test]
    fn a_row_that_fits_reads_its_wide_characters_whole() {
        let text = "size: 10x1  cursor: 0,8\nabcd\u{6771}Zyw";
        let screen = Screen::parse(text).unwrap();
        assert!(screen.cell(0, 5).unwrap().is_wide_continuation());
        assert_eq!(screen.cell(0, 6).unwrap().contents(), "Z");
        assert_eq!(screen.to_string(), text);
    }

    /// The emulator gives `⚠️` two columns since vt100 0.18, so a row with
    /// one reads back with the same columns; a row that only fits at one
    /// column per such cell is what earlier releases wrote, and still reads
    /// (#580).
    #[test]
    fn an_emoji_presentation_selector_widens_its_cell_unless_the_row_cannot_hold_it() {
        let screen = Screen::parse("size: 4x1  cursor: 0,0\n\u{26a0}\u{fe0f}X").unwrap();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "\u{26a0}\u{fe0f}");
        assert!(screen.cell(0, 0).unwrap().is_wide());
        assert!(screen.cell(0, 1).unwrap().is_wide_continuation());
        assert_eq!(screen.cell(0, 2).unwrap().contents(), "X");
        // Four columns of text at one column per selector cell: a 0.11.6
        // row, read the way 0.11.6 laid it out.
        let earlier = Screen::parse("size: 4x1  cursor: 0,0\n\u{26a0}\u{fe0f}XYZ").unwrap();
        assert!(!earlier.cell(0, 0).unwrap().is_wide());
        assert_eq!(earlier.cell(0, 1).unwrap().contents(), "X");
        assert_eq!(earlier.cell(0, 3).unwrap().contents(), "Z");
        // A row too wide either way is still refused.
        assert!(Screen::parse("size: 4x1  cursor: 0,0\n\u{26a0}\u{fe0f}XYZW").is_err());
    }

    /// `unicode_width` answers `None` for a control character and `Some(0)`
    /// for a combining mark, and only the second joins a cell (#376).
    #[test]
    fn a_control_character_is_refused_where_a_combining_mark_joins() {
        for ch in ['\u{1b}', '\u{1}', '\t'] {
            let input = format!("size: 8x1  cursor: 0,0\na{ch}b");
            let err = Screen::parse(&input).unwrap_err();
            assert!(matches!(err, Error::Parse(_)), "{ch:?}: {err}");
            let text = err.to_string();
            assert!(
                text.contains("line 2") && text.contains(&format!("{ch:?}")),
                "{ch:?}: {text}"
            );
        }
        // A zero-width mark still joins the cell before it, the way it
        // does after the wide character above.
        let screen = Screen::parse("size: 8x1  cursor: 0,0\na\u{301}b").unwrap();
        assert_eq!(screen.cell(0, 0).unwrap().contents(), "a\u{301}");
        assert_eq!(screen.cell(0, 1).unwrap().contents(), "b");
    }

    /// `Display for Color` is the forward half of the token `parse_color`
    /// reads (#315): the two halves of one documented format agree.
    #[test]
    fn a_colour_displays_as_the_token_the_parser_reads() {
        for color in [
            Color::Indexed(0),
            Color::Indexed(4),
            Color::Indexed(208),
            Color::Indexed(255),
            Color::Rgb(0x1e, 0x1e, 0x2e),
            Color::Rgb(0, 0, 0),
            Color::Rgb(255, 0, 7),
        ] {
            let token = color.to_string();
            assert_eq!(parse_color(&token), Some(color), "{token}");
        }
        assert_eq!(Color::Indexed(4).to_string(), "4");
        assert_eq!(Color::Rgb(0x1e, 0x1e, 0x2e).to_string(), "#1e1e2e");
        // The block never writes a default colour — absence means default —
        // so the parser does not read the word back; it is for messages.
        assert_eq!(Color::Default.to_string(), "default");
        assert_eq!(parse_color("default"), None);
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
        assert!(junk.contains("line 3: expected `styles:`"), "{junk}");
        // The lines after the grid are walked, not gathered (#779), and keep
        // their numbers past blank lines and a block of `(none)`.
        let late = Screen::parse("size: 2x1  cursor: 0,0\nab\n\n\n\nbogus")
            .unwrap_err()
            .to_string();
        assert!(late.contains("line 6: expected `styles:`"), "{late}");
        let span =
            Screen::parse("size: 2x3  cursor: 0,0\nab\ncd\nef\n\nstyles:\n(none)\n\n0: 0 shiny")
                .unwrap_err()
                .to_string();
        assert!(
            span.contains("line 9") && span.contains("unknown style token"),
            "{span}"
        );
        let trimmed = Screen::parse("size: 2x3  cursor: 0,0\nab\n")
            .expect("a trimmed grid")
            .with_styles()
            .to_string();
        assert_eq!(trimmed, "size: 2x3  cursor: 0,0\nab\n\n\n\nstyles:\n(none)");
    }
}
