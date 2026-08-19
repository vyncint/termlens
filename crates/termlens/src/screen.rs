//! The rendered screen grid: [`Screen`], [`Cell`], [`Style`], [`Color`].
//!
//! A [`Screen`] is an immutable snapshot of the emulated terminal at one
//! moment. It is a cheap-to-clone value type (the grid is behind an [`Arc`]),
//! so errors and assertions can carry whole screens around freely.

use std::fmt;
use std::ops::{Bound, RangeBounds};
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
/// Inspect them per cell via [`Cell::style`], or snapshot them wholesale
/// with [`Screen::with_styles`] (plain snapshots stay text-only).
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
    /// Blinking (`SGR 5`/`6`; the two rates are not distinguished).
    pub blink: bool,
    /// Concealed: the cell holds text the terminal does not display
    /// (`SGR 8`) — a masked password field, typically.
    ///
    /// This is the attribute worth checking explicitly. Without it, a test
    /// asserting that a field is masked passes just as happily against an
    /// application that printed the secret in clear, because the two
    /// renderings are identical in the grid. [`Screen::cell`] still reports
    /// the underlying text, exactly as a real terminal holds it — what
    /// changes is that you can now tell the difference.
    pub conceal: bool,
    /// Struck through (`SGR 9`).
    pub strikethrough: bool,
}

/// Which mouse events the application asked its terminal to report.
///
/// Read it from a snapshot via [`Screen::mouse_mode`];
/// [`Terminal::click`](crate::Terminal::click) and
/// [`Terminal::scroll`](crate::Terminal::scroll) consult the same state to
/// encode exactly what the application expects.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MouseMode {
    /// No mouse tracking enabled.
    #[default]
    None,
    /// X10 mode (`CSI ?9 h`): presses only.
    Press,
    /// VT200 mode (`CSI ?1000 h`): presses and releases.
    PressRelease,
    /// Button-event tracking (`CSI ?1002 h`): presses, releases, and motion
    /// while a button is held down.
    ButtonMotion,
    /// Any-event tracking (`CSI ?1003 h`): presses, releases, and all
    /// motion.
    AnyMotion,
}

/// What an application copied with `OSC 52`, as observed at one snapshot.
///
/// Read it from a snapshot via [`Screen::clipboard`]. A toast on screen
/// proves the copy path ran; this proves the payload, which is usually the
/// behaviour actually under test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clipboard {
    targets: Arc<str>,
    text: Option<Arc<str>>,
}

impl Clipboard {
    pub(crate) fn new(targets: &str, text: Option<String>) -> Self {
        Self {
            targets: Arc::from(targets),
            text: text.map(Arc::from),
        }
    }

    /// The copied text, or `None` when the payload was not usable text.
    ///
    /// `None` means the application sent something termlens could not
    /// decode: base64 with invalid characters or a broken length, bytes
    /// that are not valid UTF-8, or a payload past the capture bound. It is
    /// deliberately distinct from `Some("")`, which is a real write of
    /// nothing — the way an application clears the clipboard.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// The selections written to, exactly as the application named them:
    /// `c` (clipboard), `p` (primary), `q`, `s`, or `0`–`7`, in any
    /// combination — an application writing to the wrong one is a real bug
    /// worth catching.
    ///
    /// Empty means the application named none, in which case a real
    /// terminal picks its default (xterm: clipboard *and* primary).
    #[must_use]
    pub fn targets(&self) -> &str {
        &self.targets
    }
}

/// Out-of-band terminal state captured with each snapshot. Deliberately
/// invisible in the text rendering (existing snapshot files stay valid);
/// exposed through the accessors on [`Screen`].
#[derive(Debug, Clone)]
pub(crate) struct TermState {
    pub(crate) title: Arc<str>,
    pub(crate) alternate_screen: bool,
    pub(crate) bracketed_paste: bool,
    pub(crate) application_cursor: bool,
    pub(crate) mouse: MouseMode,
    /// Behind an `Arc` deliberately: `Screen` is embedded in every
    /// `Error` and cloned on every wait, so its size is load-bearing.
    pub(crate) clipboard: Option<Arc<Clipboard>>,
    /// Rows that have scrolled off the top, oldest first, as text.
    ///
    /// Text rather than cells, deliberately: a thousand rows of styled
    /// cells per snapshot would dominate the cost of every wait, and
    /// history is asserted on for its content. The rows are shared, so a
    /// snapshot pays one `Arc` clone each.
    pub(crate) scrollback: Arc<[Arc<str>]>,
}

