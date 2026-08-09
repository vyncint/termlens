//! The rendered screen grid: [`Screen`], [`Cell`], [`Style`], [`Color`].
//!
//! A [`Screen`] is an immutable snapshot of the emulated terminal at one
//! moment. It is a cheap-to-clone value type (the grid is behind an [`Arc`]),
//! so errors and assertions can carry whole screens around freely.

use std::fmt;
use std::sync::Arc;

/// A terminal color, as reported by the emulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    /// The terminal's default foreground/background.
    #[default]
    Default,
    /// A palette color (0–255).
    Indexed(u8),
    /// A 24-bit RGB color.
    Rgb(u8, u8, u8),
}

/// Visual attributes of a [`Cell`].
///
/// Captured for every cell in v0.1; the textual snapshot format does not
/// render them yet (a `with_styles()` styles block is planned for v0.2 — see
/// `docs/DESIGN.md`), but assertions can inspect them via [`Cell::style`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style {
    /// Foreground color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
    /// Bold / increased intensity.
    pub bold: bool,
    /// Dim / decreased intensity.
    pub dim: bool,
    /// Italic.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
    /// Reverse video (foreground and background swapped).
    pub reverse: bool,
}

/// One cell of the screen grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    contents: String,
    style: Style,
    wide: bool,
    wide_continuation: bool,
}

impl Cell {
    pub(crate) fn new(contents: String, style: Style, wide: bool, wide_continuation: bool) -> Self {
        Self {
            contents,
            style,
            wide,
            wide_continuation,
        }
    }

    /// The cell's text: usually a single grapheme (possibly with combining
    /// characters). Empty for blank cells and for wide-continuation cells.
    #[must_use]
    pub fn contents(&self) -> &str {
        &self.contents
    }

    /// The cell's visual attributes.
    #[must_use]
    pub fn style(&self) -> &Style {
        &self.style
    }

    /// True if the cell holds a double-width character (CJK, most emoji).
    /// The following cell is then a wide-continuation placeholder.
    #[must_use]
    pub fn is_wide(&self) -> bool {
        self.wide
    }

    /// True if this cell is the placeholder occupying the second column of a
    /// double-width character.
    #[must_use]
    pub fn is_wide_continuation(&self) -> bool {
        self.wide_continuation
    }
}

/// An immutable snapshot of the terminal screen.
///
/// Cheap to clone (the grid is shared behind an [`Arc`]); every clone
/// observes the same instant. Coordinates are `(row, col)`, zero-based,
/// with `(0, 0)` at the top left. [`Screen::size`] follows the terminal
/// convention of *columns × rows* instead — the same order as
/// [`TerminalBuilder::size`](crate::TerminalBuilder::size).
///
/// The [`Display`](fmt::Display) rendering is the snapshot format documented
/// in `docs/DESIGN.md`: a header line, then the grid with trailing
/// whitespace stripped per line. Trailing blanks are *preserved* inside the
/// grid itself, so coordinate queries like [`Screen::cell`] and
/// [`Screen::find`] are unaffected by the trimming.
#[derive(Clone)]
pub struct Screen {
    cols: u16,
    rows: u16,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
    cells: Arc<[Cell]>,
}

impl Screen {
    pub(crate) fn from_parts(
        cols: u16,
        rows: u16,
        cursor_row: u16,
        cursor_col: u16,
        cursor_visible: bool,
        cells: Vec<Cell>,
    ) -> Self {
        debug_assert_eq!(cells.len(), usize::from(cols) * usize::from(rows));
        Self {
            cols,
            rows,
            cursor_row,
            cursor_col,
            cursor_visible,
            cells: cells.into(),
        }
    }

    /// Number of columns (the screen's width).
    #[must_use]
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Number of rows (the screen's height).
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Screen size as `([cols](Self::cols), [rows](Self::rows))` — width ×
    /// height, matching [`TerminalBuilder::size`](crate::TerminalBuilder::size).
    /// Note the order differs from cell addressing, which is `(row, col)`;
    /// prefer the named accessors when in doubt.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Cursor position and visibility: `(row, col, visible)`.
    #[must_use]
    pub fn cursor(&self) -> (u16, u16, bool) {
        (self.cursor_row, self.cursor_col, self.cursor_visible)
    }

    /// The cell at `(row, col)`, or `None` when out of bounds.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.cells
            .get(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }

    /// The text of one row, blank cells rendered as spaces, trailing
    /// whitespace **included**. Returns an empty string for out-of-bounds
    /// rows.
    ///
    /// Wide characters contribute their character once; their continuation
    /// cell contributes nothing (so the string's *display width* matches the
    /// row, not its `char` count).
    #[must_use]
    pub fn row_text(&self, row: u16) -> String {
        let mut out = String::with_capacity(usize::from(self.cols));
        for col in 0..self.cols {
            let Some(cell) = self.cell(row, col) else {
                return out;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            if cell.contents().is_empty() {
                out.push(' ');
            } else {
                out.push_str(cell.contents());
            }
        }
        out
    }

    /// The whole grid as text: one line per row, trailing whitespace
    /// stripped per line, rows joined with `\n`. This is exactly the body of
    /// the [`Display`](fmt::Display) rendering, without the header.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        for row in 0..self.rows {
            if row > 0 {
                out.push('\n');
            }
            let line = self.row_text(row);
            out.push_str(line.trim_end());
        }
        out
    }

