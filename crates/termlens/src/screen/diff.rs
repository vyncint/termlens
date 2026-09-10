//! [`Screen::diff`]: what changed between two screens, rendered as only
//! the rows that changed with the changed columns marked (#246). A 24-row
//! grid printed twice hides exactly the failure a TUI test produces — one
//! cell moved, one colour changed — and the derived `Debug` of a `Screen`
//! is deliberately the compact `Display`, right for one screen and
//! unhelpful for two. This is the other half of the "readable failures"
//! promise the embedded screens started.

use std::fmt;

use super::{same_cursor, Cell, Screen, Style};

/// The difference between two screens. Built by [`Screen::diff`]; render it
/// with `{}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenDiff {
    before_size: (u16, u16),
    after_size: (u16, u16),
    before_cursor: (u16, u16, bool),
    after_cursor: (u16, u16, bool),
    /// `(row, col, before, after)` for every cell — within the overlap of
    /// the two grids — whose contents or style differ, in reading order.
    cells: Vec<(u16, u16, Cell, Cell)>,
    /// Rows within the overlap with no changed cell.
    unchanged_rows: u16,
    /// The `styles:` runs of each changed row, before and after, where
    /// they differ.
    style_changes: Vec<(u16, String, String)>,
    /// The rows of the taller screen whose text is in the diff.
    rows: Vec<(u16, String, String, Vec<u16>)>,
}

impl Screen {
    /// What changed from `self` to `other`: every cell whose contents or
    /// style differ, the cursor and size deltas, and the style runs of the
    /// rows that changed. Empty when the two screens show the same picture
    /// — size, cursor and every cell — which is narrower than `==`, since
    /// the out-of-band state (bells, repaints, title) is not a picture.
    ///
    /// The documented way to compare two screens outside insta:
    ///
    /// ```
    /// # fn main() -> termlens::Result<()> {
    /// # let mut t = termlens::Terminal::builder().args(["-c", "printf 'Counter: 1'; read q"]).spawn("sh")?;
    /// # t.wait_until(|s| s.contains("Counter: 1"))?;
    /// let a = t.screen();
    /// let b = t.screen();
    /// assert!(a.diff(&b).is_empty(), "{}", a.diff(&b));
    /// # t.send(termlens::Key::Enter); t.wait_exit()?; Ok(())
    /// # }
    /// ```
    ///
    /// Two cells are the same when they look the same: an erased cell and a
    /// written space in the same style are one blank on every terminal, so
    /// they do not differ here even though [`Cell::contents`] distinguishes
    /// them (`""` against `" "`). That is what lets a screen rendered through
    /// a PTY be held against one built cell by cell, where every blank was
    /// written.
    ///
    /// Sizes that differ diff what overlaps, and the rendering says what was
    /// clipped. The rendering is plain text — no colour, so CI logs stay
    /// readable — and shows only the rows that changed, with a marker line
    /// under the changed columns and the style runs before and after.
    #[must_use]
    pub fn diff(&self, other: &Screen) -> ScreenDiff {
        let cols = self.cols().min(other.cols());
        let rows = self.rows().min(other.rows());
        let mut cells = Vec::new();
        let mut rows_out = Vec::new();
        let mut style_changes = Vec::new();
        let mut unchanged_rows = 0;
        for row in 0..rows {
            let mut changed_cols = Vec::new();
            for col in 0..cols {
                let (Some(a), Some(b)) = (self.cell(row, col), other.cell(row, col)) else {
                    continue;
                };
                if !same_picture(a, b) {
                    changed_cols.push(col);
                    cells.push((row, col, a.clone(), b.clone()));
                }
            }
            if changed_cols.is_empty() {
                unchanged_rows += 1;
                continue;
            }
            rows_out.push((
                row,
                self.row_text(row).trim_end().to_owned(),
                other.row_text(row).trim_end().to_owned(),
                changed_cols,
            ));
            let before = row_styles(self, row);
            let after = row_styles(other, row);
            if before != after {
                style_changes.push((row, before, after));
            }
        }
        ScreenDiff {
            before_size: self.size(),
            after_size: other.size(),
            before_cursor: self.cursor(),
            after_cursor: other.cursor(),
            cells,
            unchanged_rows,
            style_changes,
            rows: rows_out,
        }
    }
}

/// Whether two cells draw the same thing: equal, or both blank in the same
/// style — an erased cell (`""`) and a written space (`" "`) are the one
/// picture a terminal can show for either.
fn same_picture(a: &Cell, b: &Cell) -> bool {
    let blank = |cell: &Cell| matches!(cell.contents(), "" | " ");
    a.style() == b.style()
        && a.is_wide() == b.is_wide()
        && a.is_wide_continuation() == b.is_wide_continuation()
        && (a.contents() == b.contents() || (blank(a) && blank(b)))
}