impl Default for TermState {
    fn default() -> Self {
        Self {
            title: Arc::from(""),
            alternate_screen: false,
            bracketed_paste: false,
            application_cursor: false,
            mouse: MouseMode::None,
            clipboard: None,
            scrollback: Arc::from([] as [Arc<str>; 0]),
        }
    }
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
    state: TermState,
}

impl Screen {
    pub(crate) fn from_parts(
        cols: u16,
        rows: u16,
        cursor_row: u16,
        cursor_col: u16,
        cursor_visible: bool,
        cells: Vec<Cell>,
        state: TermState,
    ) -> Self {
        debug_assert_eq!(cells.len(), usize::from(cols) * usize::from(rows));
        Self {
            cols,
            rows,
            cursor_row,
            cursor_col,
            cursor_visible,
            cells: cells.into(),
            state,
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

    /// The window title, as the application most recently set it (`OSC 0`
    /// or `OSC 2` — crossterm's `SetTitle`). Empty until the application
    /// sets one; `OSC 1` (icon name only) is ignored. termlens tracks the
    /// title itself, so it works regardless of the emulator backend.
    ///
    /// Like all out-of-band state, the title is not part of the
    /// [`Display`](fmt::Display) rendering — assert on it directly:
    /// `wait_until(|s| s.title() == "editor — draft.txt")`.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.state.title
    }

    /// True while the application has the alternate screen active (modes
    /// 47/1049) — the buffer full-screen TUIs switch to on startup and
    /// leave on exit, restoring the shell's scrollback.
    #[must_use]
    pub fn alternate_screen(&self) -> bool {
        self.state.alternate_screen
    }

    /// True while bracketed paste (mode 2004) is enabled.
    /// [`Terminal::paste`](crate::Terminal::paste) consults this: the text
    /// then arrives as one paste event instead of a burst of key presses.
    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.state.bracketed_paste
    }

    /// True while application cursor mode (DECCKM) is set.
    /// [`Terminal::send`](crate::Terminal::send) consults this: arrow keys
    /// then use their `ESC O` application forms.
    #[must_use]
    pub fn application_cursor(&self) -> bool {
        self.state.application_cursor
    }

    /// Which mouse events the application asked to be reported —
    /// [`MouseMode::None`] until it enables a tracking mode.
    /// [`Terminal::click`](crate::Terminal::click) and
    /// [`Terminal::scroll`](crate::Terminal::scroll) consult the same
    /// state, so their reports always match what the application expects.
    #[must_use]
    pub fn mouse_mode(&self) -> MouseMode {
        self.state.mouse
    }

    /// The most recent `OSC 52` clipboard write observed at this snapshot,
    /// or `None` if the application has not copied anything yet.
    ///
    /// Snapshot state, so it follows snapshot rules: the value is what the
    /// clipboard held at this observation, which makes a
    /// [`wait_frame`](crate::Terminal::wait_frame) or
    /// [`wait_until`](crate::Terminal::wait_until) predicate over a
    /// clipboard write well-defined.
    ///
    /// ```no_run
    /// # fn main() -> termlens::Result<()> {
    /// # let mut t = termlens::Terminal::builder().spawn("true")?;
    /// t.wait_until(|s| s.clipboard().is_some_and(|c| c.text() == Some("the title")))?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Clipboard *reads* (`OSC 52 ; c ; ?`) are a different sequence and
    /// are not answered: they stay named in timeout errors, so an
    /// application blocked on one is diagnosed rather than left hanging.
    #[must_use]
    pub fn clipboard(&self) -> Option<&Clipboard> {
        self.state.clipboard.as_deref()
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

    /// How many rows have scrolled off the top and are still retained.
    ///
    /// Zero when nothing has scrolled — or when the terminal was built with
    /// [`scrollback(0)`](crate::TerminalBuilder::scrollback). Caps at the
    /// configured length: past that, the oldest rows are dropped, and this
    /// stops growing rather than reporting everything the application ever
    /// wrote.
    #[must_use]
    pub fn scrollback_rows(&self) -> usize {
        self.state.scrollback.len()
    }

    /// The retained history as text: one line per scrolled-off row, oldest
    /// first, trailing whitespace stripped, joined with `\n`.
    ///
    /// Empty when nothing has scrolled. History is text only — a scrolled
    /// row has no [`Style`] and no [`cell`](Self::cell) addressing, which
    /// is what keeps a snapshot cheap enough to take on every wait.
    #[must_use]
    pub fn scrollback_text(&self) -> String {
        let mut out = String::new();
        for (i, row) in self.state.scrollback.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(row);
        }
        out
    }