    /// True if `needle` occurs in the rendered text ([`Screen::text`]).
    ///
    /// Because rows are joined with `\n`, multi-line needles match across
    /// consecutive rows (with trailing whitespace stripped per row).
    #[must_use]
    pub fn contains(&self, needle: &str) -> bool {
        self.text().contains(needle)
    }

    /// Locate the first occurrence of `needle` scanning rows top to bottom;
    /// returns the `(row, col)` of its first character.
    ///
    /// The needle must fit within a single row (use [`Screen::contains`] for
    /// multi-line matches). Columns account for double-width characters: a
    /// match after a CJK character reports the real terminal column.
    #[must_use]
    pub fn find(&self, needle: &str) -> Option<(u16, u16)> {
        if needle.is_empty() {
            return Some((0, 0));
        }
        for row in 0..self.rows {
            let text = self.row_text(row);
            let Some(byte_off) = text.find(needle) else {
                continue;
            };
            // Map the byte offset back to the column of the cell that
            // contributed that byte.
            let mut acc = 0usize;
            for col in 0..self.cols {
                let cell = self.cell(row, col)?;
                let len = if cell.is_wide_continuation() {
                    0
                } else if cell.contents().is_empty() {
                    1 // rendered as a space
                } else {
                    cell.contents().len()
                };
                if len > 0 && byte_off < acc + len {
                    return Some((row, col));
                }
                acc += len;
            }
        }
        None
    }
}

impl fmt::Debug for Screen {
    /// Deliberately compact: the header plus the rendered text, exactly like
    /// [`Display`](fmt::Display). The derived alternative — thousands of
    /// [`Cell`]s on one line — makes `Err(Error::Timeout { .. })` in a
    /// `Result`-returning test unreadable (and long enough that CI log
    /// pipelines drop the line entirely). Use [`Screen::cell`] to inspect
    /// individual cells.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Screen({self})")
    }
}

impl fmt::Display for Screen {
    /// The snapshot text format: `size: <cols>x<rows>  cursor: <row>,<col>`
    /// (or `cursor: hidden`), then the grid verbatim, one terminal row per
    /// line with trailing whitespace stripped.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "size: {}x{}  cursor: ", self.cols, self.rows)?;
        if self.cursor_visible {
            write!(f, "{},{}", self.cursor_row, self.cursor_col)?;
        } else {
            write!(f, "hidden")?;
        }
        write!(f, "\n{}", self.text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a screen from rows of text; `'*'` becomes a styled bold cell,
    /// wide characters get a proper continuation cell.
    fn screen(cols: u16, rows: u16, lines: &[&str]) -> Screen {
        use unicode_width::UnicodeWidthChar;

        let mut cells: Vec<Cell> = Vec::new();
        for r in 0..usize::from(rows) {
            let mut row_cells: Vec<Cell> = Vec::new();
            if let Some(line) = lines.get(r) {
                for ch in line.chars() {
                    let wide = ch.width().unwrap_or(1) == 2;
                    let style = Style {
                        bold: ch == '*',
                        ..Style::default()
                    };
                    row_cells.push(Cell::new(ch.to_string(), style, wide, false));
                    if wide {
                        row_cells.push(Cell::new(String::new(), Style::default(), false, true));
                    }
                }
            }
            assert!(row_cells.len() <= usize::from(cols), "test line too long");
            while row_cells.len() < usize::from(cols) {
                row_cells.push(Cell::new(String::new(), Style::default(), false, false));
            }
            cells.extend(row_cells);
        }
        Screen::from_parts(cols, rows, 1, 2, true, cells)
    }

    #[test]
    fn row_text_pads_blanks_and_skips_continuations() {
        let s = screen(10, 2, &["ab", "汉x"]);
        assert_eq!(s.row_text(0), "ab        ");
        // 汉 is wide: one char + continuation, then x, then 7 blanks.
        assert_eq!(s.row_text(1), "汉x       ");
        assert_eq!(s.row_text(9), "");
    }

    #[test]
    fn text_strips_trailing_whitespace_per_line() {
        let s = screen(10, 3, &["ab", "", "c"]);
        assert_eq!(s.text(), "ab\n\nc");
    }

    #[test]
    fn contains_matches_across_rows() {
        let s = screen(10, 2, &["hello", "world"]);
        assert!(s.contains("hello"));
        assert!(s.contains("hello\nworld"));
        assert!(!s.contains("hello world"));
    }

    #[test]
    fn find_reports_wide_aware_columns() {
        let s = screen(10, 2, &["abc", "汉字x"]);
        assert_eq!(s.find("bc"), Some((0, 1)));
        // 汉 occupies cols 0-1, 字 occupies 2-3, x sits at col 4.
        assert_eq!(s.find("x"), Some((1, 4)));
        assert_eq!(s.find("字"), Some((1, 2)));
        assert_eq!(s.find("missing"), None);
    }

    #[test]
    fn cell_and_cursor_accessors() {
        let s = screen(10, 2, &["a*"]);
        assert_eq!(s.cell(0, 0).unwrap().contents(), "a");
        assert!(s.cell(0, 1).unwrap().style().bold);
        assert!(s.cell(2, 0).is_none());
        assert!(s.cell(0, 10).is_none());
        assert_eq!(s.cursor(), (1, 2, true));
        assert_eq!(s.size(), (10, 2));
        assert_eq!((s.cols(), s.rows()), (10, 2));
    }

    #[test]
    fn display_format_matches_spec() {
        let s = screen(10, 2, &["hi"]);
        assert_eq!(format!("{s}"), "size: 10x2  cursor: 1,2\nhi\n");
    }
}