/// One row's `styles:` runs, `(none)` for an all-default row — the same
/// tokens `with_styles` renders, so a diff and a snapshot agree.
pub(super) fn row_styles(screen: &Screen, row: u16) -> String {
    let mut spans: Vec<String> = Vec::new();
    let mut run: Option<(u16, u16, Style)> = None;
    let flush = |run: Option<(u16, u16, Style)>, spans: &mut Vec<String>| {
        if let Some((start, end, style)) = run {
            if !style.is_default() {
                let range = if start == end {
                    format!("{start}")
                } else {
                    format!("{start}-{end}")
                };
                spans.push(format!("{range} {}", style.tokens()));
            }
        }
    };
    for col in 0..screen.cols() {
        let style = screen
            .cell(row, col)
            .map_or_else(Style::default, |cell| *cell.style());
        match &mut run {
            Some((_, end, current)) if *current == style => *end = col,
            _ => {
                flush(run.take(), &mut spans);
                run = Some((col, col, style));
            }
        }
    }
    flush(run, &mut spans);
    if spans.is_empty() {
        "(none)".to_owned()
    } else {
        spans.join("; ")
    }
}

impl ScreenDiff {
    /// True when the two screens show the same picture: same size, same
    /// cursor, every cell equal.
    ///
    /// A *hidden* cursor's coordinates are not part of the picture — it
    /// draws nothing, and the snapshot text format does not record where it
    /// was, so a screen parsed back from its own snapshot used to report a
    /// difference no one could see (#298). Visibility itself is compared,
    /// and so is a visible cursor's position.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
            && self.before_size == self.after_size
            && same_cursor(self.before_cursor, self.after_cursor)
    }

    /// Every changed cell as `(row, col, before, after)`, in reading order,
    /// within the overlap of the two grids.
    pub fn cells(&self) -> impl Iterator<Item = (u16, u16, &Cell, &Cell)> {
        self.cells.iter().map(|(r, c, a, b)| (*r, *c, a, b))
    }

    /// The rows with at least one changed cell, ascending, each once —
    /// so "the highlight moved from row 1 to row 2 and nothing else
    /// changed" is `assert_eq!(diff.changed_rows().collect::<Vec<_>>(),
    /// [1, 2])` rather than a dedup over [`cells`](Self::cells).
    ///
    /// Within the overlap of the two grids, like `cells`; a size or cursor
    /// change alone leaves this empty while [`is_empty`](Self::is_empty)
    /// is false.
    pub fn changed_rows(&self) -> impl Iterator<Item = u16> + '_ {
        self.rows.iter().map(|(row, ..)| *row)
    }

    /// The `styles:` runs of each changed row whose runs differ, as
    /// `(row, before, after)` in the tokens [`Screen::with_styles`]
    /// writes (`(none)` for an all-default row), ascending by row. A row
    /// whose text changed under unchanged styles is not listed; a row
    /// whose styles changed under unchanged text is, since a changed
    /// style is a changed cell.
    pub fn style_changes(&self) -> impl Iterator<Item = (u16, &str, &str)> {
        self.style_changes
            .iter()
            .map(|(row, before, after)| (*row, before.as_str(), after.as_str()))
    }
}

/// `a → b` when they differ, `a` alone when they do not.
fn arrow<T: PartialEq + fmt::Display>(a: T, b: T) -> String {
    if a == b {
        a.to_string()
    } else {
        format!("{a} → {b}")
    }
}

fn cursor_text((row, col, visible): (u16, u16, bool)) -> String {
    if visible {
        format!("{row},{col}")
    } else {
        "hidden".to_owned()
    }
}

impl fmt::Display for ScreenDiff {
    /// Header with the size and cursor deltas; then, for each changed row,
    /// the row before and after side by side over a marker line with `^`
    /// under every changed column; a count of unchanged rows; and the
    /// style runs of the changed rows before and after, where they differ.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return write!(f, "no difference");
        }
        writeln!(
            f,
            "size: {}   cursor: {}",
            arrow(
                format!("{}x{}", self.before_size.0, self.before_size.1),
                format!("{}x{}", self.after_size.0, self.after_size.1)
            ),
            arrow(
                cursor_text(self.before_cursor),
                cursor_text(self.after_cursor)
            )
        )?;
        if self.before_size != self.after_size {
            let cols = self.before_size.0.min(self.after_size.0);
            let rows = self.before_size.1.min(self.after_size.1);
            writeln!(
                f,
                "compared over the {cols}x{rows} overlap; the rest is clipped"
            )?;
        }
        let width = usize::from(self.before_size.0.min(self.after_size.0));
        for (row, before, after, changed) in &self.rows {
            writeln!(f, "{row:>3} │{before:<width$}│{after}")?;
            let mut marks = vec![' '; width];
            for &col in changed {
                if let Some(mark) = marks.get_mut(usize::from(col)) {
                    *mark = '^';
                }
            }
            let marks: String = marks.into_iter().collect();
            let marks = marks.trim_end();
            writeln!(f, "    │{marks:<width$}│{marks}")?;
        }
        if self.unchanged_rows > 0 {
            writeln!(f, "… {} rows unchanged", self.unchanged_rows)?;
        }
        for (row, before, after) in &self.style_changes {
            writeln!(f, "styles: {row}: {before} → {after}")?;
        }
        Ok(())
    }
}