    /// History **and** visible screen, as one text block: the retained
    /// scrolled-off rows followed by [`text`](Self::text).
    ///
    /// This is the accessor for the assertion an author actually writes —
    /// "this block reached the terminal, wherever it currently sits". An
    /// application that commits finished output into scrollback and keeps a
    /// small live region moves content between the two regions as it goes,
    /// so a test that has to know which region to look in is a test that
    /// breaks when the application scrolls one line further.
    #[must_use]
    pub fn full_text(&self) -> String {
        let mut out = self.scrollback_text();
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&self.text());
        out
    }

    /// True if `needle` occurs in the rendered text ([`Screen::text`]).
    ///
    /// Because rows are joined with `\n`, multi-line needles match across
    /// consecutive rows (with trailing whitespace stripped per row).
    ///
    /// The **visible screen only** — like every other query on this type.
    /// For content that may already have scrolled off, use
    /// [`full_text`](Self::full_text).
    #[must_use]
    pub fn contains(&self, needle: &str) -> bool {
        self.text().contains(needle)
    }

    /// Locate the first occurrence of `needle` scanning rows top to bottom;
    /// returns the `(row, col)` of its first character.
    ///
    /// Needles containing `\n` match across consecutive rows with exactly
    /// the semantics of [`Screen::contains`] (trailing whitespace stripped
    /// per row): a multi-row needle is found wherever `contains` would be
    /// true. A needle that *begins* with `\n` reports the position of its
    /// first character after those newlines.
    ///
    /// Columns account for double-width characters: a match after a CJK
    /// character reports the real terminal column.
    #[must_use]
    pub fn find(&self, needle: &str) -> Option<(u16, u16)> {
        if needle.is_empty() {
            return Some((0, 0));
        }
        if !needle.contains('\n') {
            for row in 0..self.rows {
                let text = self.row_text(row);
                if let Some(byte_off) = text.find(needle) {
                    return Some((row, self.col_of_byte(row, byte_off)?));
                }
            }
            return None;
        }

        // Multi-row: the needle is a substring of `text()` — its first
        // segment ends a row (after the trailing-whitespace trim), the
        // middle segments equal whole rows, the last starts one.
        let segments: Vec<&str> = needle.split('\n').collect();
        let extra = u16::try_from(segments.len() - 1).ok()?;
        for row in 0..self.rows.checked_sub(extra)? {
            let first_line = self.row_text(row);
            let first = first_line.trim_end();
            if !first.ends_with(segments[0]) {
                continue;
            }
            let tail_matches = segments[1..].iter().enumerate().all(|(i, seg)| {
                let line = self.row_text(row + 1 + i as u16);
                let line = line.trim_end();
                if i as u16 == extra - 1 {
                    line.starts_with(seg) // last segment: prefix
                } else {
                    line == *seg // middle segments: whole rows
                }
            });
            if !tail_matches {
                continue;
            }
            // The needle's first character: on this row for a non-empty
            // first segment, else the first character after the leading
            // newlines (start of a following row).
            return match segments.iter().position(|s| !s.is_empty()) {
                Some(0) => {
                    let byte_off = first.len() - segments[0].len();
                    Some((row, self.col_of_byte(row, byte_off)?))
                }
                Some(k) => Some((row + u16::try_from(k).ok()?, 0)),
                None => Some((row + extra, 0)),
            };
        }
        None
    }

    /// The text within a rectangle: the given columns of the given rows,
    /// one line per row, trailing whitespace stripped per line (the same
    /// rule as [`Screen::text`]). Ranges take any range expression and are
    /// clamped to the screen:
    ///
    /// ```
    /// # fn main() -> termlens::Result<()> {
    /// # let mut t = termlens::Terminal::builder()
    /// #     .args(["-c", "printf 'left | right'; read q"]).spawn("sh")?;
    /// # t.wait_until(|s| s.contains("right"))?;
    /// let s = t.screen();
    /// let right_pane = s.rect_text(7.., ..);   // columns 7 → end, all rows
    /// assert!(right_pane.contains("right"));
    /// # t.send(termlens::Key::Enter); t.wait_exit()?; Ok(())
    /// # }
    /// ```
    ///
    /// Cells contribute as in [`Screen::row_text`]: blanks render as
    /// spaces, and a wide character contributes where its leading cell
    /// sits — even when the rectangle cuts it in half.
    ///
    /// # Panics
    ///
    /// If either range runs backwards (`3..0`). Note the argument order:
    /// this is the one API in the crate that takes **columns first**, to
    /// match [`TerminalBuilder::size`](crate::TerminalBuilder::size), while
    /// every cell address elsewhere is `(row, col)`. Swapping the two is
    /// therefore the mistake to expect, and a swap can invert a range.
    ///
    /// A panic rather than an error, deliberately, and for the same reason
    /// `&slice[3..0]` panics: a backwards range is not a fact about the
    /// terminal discovered at runtime, it is a mistake in the calling
    /// source. Returned quietly it read as `""` — "this pane is empty",
    /// a perfectly plausible assertion outcome — so a mis-ordered call
    /// passed for the wrong reason and kept passing. Clippy's
    /// `reversed_empty_ranges` already refuses a written-out `3..0`; this
    /// covers the computed bounds it cannot see. Out-of-*range* bounds are
    /// different and stay clamped: asking for more screen than exists is a
    /// reasonable thing to do.
    #[must_use]
    pub fn rect_text(&self, cols: impl RangeBounds<u16>, rows: impl RangeBounds<u16>) -> String {
        let (col_start, col_end) = clamp_range(&cols, self.cols, "column");
        let (row_start, row_end) = clamp_range(&rows, self.rows, "row");
        let mut out = String::new();
        for row in row_start..row_end {
            if row > row_start {
                out.push('\n');
            }
            let mut line = String::new();
            for col in col_start..col_end {
                let Some(cell) = self.cell(row, col) else {
                    break;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                if cell.contents().is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(cell.contents());
                }
            }
            out.push_str(line.trim_end());
        }
        out
    }

    /// The first cell satisfying `predicate` (scanning rows top to bottom,
    /// columns left to right), as `(row, col)`.
    ///
    /// This is the tool for "where did the highlight go":
    ///
    /// ```
    /// # fn main() -> termlens::Result<()> {
    /// # let mut t = termlens::Terminal::builder()
    /// #     .args(["-c", r"printf 'a \033[7mchoice\033[0m'; read q"]).spawn("sh")?;
    /// # t.wait_until(|s| s.contains("choice"))?;
    /// let s = t.screen();
    /// assert_eq!(s.find_by(|c| c.style().reverse), Some((0, 2)));
    /// # t.send(termlens::Key::Enter); t.wait_exit()?; Ok(())
    /// # }
    /// ```
    ///
    /// Every cell is scanned, including blanks and wide-character
    /// continuation cells (they carry their character's style).
    #[must_use]
    pub fn find_by(&self, mut predicate: impl FnMut(&Cell) -> bool) -> Option<(u16, u16)> {
        for row in 0..self.rows {
            for col in 0..self.cols {
                if predicate(self.cell(row, col)?) {
                    return Some((row, col));
                }
            }
        }
        None
    }

    /// Map a byte offset within `row_text(row)` back to the column of the
    /// cell that contributed that byte (wide characters span two columns
    /// but contribute their bytes once).
    fn col_of_byte(&self, row: u16, byte_off: usize) -> Option<u16> {
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
                return Some(col);
            }
            acc += len;
        }
        None
    }

    /// This screen rendered **with its styles**: the normal
    /// [`Display`](fmt::Display) output followed by a `styles:` block
    /// listing every non-default span (format specified in
    /// `docs/DESIGN.md` §3). Style-only regressions — a highlight moving
    /// to another row, a color changing — become visible snapshot diffs:
    ///
    /// ```no_run
    /// # fn main() -> termlens::Result<()> {
    /// # let t = termlens::Terminal::builder().spawn("true")?;
    /// insta::assert_snapshot!(t.screen().with_styles());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Plain snapshots stay text-only; this is the opt-in.
    #[must_use]
    pub fn with_styles(&self) -> ScreenWithStyles<'_> {
        ScreenWithStyles { screen: self }
    }
}

