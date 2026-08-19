//! The rendered screen grid: [`Screen`], [`Cell`], [`Style`], [`Color`].
//!
//! A [`Screen`] is an immutable snapshot of the emulated terminal at one
//! moment. It is a cheap-to-clone value type (the grid is behind an [`Arc`]),
//! so errors and assertions can carry whole screens around freely.

use std::fmt;
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;

use unicode_normalization::UnicodeNormalization;

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

/// Inline graphics payloads the application transmitted, as counted at one
/// snapshot.
///
/// Read it from a snapshot via [`Screen::graphics`]. Counting is not
/// rendering and not a claim of support: DA1 still declines both protocols,
/// which is exactly why an application that emits them anyway is worth
/// catching.
///
/// The assertion this exists for is as often the negative one — "this must
/// render as text and **never** be transmitted as an image, so it looks the
/// same in every terminal" — which is why [`is_empty`](Self::is_empty) is a
/// method rather than something to spell out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraphicsSeen {
    kitty: u32,
    sixel: u32,
    bytes: u64,
}

impl GraphicsSeen {
    /// Kitty graphics payloads (`APC G … ST`) seen so far.
    #[must_use]
    pub fn kitty(&self) -> u32 {
        self.kitty
    }

    /// Sixel payloads (`DCS q … ST`) seen so far.
    #[must_use]
    pub fn sixel(&self) -> u32 {
        self.sixel
    }

    /// Payloads of either protocol.
    #[must_use]
    pub fn total(&self) -> u32 {
        self.kitty + self.sixel
    }