/// Clamp any range expression to `0..len`, as `(start, end)` exclusive.
///
/// # Panics
///
/// If the range runs backwards, naming the axis. See [`Screen::rect_text`]
/// for why this is a panic and not an error.
fn clamp_range(range: &impl RangeBounds<u16>, len: u16, axis: &str) -> (u16, u16) {
    let start = match range.start_bound() {
        Bound::Included(&s) => s,
        Bound::Excluded(&s) => s.saturating_add(1),
        Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
        Bound::Included(&e) => e.saturating_add(1),
        Bound::Excluded(&e) => e,
        Bound::Unbounded => len,
    };
    // Checked on what the caller wrote, before clamping, so the message
    // quotes their numbers. Clamping cannot create an inversion: both
    // bounds are clamped to the same `len`.
    assert!(
        start <= end,
        "rect_text: {axis} range starts at {start} but ends at {end}"
    );
    (start.min(len), end.min(len))
}

impl Style {
    /// True when every attribute is at its default.
    fn is_default(&self) -> bool {
        *self == Style::default()
    }

    /// Fixed-order tokens for the `styles:` block (see `docs/DESIGN.md` §3).
    fn tokens(&self) -> String {
        fn color(prefix: &str, color: Color, out: &mut Vec<String>) {
            match color {
                Color::Default => {}
                Color::Indexed(i) => out.push(format!("{prefix}={i}")),
                Color::Rgb(r, g, b) => out.push(format!("{prefix}=#{r:02x}{g:02x}{b:02x}")),
            }
        }
        let mut tokens = Vec::new();
        color("fg", self.fg, &mut tokens);
        color("bg", self.bg, &mut tokens);
        // SGR order, which is the order the existing tokens were already
        // in — so a cell's tokens are unchanged unless it carries one of
        // the new attributes.
        for (on, name) in [
            (self.bold, "bold"),
            (self.dim, "dim"),
            (self.italic, "italic"),
            (self.underline, "underline"),
            (self.blink, "blink"),
            (self.reverse, "reverse"),
            (self.conceal, "conceal"),
            (self.strikethrough, "strikethrough"),
        ] {
            if on {
                tokens.push(name.to_owned());
            }
        }
        tokens.join(" ")
    }
}

/// [`Screen`] rendered with its styles — see [`Screen::with_styles`].
#[derive(Debug, Clone, Copy)]
pub struct ScreenWithStyles<'a> {
    screen: &'a Screen,
}

impl fmt::Display for ScreenWithStyles<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let screen = self.screen;
        write!(f, "{screen}\n\nstyles:")?;
        let mut any = false;
        for row in 0..screen.rows() {
            let mut spans: Vec<String> = Vec::new();
            let mut run: Option<(u16, u16, Style)> = None;
            for col in 0..screen.cols() {
                let style = screen
                    .cell(row, col)
                    .map_or_else(Style::default, |cell| *cell.style());
                match &mut run {
                    Some((_, end, current)) if *current == style => *end = col,
                    _ => {
                        if let Some(span) = flush(run.take()) {
                            spans.push(span);
                        }
                        run = Some((col, col, style));
                    }
                }
            }
            if let Some(span) = flush(run) {
                spans.push(span);
            }
            if !spans.is_empty() {
                any = true;
                write!(f, "\n{row}: {}", spans.join("; "))?;
            }
        }
        if !any {
            write!(f, "\n(none)")?;
        }
        return Ok(());

        /// Render one run, or `None` for default-styled runs (absence
        /// means default).
        fn flush(run: Option<(u16, u16, Style)>) -> Option<String> {
            let (start, end, style) = run?;
            if style.is_default() {
                return None;
            }
            let range = if start == end {
                format!("{start}")
            } else {
                format!("{start}-{end}")
            };
            Some(format!("{range} {}", style.tokens()))
        }
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
        Screen::from_parts(cols, rows, 1, 2, true, cells, TermState::default())
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
    fn find_locates_multi_row_needles_like_contains() {
        let s = screen(10, 3, &["hello", "world", "again"]);
        assert_eq!(s.find("hello\nworld"), Some((0, 0)));
        assert_eq!(s.find("llo\nwor"), Some((0, 2)));
        assert_eq!(s.find("world\nagain"), Some((1, 0)));
        assert_eq!(s.find("o\nworld\nag"), Some((0, 4)));
        assert_eq!(s.find("hello\nagain"), None); // rows aren't consecutive
        assert_eq!(s.find("hell\nworld"), None); // "hell" doesn't end row 0
        assert_eq!(s.find("hello\nworl\nagain"), None); // middle must be whole
        assert_eq!(s.find("again\nmore"), None); // would run off the screen
                                                 // Trailing whitespace is trimmed per row, exactly like `contains`.
        assert_eq!(s.find("hello \nworld"), None);
        // A needle starting with '\n' reports its first real character.
        assert_eq!(s.find("\nworld"), Some((1, 0)));
        assert_eq!(s.find("\n"), Some((1, 0)));
        // Property: multi-row find agrees with contains.
        for needle in ["hello\nworld", "llo\nwor", "x\nworld", "\nagain"] {
            assert_eq!(s.find(needle).is_some(), s.contains(needle), "{needle:?}");
        }
    }

    #[test]
    fn multi_row_find_reports_wide_aware_columns() {
        let s = screen(10, 2, &["汉字x", "next"]);
        // 汉 = cols 0-1, 字 = cols 2-3, x = col 4.
        assert_eq!(s.find("字x\nnext"), Some((0, 2)));
        assert_eq!(s.find("x\nnext"), Some((0, 4)));
    }

    #[test]
    fn rect_text_slices_columns_and_rows() {
        let s = screen(10, 3, &["0123456789", "abcdefghij", "xyz"]);
        assert_eq!(s.rect_text(2..5, 0..2), "234\ncde");
        assert_eq!(s.rect_text(2..=4, 0..=1), "234\ncde"); // inclusive forms
        assert_eq!(s.rect_text(.., 2..), "xyz"); // trailing blanks trimmed
        assert_eq!(s.rect_text(8.., ..2), "89\nij");
        assert_eq!(s.rect_text(0..3, 5..9), ""); // rows clamp to nothing
        assert_eq!(s.rect_text(20..30, ..1), ""); // cols clamp to nothing
        assert_eq!(s.rect_text(.., ..), s.text()); // the whole screen
    }

    /// Both axes, because they used to disagree: a reversed column range
    /// returned a bare "\n" and a reversed row range returned "", and
    /// neither said anything was wrong.
    ///
    /// The bounds come from variables on purpose. A *literal* `3..0` is
    /// already caught by clippy's `reversed_empty_ranges`, so the case that
    /// reaches a running test is the computed one — which is also the shape
    /// a swapped-argument mistake actually takes.
    #[test]
    #[should_panic(expected = "column range starts at 3 but ends at 0")]
    fn a_reversed_column_range_panics() {
        let s = screen(10, 3, &["0123456789", "abcdefghij", "xyz"]);
        let (from, to) = (3, 0);
        let _ = s.rect_text(from..to, 0..2);
    }

    #[test]
    #[should_panic(expected = "row range starts at 2 but ends at 0")]
    fn a_reversed_row_range_panics() {
        let s = screen(10, 3, &["0123456789", "abcdefghij", "xyz"]);
        let (from, to) = (2, 0);
        let _ = s.rect_text(0..3, from..to);
    }

    /// Out-of-range is not the same mistake and stays clamped: asking for
    /// more screen than exists is reasonable, asking backwards is not.
    #[test]
    fn out_of_range_bounds_still_clamp() {
        let s = screen(10, 3, &["0123456789", "abcdefghij", "xyz"]);
        assert_eq!(s.rect_text(8..99, ..1), "89");
        assert_eq!(s.rect_text(.., 1..99), "abcdefghij\nxyz");
        assert_eq!(s.rect_text(5..5, ..), "\n\n"); // empty but not inverted
    }

    #[test]
    fn rect_text_wide_characters_count_where_they_start() {
        let s = screen(10, 1, &["汉字x"]);
        assert_eq!(s.rect_text(0..2, ..), "汉");
        // Slicing in from the continuation side drops the cut character
        // (its leading cell is outside) and keeps the next one whole.
        assert_eq!(s.rect_text(1..3, ..), "字");
        assert_eq!(s.rect_text(4.., ..), "x");
    }

    #[test]
    fn find_by_scans_row_major_and_sees_styles() {
        let s = screen(10, 2, &["ab*", "c"]);
        assert_eq!(s.find_by(|c| c.style().bold), Some((0, 2)));
        assert_eq!(s.find_by(|c| c.contents() == "c"), Some((1, 0)));
        assert_eq!(s.find_by(|c| c.style().reverse), None);
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
    fn with_styles_renders_runs_in_fixed_token_order() {
        use unicode_width::UnicodeWidthChar as _;
        let mut cells: Vec<Cell> = Vec::new();
        let styled = Style {
            fg: Color::Indexed(4),
            bold: true,
            ..Style::default()
        };
        // Row 0: "hi" styled, rest default. Row 1: all default text.
        // Row 2: one reverse blank cell at col 3 (highlight past text).
        for ch in ['h', 'i'] {
            assert_eq!(ch.width(), Some(1));
            cells.push(Cell::new(ch.to_string(), styled, false, false));
        }
        for _ in 2..6 {
            cells.push(Cell::new(String::new(), Style::default(), false, false));
        }
        for ch in "plain ".chars() {
            cells.push(Cell::new(ch.to_string(), Style::default(), false, false));
        }
        for col in 0..6 {
            let style = if col == 3 {
                Style {
                    reverse: true,
                    ..Style::default()
                }
            } else {
                Style::default()
            };
            cells.push(Cell::new(String::new(), style, false, false));
        }
        let screen = Screen::from_parts(6, 3, 0, 0, true, cells, TermState::default());

        let rendered = screen.with_styles().to_string();
        let styles_block = rendered.split("\n\nstyles:\n").nth(1).unwrap();
        assert_eq!(styles_block, "0: 0-1 fg=4 bold\n2: 3 reverse");
        // The plain rendering is a strict prefix.
        assert!(rendered.starts_with(&screen.to_string()));
    }

    #[test]
    fn with_styles_on_a_default_screen_says_none() {
        let s = screen(10, 2, &["hello"]);
        let rendered = s.with_styles().to_string();
        assert!(rendered.ends_with("\n\nstyles:\n(none)"), "{rendered}");
    }

    #[test]
    fn with_styles_renders_rgb_and_merges_adjacent_runs() {
        let style = Style {
            bg: Color::Rgb(0x1e, 0x1e, 0x2e),
            ..Style::default()
        };
        let mut cells: Vec<Cell> = Vec::new();
        for ch in ['a', 'b', 'c'] {
            cells.push(Cell::new(ch.to_string(), style, false, false));
        }
        cells.push(Cell::new(String::new(), Style::default(), false, false));
        let screen = Screen::from_parts(4, 1, 0, 0, true, cells, TermState::default());
        let rendered = screen.with_styles().to_string();
        assert!(
            rendered.ends_with("styles:\n0: 0-2 bg=#1e1e2e"),
            "{rendered}"
        );
    }

    #[test]
    fn display_format_matches_spec() {
        let s = screen(10, 2, &["hi"]);
        assert_eq!(format!("{s}"), "size: 10x2  cursor: 1,2\nhi\n");
    }

    #[test]
    fn state_accessors_report_the_captured_state_and_stay_out_of_display() {
        let default = screen(4, 1, &["x"]);
        assert_eq!(default.title(), "");
        assert!(!default.alternate_screen());
        assert!(!default.bracketed_paste());
        assert!(!default.application_cursor());
        assert_eq!(default.mouse_mode(), MouseMode::None);
        assert!(default.clipboard().is_none());

        let state = TermState {
            title: Arc::from("my app"),
            alternate_screen: true,
            bracketed_paste: true,
            application_cursor: true,
            mouse: MouseMode::AnyMotion,
            clipboard: Some(Arc::new(Clipboard::new("c", Some("copied".into())))),
            scrollback: Arc::from([Arc::from("scrolled away")]),
        };
        let cells = vec![Cell::new("x".into(), Style::default(), false, false)];
        let s = Screen::from_parts(1, 1, 0, 0, true, cells, state);
        assert_eq!(s.title(), "my app");
        assert!(s.alternate_screen() && s.bracketed_paste() && s.application_cursor());
        assert_eq!(s.mouse_mode(), MouseMode::AnyMotion);
        let clip = s.clipboard().expect("captured");
        assert_eq!((clip.targets(), clip.text()), ("c", Some("copied")));
        assert_eq!(s.scrollback_rows(), 1);
        assert_eq!(s.scrollback_text(), "scrolled away");
        assert_eq!(s.full_text(), "scrolled away\nx");
        // Out-of-band state never leaks into the text format — including
        // history, so existing snapshot files stay valid now that
        // retention is on by default.
        assert_eq!(format!("{s}"), "size: 1x1  cursor: 0,0\nx");
    }
}