    /// True when the application has transmitted no inline graphics at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// Total payload bytes across both protocols: everything between the
    /// introducer and the terminator, counted the same way for each.
    ///
    /// A size rather than the data itself: a test asserts that an image was
    /// or was not sent, and how big it was — not what it depicted, which
    /// nothing here can decode.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) fn record(&mut self, kitty: bool, bytes: u64) {
        if kitty {
            self.kitty += 1;
        } else {
            self.sixel += 1;
        }
        self.bytes += bytes;
    }
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
///
/// Held behind a single [`Arc`] on [`Screen`], for two reasons that pull the
/// same way. `Screen` is embedded in every [`Error`](crate::Error), so its
/// size is load-bearing — enough scalars here and `Result<T>` grows past
/// what clippy's `result_large_err` will accept. And a `Screen` clone then
/// bumps one refcount instead of copying every field, which matters because
/// a clone happens on each wait evaluation.
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
    /// Bells rung in ground state (a `BEL` terminating an OSC string is a
    /// terminator, not a bell).
    pub(crate) bells: u64,
    /// Whether the application enabled focus reporting (mode 1004).
    pub(crate) focus_events: bool,
    /// Inline graphics payloads transmitted.
    pub(crate) graphics: GraphicsSeen,
    /// Completed synchronized updates. Filled in by the terminal rather
    /// than the emulator, which does not own the frame count.
    pub(crate) repaints: u64,
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
            bells: 0,
            focus_events: false,
            graphics: GraphicsSeen::default(),
            repaints: 0,
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
    state: Arc<TermState>,
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
            state: Arc::new(state),
        }
    }

    /// Stamp the repaint count onto a freshly built snapshot.
    ///
    /// The count lives on the terminal, not the emulator: it is the same
    /// counter `wait_frame`'s cursor is built on, and a second one in the
    /// emulator could drift from it.
    pub(crate) fn with_repaints(mut self, repaints: u64) -> Self {
        // Called on a snapshot nobody else holds yet, so `make_mut` mutates
        // in place rather than cloning.
        Arc::make_mut(&mut self.state).repaints = repaints;
        self
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

    /// True while the application has focus reporting (mode 1004) enabled.
    /// [`Terminal::focus_in`](crate::Terminal::focus_in) and
    /// [`Terminal::focus_out`](crate::Terminal::focus_out) consult the same
    /// state, so a test can assert the application asked for focus events
    /// before trying to deliver one.
    #[must_use]
    pub fn focus_events(&self) -> bool {
        self.state.focus_events
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

    /// How many times the application has **completed a repaint** — a DEC
    /// 2026 synchronized update begun and ended — as of this observation.
    ///
    /// Monotonic, so the natural use is a delta around an action:
    ///
    /// ```no_run
    /// # fn main() -> termlens::Result<()> {
    /// # let mut t = termlens::Terminal::builder().spawn("true")?;
    /// let before = t.screen().repaints();
    /// t.scroll(0, 0, termlens::Scroll::Down)?;
    /// let frame = t.wait_frame(|s| s.contains("row 2"))?;
    /// // One wheel notch must not become five repaints.
    /// assert_eq!(frame.repaints() - before, 1);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// **It counts repaints, not changes.** A Begin/End pair that altered no
    /// cell still counts, exactly as [`wait_frame`](crate::Terminal::wait_frame)
    /// treats it — and that is the property an amplification test depends
    /// on. "One input produced four repaints" is invisible to any content
    /// predicate, because every intermediate frame shows correct content.
    /// Only the count sees it.
    ///
    /// Zero for an application that never emits synchronized updates; there
    /// is nothing to count, not even its redraws.
    #[must_use]
    pub fn repaints(&self) -> u64 {
        self.state.repaints
    }

    /// How many times the application has rung the bell (`BEL`, `0x07`) as
    /// of this observation.
    ///
    /// A count rather than a flag, so "rang twice" is distinguishable from
    /// "rang once", and monotonic like [`repaints`](Self::repaints) so a test
    /// can take a delta around one action.
    ///
    /// The bell is often the *only* feedback a rejected input produces:
    /// "pressing an invalid key does nothing" and "pressing an invalid key is
    /// refused with a bell" are different behaviours, and without this they
    /// are the same screen.
    ///
    /// Only a `BEL` in ground state counts. The one that terminates an
    /// `OSC` string is punctuation, not a bell, and a `BEL` inside a
    /// DCS-class string is payload.
    #[must_use]
    pub fn bells(&self) -> u64 {
        self.state.bells
    }

    /// Inline graphics payloads the application has transmitted — kitty
    /// (`APC G … ST`) and sixel (`DCS q … ST`) — as of this observation.
    ///
    /// ```no_run
    /// # fn main() -> termlens::Result<()> {
    /// # let mut t = termlens::Terminal::builder().spawn("true")?;
    /// t.wait_until(|s| s.contains("diagram"))?;
    /// // A diagram must render as box art in every terminal, so it must
    /// // never go out as an image.
    /// assert!(t.screen().graphics().is_empty());
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Observing is not rendering, and not a claim of support: DA1 goes on
    /// declining both protocols, which is precisely why an application that
    /// transmits one anyway is worth catching.
    #[must_use]
    pub fn graphics(&self) -> GraphicsSeen {
        self.state.graphics
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
    ///
    /// # Normalization
    ///
    /// Both sides are folded to **NFC** before comparing, so a needle finds
    /// text the application normalized the other way. A terminal draws
    /// `caf\u{e9}` and `cafe\u{301}` identically, and so does the failure
    /// output and the diff — which is what made the mismatch a trap rather
    /// than a limitation. A test author types NFC (that is what editors
    /// produce); text from a filesystem path, a git author name, or macOS
    /// input is frequently NFD.
    ///
    /// Folding is unconditional here and needs no escape hatch, because the
    /// raw form is never taken away: [`text`](Self::text),
    /// [`row_text`](Self::row_text), [`rect_text`](Self::rect_text),
    /// [`cell`](Self::cell) and [`title`](Self::title) all return exactly
    /// the codepoints the application sent, so a test that means to assert
    /// on normalization compares those directly. A snapshot is an
    /// observation; only the search over it is forgiving.
    ///
    /// Note that this makes matching grapheme-shaped rather than
    /// byte-shaped: on a screen showing `caf\u{e9}`, `contains("cafe")` is
    /// **false**, because the screen does not show `cafe`.
    ///
    /// Text pulled out as a `String` and matched with `str` methods —
    /// `full_text().contains(..)` — is byte-exact, since the comparison is
    /// then `std`'s and not ours.
    #[must_use]
    pub fn contains(&self, needle: &str) -> bool {
        if self.is_ascii() && needle.is_ascii() {
            return self.text().contains(needle);
        }
        self.nfc_text().contains(&nfc(needle))
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
    ///
    /// Matching folds both sides to NFC, exactly as
    /// [`contains`](Self::contains) does and for the same reasons — a needle
    /// is found here precisely when `contains` is true. The reported column
    /// is the real one: folding happens per cell, so the byte-to-column map
    /// stays exact even where a composition shortened the text.
    #[must_use]
    pub fn find(&self, needle: &str) -> Option<(u16, u16)> {
        if needle.is_empty() {
            return Some((0, 0));
        }
        let needle = &self.fold(needle);
        if !needle.contains('\n') {
            for row in 0..self.rows {
                let (text, cols) = self.searchable_row(row);
                if let Some(byte_off) = text.find(needle.as_str()) {
                    return Some((row, cols.get(byte_off).copied()?));
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
            let (first_line, cols) = self.searchable_row(row);
            let first = first_line.trim_end();
            if !first.ends_with(segments[0]) {
                continue;
            }
            let tail_matches = segments[1..].iter().enumerate().all(|(i, seg)| {
                let (line, _) = self.searchable_row(row + 1 + i as u16);
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
                    Some((row, cols.get(byte_off).copied()?))
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

    /// True when every cell holds only ASCII, so normalization is the
    /// identity and the search helpers can skip it. This is the shape of
    /// virtually every screen, which is what keeps `contains` — evaluated
    /// on every wait wake-up — as cheap as it was.
    fn is_ascii(&self) -> bool {
        self.cells.iter().all(|c| c.contents().is_ascii())
    }

    /// `needle` in the form the search helpers compare against.
    fn fold(&self, needle: &str) -> String {
        if self.is_ascii() && needle.is_ascii() {
            needle.to_owned()
        } else {
            nfc(needle)
        }
    }

    /// The whole grid in searchable form: rows joined with `\n`, trailing
    /// whitespace stripped per row, each row folded the same way
    /// [`searchable_row`](Self::searchable_row) folds it — so `contains` and
    /// `find` can never disagree about what matches.
    fn nfc_text(&self) -> String {
        let mut out = String::new();
        for row in 0..self.rows {
            if row > 0 {
                out.push('\n');
            }
            let (line, _) = self.searchable_row(row);
            out.push_str(line.trim_end());
        }
        out
    }

    /// One row in searchable form, plus the column that produced each byte.
    ///
    /// Folding is **per cell**, not over the joined string, and that is the
    /// load-bearing detail: a cell holds a base character together with its
    /// combining marks (vt100 appends them to the cell being written), so
    /// folding cell by cell composes exactly what the terminal draws in one
    /// cell and can never compose across a cell boundary. It also keeps the
    /// byte-to-column map exact, which is what [`find`](Self::find) reports.
    fn searchable_row(&self, row: u16) -> (String, Vec<u16>) {
        let ascii = self.is_ascii();
        let mut text = String::with_capacity(usize::from(self.cols));
        let mut cols = Vec::with_capacity(usize::from(self.cols));
        for col in 0..self.cols {
            let Some(cell) = self.cell(row, col) else {
                break;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let before = text.len();
            if cell.contents().is_empty() {
                text.push(' ');
            } else if ascii {
                text.push_str(cell.contents());
            } else {
                text.extend(cell.contents().nfc());
            }
            cols.resize(text.len(), col);
            debug_assert!(text.len() > before || cell.is_wide_continuation());
        }
        // One extra entry, so a match at the very end of the row maps to
        // the last column instead of falling off the map.
        cols.push(self.cols.saturating_sub(1));
        (text, cols)
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

/// `s` folded to NFC.
fn nfc(s: &str) -> String {
    s.nfc().collect()
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
                    // A zero-width combining mark joins the cell it modifies,
                    // which is what vt100 does: it appends combining
                    // characters to the cell currently being written rather
                    // than advancing. Modelling that here matters, because
                    // needle folding is per cell.
                    if ch.width().unwrap_or(1) == 0 {
                        if let Some(last) = row_cells.last_mut() {
                            let joined = format!("{}{ch}", last.contents());
                            *last = Cell::new(joined, *last.style(), last.is_wide(), false);
                            continue;
                        }
                    }
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

    /// The trap: NFC and NFD render identically, so a needle that misses
    /// looks like content that is absent. Both directions, because the
    /// author's needle and the application's output can each be either.
    #[test]
    fn needles_match_across_normalization_forms() {
        let nfc = "caf\u{e9}";
        let nfd = "cafe\u{301}";

        let on_nfd = screen(10, 1, &[nfd]);
        assert!(on_nfd.contains(nfc), "NFC needle must find NFD text");
        assert!(on_nfd.contains(nfd));
        assert_eq!(on_nfd.find(nfc), Some((0, 0)));
        assert_eq!(on_nfd.find(nfd), Some((0, 0)));

        let on_nfc = screen(10, 1, &[nfc]);
        assert!(on_nfc.contains(nfd), "NFD needle must find NFC text");
        assert!(on_nfc.contains(nfc));
        assert_eq!(on_nfc.find(nfd), Some((0, 0)));

        // The grid still holds what the application sent: an observation is
        // not rewritten, and a test that means to assert on the form can.
        assert_eq!(on_nfd.text(), nfd);
        assert_eq!(on_nfc.text(), nfc);
        assert_ne!(on_nfd.text(), on_nfc.text());
    }

    /// Matching is grapheme-shaped once folding applies: the screen shows
    /// `caf\u{e9}`, so it does not show `cafe`.
    #[test]
    fn a_folded_match_does_not_split_a_composed_character() {
        let on_nfd = screen(10, 1, &["cafe\u{301}"]);
        assert!(!on_nfd.contains("cafe"), "the screen shows caf\u{e9}");
        assert!(on_nfd.contains("caf"));
    }

    /// Columns must survive folding: NFD text is longer in bytes than its
    /// NFC form, so a naive offset would land in the wrong cell.
    #[test]
    fn folded_matches_still_report_real_columns() {
        // "e" + combining acute in cell 0, then a marker further along.
        let s = screen(10, 1, &["e\u{301}xyMARK"]);
        assert_eq!(s.find("MARK"), Some((0, 3)));
        assert_eq!(s.find("x"), Some((0, 1)));
        // Wide characters and folding together.
        let wide = screen(10, 1, &["\u{6c49}e\u{301}Z"]);
        assert_eq!(wide.find("Z"), Some((0, 3)));
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
            bells: 3,
            focus_events: true,
            graphics: {
                let mut g = GraphicsSeen::default();
                g.record(true, 120);
                g.record(false, 40);
                g
            },
            repaints: 9,
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
        assert_eq!(s.bells(), 3);
        assert!(s.focus_events());
        assert_eq!(s.repaints(), 9);
        assert_eq!(s.graphics().kitty(), 1);
        assert_eq!(s.graphics().sixel(), 1);
        assert_eq!(s.graphics().total(), 2);
        assert_eq!(s.graphics().bytes(), 160);
        assert!(!s.graphics().is_empty());
        assert!(GraphicsSeen::default().is_empty());
        assert_eq!(s.scrollback_text(), "scrolled away");
        assert_eq!(s.full_text(), "scrolled away\nx");
        // Out-of-band state never leaks into the text format — including
        // history, so existing snapshot files stay valid now that
        // retention is on by default.
        assert_eq!(format!("{s}"), "size: 1x1  cursor: 0,0\nx");
    }
}
