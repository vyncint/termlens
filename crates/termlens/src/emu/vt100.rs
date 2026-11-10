//! The `vt100`-crate backend. Public types never leak from here: every
//! snapshot converts vt100's grid into termlens's own [`Screen`].

use std::borrow::Cow;
use std::collections::VecDeque;
use std::num::NonZeroU16;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::seq::{SeqEvent, SeqTracker, TabOp};
use super::shadow::{AttrShadow, ColorNormalizer};
use super::unhandled::Unhandled;
use super::{Emulator, FrameSpan, InputModes, ModeState, MouseEncoding, Processed, Stop};
use crate::graphics::{
    kitty_cells, GraphicsAction, GraphicsPayload, GraphicsProtocol, GraphicsSeen, Placement,
    HISTORY,
};
use crate::screen::{Cell, Color, MouseMode, Screen, Style, TermState};
use crate::Graphics;

pub(crate) struct Vt100Emulator {
    /// Built with the callback set that records what vt100 could not
    /// render, so a snapshot can say so (`emu/unhandled.rs`).
    parser: ::vt100::Parser<Unhandled>,
    tracker: SeqTracker,
    /// Carries blink, conceal and strikethrough, which vt100 drops. See
    /// `emu/shadow.rs` for why this is a second parser rather than
    /// hand-rolled attribute tracking.
    shadow: AttrShadow,
    colors: ColorNormalizer,
    /// How many rows of history to retain (0 disables it entirely).
    scrollback_len: usize,
    /// When the current synchronized update began, stamped at the byte that
    /// opened it. `None` outside a frame.
    frame_started: Option<Instant>,
    /// Rows that have scrolled off the top, oldest first, as text.
    ///
    /// Materialized here — once per read that scrolled — rather than in
    /// `snapshot`, which runs far more often (every wait evaluation on a
    /// chatty stream). A snapshot then costs one `Arc` clone per row
    /// instead of rebuilding the whole history.
    history: VecDeque<Arc<str>>,
    /// The same rows as cells, when the terminal was built to retain
    /// styles in history (#146); `None` otherwise, so a suite that never
    /// asks pays nothing. Kept in lockstep with `history`, one entry each.
    history_cells: Option<VecDeque<Arc<[Cell]>>>,
    /// Rows of vt100's own scrollback already copied into `history`, so a
    /// read that scrolled nothing costs one length check.
    captured: usize,
    /// Set just before the byte that empties vt100's history — RIS, or ED 3
    /// on the main screen — so the capture that byte's feed runs starts the
    /// mark over instead of reading the shorter history as rows that came
    /// back onto the screen. See `capture_scrolled_rows`.
    history_reset: bool,
    /// Inline graphics payloads, oldest first, behind an `Arc` so a
    /// snapshot costs one refcount rather than a copy of every image.
    graphics: Arc<Vec<GraphicsPayload>>,
    /// For each payload in `graphics`, the image ids a kitty delete freed
    /// ([`Placement::forgets`]), so a later `a=p` of one is not sized from
    /// an image kitty has forgotten (#672). Kept in lockstep with
    /// `graphics`, one entry each, and so bounded by the same two bounds.
    graphics_forgets: VecDeque<Option<(u32, u32)>>,
    /// Payload bytes currently retained, so eviction is a subtraction
    /// rather than a walk.
    graphics_bytes: usize,
    /// The retention budget, from `TerminalBuilder::capture_graphics`.
    capture: usize,
    /// The inline-graphics support the test declared, and its cell size:
    /// what decides whether, and how far, an image moves the cursor (#588).
    image_support: Graphics,
    cell_size: Option<(u16, u16)>,
    /// Bytes rewritten on their way to the parsers and not yet handed over.
    /// Empty except while a DEC Special Graphics set is invoked: a byte the
    /// set redefines reaches the grid as the glyph it draws, so the stream
    /// the parsers see is then no longer a sub-slice of the read. Kept
    /// between calls only for its allocation.
    staged: Vec<u8>,
}

/// How far past an image the declared terminal moves the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageStep {
    /// kitty: the placement's size in cells.
    Kitty { cols: u32, rows: u32 },
    /// xterm's sixel: the text rows the image covers.
    Sixel { rows: u32 },
}

impl Vt100Emulator {
    /// Close out the frame that just ended: how long it ran between the
    /// application's own markers, and how much it drew.
    fn close_frame(&mut self) -> FrameSpan {
        let started = self.frame_started.take();
        debug_assert!(
            started.is_some(),
            "a frame only ends where a Begin was seen, so the start must exist"
        );
        FrameSpan {
            duration: started.map_or(Duration::ZERO, |at| at.elapsed()),
            printable: self.tracker.take_frame_printable(),
        }
    }

    pub(crate) fn new(
        rows: u16,
        cols: u16,
        scrollback_len: usize,
        capture: usize,
        styled_history: bool,
    ) -> Self {
        Self {
            parser: ::vt100::Parser::new_with_callbacks(
                nz(rows),
                nz(cols),
                scrollback_len,
                Unhandled::default(),
            ),
            tracker: SeqTracker::new(capture, rows, cols),
            // The shadow keeps the same history as the primary, styled or
            // not: a resize that makes the screen taller pulls rows back out
            // of history onto the grid, and the shadow's grid has to get the
            // same rows back to stay in lockstep.
            shadow: AttrShadow::new(rows, cols, scrollback_len),
            history_cells: styled_history.then(VecDeque::new),
            colors: ColorNormalizer::new(),
            scrollback_len,
            frame_started: None,
            history: VecDeque::new(),
            captured: 0,
            history_reset: false,
            graphics: Arc::new(Vec::new()),
            graphics_forgets: VecDeque::new(),
            graphics_bytes: 0,
            capture,
            image_support: Graphics::None,
            cell_size: None,
            staged: Vec::new(),
        }
    }

    /// Simulate a terminal with `support` for inline graphics and cells of
    /// `cell_size` pixels: an image it places then moves the cursor as that
    /// terminal moves it. A terminal without the protocol ignores the
    /// escape and moves nothing — the default (#588).
    pub(crate) fn declare_graphics(&mut self, support: Graphics, cell_size: Option<(u16, u16)>) {
        self.image_support = support;
        self.cell_size = cell_size;
    }

    /// How far the declared terminal moves the cursor past `payload`, or
    /// `None` where it does not move it or the escape does not say.
    ///
    /// kitty moves it right by the placement's columns and down by its rows
    /// less one — onto the image's last row, one column past it — for a
    /// placement that asks (no `C=1`, `U=1` or `P=`). xterm, with sixel
    /// scrolling on as it is by default, moves it down to the image's last
    /// text row and leaves the column. Each ignores the other's escape.
    fn image_step(&self, payload: &GraphicsPayload, placement: &Placement) -> Option<ImageStep> {
        match (self.image_support, payload.protocol()) {
            (Graphics::Kitty, GraphicsProtocol::Kitty) => {
                if !placement.moves {
                    return None;
                }
                let image = match payload.action() {
                    GraphicsAction::TransmitAndPlace => payload.size(),
                    // A placement of an image sent earlier: its size is that
                    // of the last transmission of that id still in the log.
                    // The last one, even when it declared no size: kitty
                    // replaces an image on retransmission (`handle_add_command`
                    // frees the existing image's resources), so the size of
                    // one sent before it describes nothing on screen (#626).
                    //
                    // Unless a delete that frees the image came after it.
                    // kitty has then forgotten the image, and refuses the
                    // placement ("Put command refers to non-existent image",
                    // `handle_put_command`): nothing is placed, so nothing
                    // moves — even with `c=` and `r=` given (#672). It was
                    // sized from the image kitty no longer has.
                    GraphicsAction::Place => {
                        let id = payload.id()?;
                        let newest = self.graphics.iter().zip(&self.graphics_forgets).rev().find(
                            |(earlier, forgets)| {
                                forgets.is_some_and(|(first, last)| (first..=last).contains(&id))
                                    || (earlier.protocol() == GraphicsProtocol::Kitty
                                        && earlier.action().carries_image()
                                        && earlier.id() == Some(id))
                            },
                        );
                        match newest {
                            Some((_, Some(_))) => return None,
                            Some((earlier, None)) => earlier.size(),
                            None => None,
                        }
                    }
                    _ => return None,
                };
                let (cols, rows) = kitty_cells(placement, image, self.cell_size)?;
                Some(ImageStep::Kitty { cols, rows })
            }
            (Graphics::Sixel, GraphicsProtocol::Sixel) => {
                let (_, cell_height) = self.cell_size.filter(|&(w, h)| w > 0 && h > 0)?;
                let rows = placement.height.div_ceil(u32::from(cell_height));
                (rows > 0).then_some(ImageStep::Sixel { rows })
            }
            _ => None,
        }
    }

    /// Move the cursor past an image (#588): down to its last row — a line
    /// feed at a time, so the bottom margin scrolls and the rows it pushes
    /// out reach history as any other scroll does — then, for kitty, one
    /// column past its right edge, or to the start of the next row when that
    /// is past the screen's. No count scrolls further than the screen is
    /// tall: past that every row it pushed out was blank.
    fn move_past_image(&mut self, step: ImageStep) {
        let (screen_rows, screen_cols) = size(self.parser.screen());
        let (_, col) = self.cursor_position();
        let (rows, across) = match step {
            ImageStep::Kitty { cols, rows } => (rows, Some(u32::from(col).saturating_add(cols))),
            ImageStep::Sixel { rows } => (rows, None),
        };
        let wrap = across.is_some_and(|x| x >= u32::from(screen_cols));
        let down = (rows.saturating_sub(1) + u32::from(wrap)).min(u32::from(screen_rows));
        let mut bytes = vec![b'\n'; down as usize];
        match across {
            Some(_) if wrap => bytes.push(b'\r'),
            Some(x) if x > u32::from(col) => {
                bytes.extend_from_slice(format!("\x1b[{}C", x - u32::from(col)).as_bytes());
            }
            _ => {}
        }
        self.feed(&bytes);
    }

    /// Hand the grid the bytes the tracker has already scanned.
    ///
    /// The tracker runs a byte ahead of the parser so it can stop the read
    /// at a frame end or a query; everything else is fed in bulk. Splitting
    /// the feed is what lets a graphics payload be stamped with the cursor
    /// position as of its terminator rather than as of the whole read.
    fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        // U+FFFD would be dropped on the way in — see `stand_in_for_replacement`.
        let bytes = stand_in_for_replacement(bytes);
        let normalized = self.colors.feed(&bytes);
        self.parser.process(&normalized);
        self.shadow.feed(&bytes);
        self.capture_scrolled_rows();
    }

    /// Hand the grid everything staged so far followed by `tail`, in order.
    ///
    /// The common case — nothing was rewritten — feeds `tail` straight
    /// through, so a stream that never designates a character set pays for
    /// none of this.
    fn feed_staged(&mut self, tail: &[u8]) {
        if self.staged.is_empty() {
            self.feed(tail);
            return;
        }
        let mut staged = std::mem::take(&mut self.staged);
        staged.extend_from_slice(tail);
        self.feed(&staged);
        staged.clear();
        self.staged = staged;
    }

    /// Carry out a tab-stop operation, given the bytes still owed to the
    /// grid: `before` is everything ahead of the one that completed the
    /// sequence, and `last` is that byte itself.
    ///
    /// Every one of the five is relative to the cursor column, so `before`
    /// has to reach the parser first — the position read otherwise belongs
    /// to wherever the last feed stopped, which on a chatty stream is an
    /// arbitrary number of characters back.
    ///
    /// The split is at the completing byte and not past it, which matters
    /// for plain `HT` alone: vt100 ignores the four escapes, but it acts on
    /// `HT`, so feeding that byte before the column is read would move the
    /// cursor to vt100's fixed eight and leave the computation working from
    /// the wrong base. Reading first is correct for all five, since no
    /// escape prefix moves the cursor either.
    ///
    /// The sequence is then fed rather than dropped. vt100 ignores `HTS`,
    /// `TBC`, `CHT` and `CBT` outright, and dropping a final byte would
    /// leave its parser sitting in a half-consumed escape that swallows
    /// whatever came next. `HT` it does act on, moving by its own fixed
    /// eight — which the `CHA` below then overrides, since `HT` draws
    /// nothing and only moves the cursor.
    fn apply_tabs(&mut self, op: TabOp, before: &[u8], last: &[u8]) {
        self.feed_staged(before);
        // The *raw* column, not the clamped one a snapshot reports: `HTS`
        // at the right margin is dropped by `TabStops::set`, on purpose —
        // clamping it would set a stop the application never asked for,
        // and that stop outlives the wrap once the grid is widened.
        let col = self.parser.screen().cursor_position().1;
        let target = self.tracker.tab_op(op, col);
        self.feed_staged(last);
        if let Some(target) = target {
            // CHA counts from one. Both parsers are fed it, so the
            // attribute shadow moves with the primary grid and the two
            // stay the same shape — the invariant `snapshot` asserts.
            self.feed(format!("\x1b[{}G", target.saturating_add(1)).as_bytes());
        }
    }

    /// File a completed payload, stamped with where it landed.
    ///
    /// The position once the terminator has been consumed *is* the image's
    /// top-left corner: stamped before [`move_past_image`] moves the cursor
    /// on, as the declared terminal does. It is the one fact about an image
    /// that lives in the grid rather than in the payload, and the one an
    /// application gets wrong when a picture drifts out from under its own
    /// labels.
    ///
    /// [`move_past_image`]: Self::move_past_image
    fn record_graphics(&mut self, mut payload: GraphicsPayload, forgets: Option<(u32, u32)>) {
        payload.place(self.cursor_position());
        let kept = payload.data().map_or(0, <[u8]>::len);
        let log = Arc::make_mut(&mut self.graphics);
        log.push(payload);
        self.graphics_forgets.push_back(forgets);
        self.graphics_bytes += kept;
        // Evict oldest-first, on both bounds. A snapshot already taken keeps
        // its own view: it holds an `Arc` to the vector as it stood.
        while log.len() > HISTORY || (self.graphics_bytes > self.capture && log.len() > 1) {
            let dropped = log.remove(0);
            self.graphics_forgets.pop_front();
            self.graphics_bytes -= dropped.data().map_or(0, <[u8]>::len);
        }
    }

    /// Carry out a completed payload: stamp it, file it, and move the cursor
    /// past it as the declared terminal does.
    fn place_graphics(&mut self, payload: GraphicsPayload, placement: &Placement) {
        // Sized before the payload joins the log, so a placement finds the
        // earlier transmission and not itself; moved after it is stamped, so
        // `at` is the top-left corner.
        let step = self.image_step(&payload, placement);
        self.record_graphics(payload, placement.forgets);
        if let Some(step) = step {
            self.move_past_image(step);
        }
    }

    /// Feed `before`, then `last` — the byte that completes RIS (`hard`) or
    /// ED 3 — on its own.
    ///
    /// Rows that scrolled since the last capture exist only in vt100's
    /// history, and the sequence is about to discard it. The feed is split
    /// at its final byte: everything before goes first — one write can carry
    /// lines and the reset together — and the capture that feed runs takes
    /// those rows. The final byte follows alone, and the capture its feed
    /// runs is told the history starts over (#391). ED 3 on the alternate
    /// screen empties only that grid's history, which is always empty, so
    /// the main screen's mark stands.
    fn feed_history_reset(&mut self, before: &[u8], last: &[u8], hard: bool) {
        self.feed_staged(before);
        self.history_reset = hard || !self.parser.screen().alternate_screen();
        self.feed_staged(last);
    }

    /// Copy any rows that have scrolled off since the last call.
    ///
    /// vt100 models scrollback as a *stateful view*: `set_scrollback(n)`
    /// moves the offset so the same accessors read history rows. A
    /// `Screen` is an immutable snapshot and the whole crate's honesty
    /// rests on that, so the view is moved here — under `&mut self`, while
    /// bytes are being consumed — and always restored to 0 before anyone
    /// can observe the grid. No snapshot ever depends on parser state read
    /// later.
    fn capture_scrolled_rows(&mut self) {
        if self.scrollback_len == 0 {
            return;
        }
        // The alternate screen's grid keeps no history, and every accessor
        // reads the grid on display: its length of zero is not the main
        // screen's history emptied. Reading it as that restarted the mark,
        // and leaving the alternate screen then copied the whole history a
        // second time (#576). The main screen's history waits, untouched,
        // until the main screen is back — and so does the count of the rows
        // a resize dropped from it meanwhile.
        if self.parser.screen().alternate_screen() {
            return;
        }
        let evicted = self.parser.callbacks_mut().take_evicted();
        let len = self.parser.screen().scrollback_len();

        // The copy's newest `captured` rows are the history vt100 held at
        // the last capture. Rows only scroll *into* it at the back, and
        // vt100 drops rows off the front — one for each it takes once the
        // history is at its bound, every one of them counted (see
        // `take_evicted`) — so the rows the copy and vt100 still share are
        // the `captured` less the dropped, and anything past them is new.
        // The length alone cannot say that: at the bound it stops changing
        // when rows scroll in, and reading the whole history again there,
        // as the copy used to, widened every row narrower than the screen
        // and cleared its wrap flag (#671).
        //
        // Two things shorten the history instead. RIS and ED 3 empty it,
        // while the rows already copied here survive: the mark starts over,
        // and the rows now at the front are read from the start (#391). A
        // resize that makes the screen taller pulls the newest rows back
        // out of history onto the grid (vt100 0.18, as xterm does): those
        // rows are on screen again, so they leave the copy too, newest
        // first. A resize applied while the alternate screen was up is
        // settled here, the first time the main screen is read again, and
        // the count tells the two ends apart: a shorter screen that pushed
        // a row and dropped one at the bound, then a taller one that pulled
        // the row back, leaves the history one row shorter at the front.
        if std::mem::take(&mut self.history_reset) {
            self.captured = 0;
        }
        let kept = self.captured.saturating_sub(evicted);
        for _ in len..kept {
            self.history.pop_back();
            if let Some(cells) = &mut self.history_cells {
                cells.pop_back();
            }
        }
        self.copy_history(kept.min(len), len);
        self.captured = len;
    }

    /// Append vt100's history rows `from..len` to the copy, oldest first,
    /// and keep the copy within its bound.
    fn copy_history(&mut self, from: usize, len: usize) {
        if from >= len {
            return;
        }
        let (rows, _) = size(self.parser.screen());
        // At offset `k` the visible window starts at history row `len - k`,
        // so `set_scrollback(len - i)` puts history row `i` at the top and
        // the next `rows` rows follow. `Screen::rows` walks the window once,
        // which matters: `Screen::cell` is O(row) per lookup.
        let mut i = from;
        while i < len {
            // The shadow's view moves with the primary's, styled history or
            // not. Moving a view is not only a read in vt100: it widens the
            // history rows it shows to the screen's width, clearing their
            // wrap flags, so a view moved in one parser alone left the two
            // with different rows to pull back onto the grid (#620). Only
            // the rows being copied are ever in view, so a row is widened at
            // most once, as it is copied — and a row a resize pushed, which
            // is copied off the grid, never.
            self.parser.screen_mut().set_scrollback(len - i);
            self.shadow.set_scrollback(len - i);
            let take = (len - i).min(usize::from(rows));
            self.copy_top_rows(take);
            i += take;
        }
        self.parser.screen_mut().set_scrollback(0);
        self.shadow.set_scrollback(0);
    }

    /// Append the top `take` rows of the view on display to the copy, then
    /// trim the copy to its bound from the front.
    ///
    /// Each row is read whole, at the width it has — the width it was
    /// pushed at, or the screen's if that is wider — and never again: a
    /// history row a narrower screen later shows is not cut to fit it.
    fn copy_top_rows(&mut self, take: usize) {
        let screen = self.parser.screen();
        let (_, cols) = size(screen);
        for line in screen.rows(0, u16::MAX).take(take) {
            self.history
                .push_back(Arc::from(restore_replacement(line.trim_end()).as_ref()));
        }
        // Styled history: the same rows as cells, read off the shadow at
        // the same offset so blink, conceal and strikethrough come along —
        // the correspondence invariant extended from the grid to history.
        // Cell reads are O(row) each in vt100, so this is the cost the
        // builder's knob documents.
        if let Some(cells) = &mut self.history_cells {
            for row in 0..u16::try_from(take).unwrap_or(u16::MAX) {
                let mut converted: Vec<Cell> = Vec::with_capacity(usize::from(cols));
                // A row in view is at least as wide as the screen, and a
                // row pushed wider is as wide as it was.
                for col in
                    (0..=u16::MAX).take_while(|&col| col < cols || screen.cell(row, col).is_some())
                {
                    let mut cell = screen.cell(row, col).map_or_else(
                        || Cell::new(String::new(), Style::default(), false, false),
                        |cell| convert_cell(cell, self.shadow.cell(row, col)),
                    );
                    if col > 0 && cell.is_wide_continuation() {
                        if let Some(lead) = converted.last() {
                            cell = Cell::new(String::new(), *lead.style(), false, true);
                        }
                    }
                    converted.push(cell);
                }
                cells.push_back(converted.into());
            }
            while cells.len() > self.scrollback_len {
                cells.pop_front();
            }
        }
        while self.history.len() > self.scrollback_len {
            self.history.pop_front();
        }
    }

    /// Carry out the grid's half of `DECSTR`: [`SOFT_RESET_REPLAY`], then
    /// the cursor back where the reset found it. xterm's soft reset moves
    /// no cursor, but vt100 homes it on each margin and origin reset the
    /// replay makes, and the replay homes it once more to save it there.
    /// The move back is absolute, since origin mode is off by then. A
    /// cursor pending wrap comes back onto the last column, not past it:
    /// `CUP` cannot address the column past the margin.
    fn soft_reset(&mut self) {
        let (row, col) = self.cursor_position();
        self.feed(SOFT_RESET_REPLAY);
        self.feed(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    }

    /// The cursor as a snapshot should report it: a column pending wrap
    /// sits one past the last cell inside the backend, and no terminal
    /// reports a column its own width does not have (#401). Clamping here
    /// keeps the snapshot, the cursor-position report and a graphics
    /// placement agreeing with the grid they describe. Tab stops read the
    /// raw column instead — see `apply_tabs`.
    fn cursor_position(&self) -> (u16, u16) {
        let screen = self.parser.screen();
        let (row, col) = screen.cursor_position();
        (row, col.min(size(screen).1.saturating_sub(1)))
    }

    /// Copy the rows a resize to `rows` is about to push into history, off
    /// the grid, before they get there.
    ///
    /// vt100 pushes the top rows of a shorter screen into history at the
    /// width they have, and a wider screen leaves them that width; reading
    /// them out of history afterwards would widen them and clear their wrap
    /// flags (#620). On the grid they can be read as they are. How many
    /// rows go is vt100's own rule — the rows below the cursor are dropped
    /// first, and only the rest of the shortfall is pushed — and the mark
    /// is moved past them, so the capture after the resize finds them
    /// already copied. The alternate screen hides the main grid, so a
    /// resize while it is up is left to the capture that runs once it is
    /// down.
    fn copy_rows_a_resize_pushes(&mut self, rows: u16) {
        let (old_rows, _) = size(self.parser.screen());
        if self.scrollback_len == 0 || self.parser.screen().alternate_screen() || rows >= old_rows {
            return;
        }
        // Every feed ends in a capture, so this finds nothing to do; it is
        // what lets the mark below count on the copy being current.
        self.capture_scrolled_rows();
        let screen = self.parser.screen();
        let below = old_rows - screen.cursor_position().0 - 1;
        let pushed = (old_rows - rows).saturating_sub(below);
        if pushed > 0 {
            self.copy_top_rows(usize::from(pushed));
            self.captured += usize::from(pushed);
        }
    }

    /// Feed an emoji-presentation selector (U+FE0F) at the live cursor.
    ///
    /// The selector joins the cell before the cursor, and when that makes
    /// the cell two columns wide vt100 widens it in place — or, when the
    /// cell is the last column's, moves it to the start of the next row,
    /// as xterm does. Two cases are not left to vt100:
    ///
    /// - The last column's, always. vt100 0.18.1 takes the cell to move
    ///   *before* the row scrolls, so at the bottom margin it moves the
    ///   blank cell of the new row and leaves the real one wide in the last
    ///   column of the row above, with no second column; the next write
    ///   over it panics inside vt100 (#619). The move is written out
    ///   instead, in steps vt100 gets right: erase the cell, line feed —
    ///   which scrolls exactly where the wrap would have — and draw the
    ///   cell again at the start of the row. That is vt100's own result
    ///   for a row that does not scroll, the row above left unwrapped
    ///   included.
    /// - Insert mode, which reserves the column the widening takes.
    fn draw_selector(&mut self) {
        let screen = self.parser.screen();
        let (_, cols) = size(screen);
        let (row, col) = screen.cursor_position();
        // vt100's base cell: the one before the cursor, or the first half of
        // a wide character whose second half that is.
        let base = match col.checked_sub(1) {
            Some(prev)
                if prev > 0
                    && screen
                        .cell(row, prev)
                        .is_some_and(::vt100::Cell::is_wide_continuation) =>
            {
                Some(prev - 1)
            }
            prev => prev,
        };
        let widened = base
            .and_then(|base| screen.cell(row, base))
            .filter(|cell| !cell.is_wide())
            .map(|cell| cell.contents().to_owned())
            .filter(|contents| widens(contents));
        let insert = self.tracker.insert_mode();
        match widened {
            Some(contents) if col >= cols => {
                let reserve = if insert { "\x1b[2@" } else { "" };
                let moved = format!(
                    "\x1b[{cols}G\x1b[X\n\x1b[1G{reserve}{contents}{}",
                    crate::width::VS16
                );
                self.feed(moved.as_bytes());
            }
            Some(_) if insert => self.feed(b"\x1b[@\xef\xb8\x8f"),
            _ => self.feed(VS16_BYTES),
        }
    }

    /// Make room, in insert mode, for the characters starting at `text`
    /// that the cursor's row takes as it stands, and return how many bytes
    /// of `text` that covers.
    ///
    /// vt100 does not model IRM, so the room is reserved with the `ICH` it
    /// does dispatch, sized in the columns the grid gives each character —
    /// its own layout, which no `unicode-width` measure is (#580). A
    /// reservation per character is what a terminal does, and one for a
    /// run of them that fits on the row is the same reservation made once.
    /// So the run stops where a character would wrap, and at a selector,
    /// which widens the character before it and reserves for itself (see
    /// `draw_selector`). A character that wraps is reserved for on the row
    /// it lands on — see `reserve_after_wrap` — so the result no longer
    /// depends on whether the PTY delivered the run whole (#624).
    fn reserve_insert(&mut self, text: &[u8]) -> usize {
        let run = &text[..text
            .iter()
            .position(|&b| !is_printable(b))
            .unwrap_or(text.len())];
        let run = match std::str::from_utf8(run) {
            Ok(run) => run,
            Err(e) => std::str::from_utf8(&run[..e.valid_up_to()]).unwrap_or_default(),
        };
        let screen = self.parser.screen();
        let (_, cols) = size(screen);
        let (row, col) = screen.cursor_position();
        let room = usize::from(cols.saturating_sub(col));
        let mut width = 0;
        let mut end = 0;
        for (at, c) in run.char_indices() {
            if c == crate::width::VS16 && at > 0 {
                break;
            }
            // A character wider than the screen is not drawn at all.
            let w = crate::width::char_columns(c)
                .filter(|&w| w <= usize::from(cols))
                .unwrap_or(0);
            if w > 0 && width + w > room {
                if at == 0 {
                    self.reserve_after_wrap(w, row, col);
                    return c.len_utf8();
                }
                break;
            }
            width += w;
            end = at + c.len_utf8();
        }
        if width > 0 {
            self.feed(format!("\x1b[{width}@").as_bytes());
        }
        // Never zero, so the caller always moves on: a byte that is not
        // UTF-8 cannot reach here past the reader's sanitizer, but if one
        // did it would be fed as it always was.
        end.max(1)
    }

    /// Reserve `width` columns for a character that wraps from `(row, col)`
    /// onto the next row, where xterm inserts it: that row's text moves
    /// right as any other row's does (#624).
    ///
    /// The wrap is vt100's: it scrolls, or not, and marks the row wrapped.
    /// The reservation is made first, on the row the wrap lands on, and the
    /// cursor put back where it was. A cursor down finds that row — it
    /// stops where the wrap's line feed would scroll, and there the row the
    /// character lands on is a new, blank one with nothing to move. A
    /// cursor pending wrap cannot be put back by a cursor movement, only by
    /// drawing the last column again, which is redrawn as it stands.
    fn reserve_after_wrap(&mut self, width: usize, row: u16, col: u16) {
        self.feed(b"\x1b[B");
        if self.parser.screen().cursor_position().0 == row {
            return;
        }
        self.feed(format!("\x1b[1G\x1b[{width}@\x1b[{}d", row + 1).as_bytes());
        let (_, cols) = size(self.parser.screen());
        if col < cols {
            self.feed(format!("\x1b[{}G", col + 1).as_bytes());
        } else {
            self.redraw_last_column(row);
        }
    }

    /// Draw the last column of `row` again, as it stands, leaving the cursor
    /// pending wrap after it — the one way to put a cursor back there.
    ///
    /// The cell is drawn in its own attributes, which the two parsers hold
    /// differently: the shadow's bold, italic and underline are the
    /// primary's blink, conceal and strikethrough (`emu/shadow.rs`). So each
    /// parser is handed its own SGR, then its own current attributes back.
    /// An empty last column — erased while the cursor stood past it — is not
    /// redrawn, and the cursor is left at the start of the next row: vt100
    /// marks no wrap past an empty column, so there is none to recreate.
    fn redraw_last_column(&mut self, row: u16) {
        let screen = self.parser.screen();
        let (_, cols) = size(screen);
        let last = cols - 1;
        let col = if last > 0
            && screen
                .cell(row, last)
                .is_some_and(::vt100::Cell::is_wide_continuation)
        {
            last - 1
        } else {
            last
        };
        let Some(cell) = screen.cell(row, col).filter(|cell| cell.has_contents()) else {
            self.feed(format!("\x1b[{}d\x1b[1G", row + 2).as_bytes());
            return;
        };
        let at = format!("\x1b[{}G", col + 1);
        let contents = cell.contents().to_owned();
        let primary = format!(
            "{at}{}{contents}{}",
            primary_sgr(cell),
            screen.attributes_formatted()
        );
        let shadow = self.shadow.redraw(row, col, &at, &contents);
        self.parser.process(primary.as_bytes());
        self.shadow.feed_verbatim(shadow.as_bytes());
    }
}

/// U+FE0F as the bytes it arrives in. Its first byte is a UTF-8 lead byte,
/// never a continuation or an ASCII byte, so matching it at a character
/// boundary is enough, and no character-set glyph is ever staged from it.
const VS16_BYTES: &[u8] = "\u{fe0f}".as_bytes();

/// Whether an emoji-presentation selector joining a cell that holds
/// `contents` makes it two columns wide: vt100's own test, on the contents
/// it would hold (an empty cell gains a space first). A cell already
/// holding 18 bytes takes no more characters at all, so nothing widens it.
fn widens(contents: &str) -> bool {
    use unicode_width::UnicodeWidthStr as _;
    let base = if contents.is_empty() { " " } else { contents };
    contents.len() < 18 && format!("{base}{}", crate::width::VS16).width() > 1
}

/// The SGR that sets `cell`'s attributes on the primary parser, from a
/// reset: vt100's own reading of each, so the cell drawn with it is the
/// cell it was.
fn primary_sgr(cell: &::vt100::Cell) -> String {
    let mut sgr = String::from("\x1b[0");
    let color = |sgr: &mut String, color: ::vt100::Color, base: u8| match color {
        ::vt100::Color::Default => {}
        ::vt100::Color::Idx(i) => sgr.push_str(&format!(";{base};5;{i}")),
        ::vt100::Color::Rgb(r, g, b) => sgr.push_str(&format!(";{base};2;{r};{g};{b}")),
    };
    color(&mut sgr, cell.fgcolor(), 38);
    color(&mut sgr, cell.bgcolor(), 48);
    for (on, param) in [
        (cell.bold(), ";1"),
        (cell.dim(), ";2"),
        (cell.italic(), ";3"),
        (cell.underline(), ";4"),
        (cell.inverse(), ";7"),
    ] {
        if on {
            sgr.push_str(param);
        }
    }
    sgr.push('m');
    sgr
}

impl Emulator for Vt100Emulator {
    fn process(&mut self, bytes: &[u8]) -> Processed {
        // How much of this segment the grid has already been given. A
        // graphics payload moves it mid-segment, and so does a byte the
        // invoked character set redefines: that byte is staged as the glyph
        // it draws instead of being sliced through, and everything else is
        // fed in one go, exactly as before.
        let mut fed = 0;
        // Where the bytes a reservation or a selector already accounted for
        // end, so each character is looked at once.
        let mut run_until = 0;
        for (i, &byte) in bytes.iter().enumerate() {
            // A character is handled at the live cursor, with everything
            // owed to the grid fed first, wherever it falls in the read:
            // the screen must not depend on where the PTY split the output
            // (#624). Two kinds need it. The emoji-presentation selector can
            // widen the cell before it, which vt100 lays out wrong at the
            // bottom margin (#619) and which insert mode must make room for.
            // And in insert mode every character pushes the rest of its row
            // right — see `reserve_insert`.
            if i >= run_until && !self.tracker.mid_sequence() && is_printable(byte) {
                if bytes[i..].starts_with(VS16_BYTES) {
                    self.feed_staged(&bytes[fed..i]);
                    self.draw_selector();
                    fed = i + VS16_BYTES.len();
                    run_until = fed;
                } else if self.tracker.insert_mode() {
                    self.feed_staged(&bytes[fed..i]);
                    fed = i;
                    run_until = i + self.reserve_insert(&bytes[i..]);
                }
            }
            // The body of an OSC string past the tracker's bound is cut out
            // of what both parsers are fed, so neither holds more of it than
            // the tracker does (#694). Only the first byte of a run feeds
            // anything; the rest find nothing owed.
            if self.tracker.withholds(byte) {
                self.feed_staged(&bytes[fed..i]);
                fed = i + 1;
            }
            // Asked before the step, because whether this byte is a character
            // at all depends on the state the tracker is in before it — and
            // a designation's own final byte must not be drawn.
            if let Some(glyph) = self.tracker.charset_glyph(byte) {
                self.staged.extend_from_slice(&bytes[fed..i]);
                self.staged.extend_from_slice(glyph.as_bytes());
                fed = i + 1;
            }
            let stop = match self.tracker.step(byte) {
                SeqEvent::SyncEnd => Some(Stop::FrameComplete(self.close_frame())),
                SeqEvent::Query(query) => Some(Stop::Query(query)),
                SeqEvent::Graphics(payload, placement) => {
                    self.feed_staged(&bytes[fed..=i]);
                    fed = i + 1;
                    self.place_graphics(*payload, &placement);
                    None
                }
                // The upload a query completed is placed before the query
                // is answered, as kitty places it before it replies (#672).
                SeqEvent::GraphicsQuery(payload, placement, query) => {
                    self.feed_staged(&bytes[fed..=i]);
                    fed = i + 1;
                    self.place_graphics(*payload, &placement);
                    Some(Stop::Query(query))
                }
                SeqEvent::SoftReset => {
                    // The tracker has reset what it holds; the grid's
                    // modes are vt100's, which does not implement DECSTR,
                    // so it is handed the sequences that reset them one by
                    // one. The soft reset itself goes through first, in
                    // stream order, then the replay.
                    self.feed_staged(&bytes[fed..=i]);
                    fed = i + 1;
                    self.soft_reset();
                    None
                }
                SeqEvent::HardReset => {
                    // The tracker closed any open update; the frame it
                    // began is dropped with it (#623).
                    self.frame_started = None;
                    self.feed_history_reset(&bytes[fed..i], &bytes[i..=i], true);
                    fed = i + 1;
                    None
                }
                SeqEvent::ScrollbackErased => {
                    self.feed_history_reset(&bytes[fed..i], &bytes[i..=i], false);
                    fed = i + 1;
                    None
                }
                SeqEvent::AlternateScreen { leaving } => {
                    // On the way in, everything before the final byte goes
                    // first, so the capture that feed runs reads the main
                    // screen's history while it is still the one on display
                    // (#622); the final byte goes with the rest of the read.
                    //
                    // On the way out the final byte goes too, so that
                    // capture reads the main screen back before the rest of
                    // the read scrolls it. A taller resize while it was
                    // hidden pulled rows back out of its history; settled in
                    // one feed with new rows scrolling off, the rows pulled
                    // back and the rows pushed cancelled in the length, and
                    // the copy kept a row the grid had taken back and lost
                    // the row that scrolled, by where the read split (#682).
                    let end = if leaving { i + 1 } else { i };
                    self.feed_staged(&bytes[fed..end]);
                    fed = end;
                    None
                }
                SeqEvent::Tabs(op) => {
                    self.apply_tabs(op, &bytes[fed..i], &bytes[i..=i]);
                    fed = i + 1;
                    None
                }
                SeqEvent::None => None,
                SeqEvent::SyncBegin => {
                    // Stamped here, at the byte that opened the update,
                    // rather than when the read arrived: a read can carry a
                    // whole burst, and stamping per chunk would fold PTY
                    // scheduling into the measurement.
                    self.frame_started = Some(Instant::now());
                    None
                }
            };
            if let Some(stop) = stop {
                self.feed_staged(&bytes[fed..=i]);
                return Processed {
                    consumed: i + 1,
                    stop: Some(stop),
                };
            }
        }
        self.feed_staged(&bytes[fed..]);
        Processed {
            consumed: bytes.len(),
            stop: None,
        }
    }

    fn snapshot(&self) -> Screen {
        let unsupported = self.parser.callbacks().shapes();
        let unsupported_overflow = self.parser.callbacks().overflow();
        let visual_bells = self.parser.callbacks().visual_bells();
        let screen = self.parser.screen();
        let (rows, cols) = size(screen);
        let mut cells: Vec<Cell> = Vec::with_capacity(usize::from(rows) * usize::from(cols));
        // The shadow grid is the same shape as the primary — vt100's
        // attributes never influence geometry, and the two streams differ
        // only by rewritten SGR — so cell (row, col) means the same in both.
        debug_assert_eq!(
            screen.contents(),
            self.shadow.contents(),
            "the attribute shadow diverged from the primary grid"
        );
        for row in 0..rows {
            for col in 0..cols {
                // In-range lookups on vt100 are always Some; blank fallback
                // keeps this total rather than panicking inside a snapshot.
                let mut converted = screen.cell(row, col).map_or_else(
                    || Cell::new(String::new(), Style::default(), false, false),
                    |cell| convert_cell(cell, self.shadow.cell(row, col)),
                );
                // A wide character's second column is painted in the leading
                // cell's colours, so it carries the leading cell's style.
                // vt100 leaves the placeholder unstyled, which split every
                // highlight over CJK or emoji into two spans with a hole
                // (#218) — the same shape as the shadow recovery: upstream
                // drops it, the snapshot restores it.
                if col > 0 && converted.is_wide_continuation() {
                    if let Some(lead) = cells.last() {
                        converted = Cell::new(String::new(), *lead.style(), false, true);
                    }
                }
                cells.push(converted);
            }
        }
        let (cursor_row, cursor_col) = self.cursor_position();
        let state = TermState {
            title: self.tracker.title(),
            alternate_screen: screen.alternate_screen(),
            bracketed_paste: screen.bracketed_paste(),
            application_cursor: screen.application_cursor(),
            mouse: self.tracker.mouse_protocol(),
            mouse_modes: self.tracker.mouse_tracking(),
            clipboard: self.tracker.clipboard(),
            bells: self.tracker.bells(),
            focus_events: self.tracker.focus_events(),
            cursor_style: self.tracker.cursor_style(),
            links: self.tracker.links(),
            graphics: GraphicsSeen::new(self.tracker.graphics(), Arc::clone(&self.graphics)),
            // Filled in by the terminal, which owns the frame count.
            repaints: 0,
            scrollback: self.history.iter().cloned().collect(),
            scrollback_cells: self
                .history_cells
                .as_ref()
                .map(|cells| cells.iter().cloned().collect()),
            // The backend's own record of a soft wrap, per row (#265).
            wrapped: (0..rows).map(|row| screen.row_wrapped(row)).collect(),
            insert_mode: self.tracker.insert_mode(),
            unsupported,
            unsupported_overflow,
            visual_bells,
        };
        Screen::from_parts(
            cols,
            rows,
            cursor_row,
            cursor_col,
            !screen.hide_cursor(),
            cells,
            state,
        )
    }

    fn reported_position(&self) -> (u16, u16) {
        // The inherent method, which `snapshot` uses too, so a reply and a
        // snapshot cannot disagree — #401's clamp included. A cursor above
        // the margin under origin mode — a `DECRC` can put it there — has
        // no row to count; xterm would report zero or less, and the
        // margin's own row is the nearest a `CUP` can reach.
        let (row, col) = Vt100Emulator::cursor_position(self);
        (row.saturating_sub(self.tracker.report_origin_row()), col)
    }

    fn size(&self) -> (u16, u16) {
        size(self.parser.screen())
    }

    fn mid_sequence(&self) -> bool {
        self.tracker.mid_sequence()
    }

    fn in_sync_update(&self) -> bool {
        self.tracker.in_sync_update()
    }

    fn input_modes(&self) -> InputModes {
        let screen = self.parser.screen();
        InputModes {
            // The tracker's, not vt100's: vt100 goes on reporting after a
            // reset of a tracking mode other than the one in force, where
            // xterm stops (#693).
            mouse: self.tracker.mouse_protocol(),
            // The tracker's too: vt100 does not implement 1015, and so
            // cannot say when it replaced or cleared the others (#640).
            mouse_encoding: self.tracker.mouse_encoding(),
            bracketed_paste: screen.bracketed_paste(),
            application_cursor: screen.application_cursor(),
            focus_events: self.tracker.focus_events(),
        }
    }

    fn mode_state(&self, mode: u32) -> ModeState {
        let screen = self.parser.screen();
        let on = |set: bool| {
            if set {
                ModeState::Set
            } else {
                ModeState::Reset
            }
        };
        // Only modes whose state we hold exactly.
        match mode {
            // Synchronized output. Answering at all is the point: an
            // application that probes before bracketing its repaints can
            // then use it, which is what makes wait_frame work against
            // programs we haven't modified.
            2026 => on(self.tracker.in_sync_update()),
            1 => on(screen.application_cursor()),
            25 => on(!screen.hide_cursor()),
            47 | 1047 | 1049 => on(screen.alternate_screen()),
            2004 => on(screen.bracketed_paste()),
            // Tracked by termlens itself, so the state is exact — which is
            // what the honesty rule requires before answering.
            1004 => on(self.tracker.focus_events()),
            1005 => on(self.tracker.mouse_encoding() == MouseEncoding::Utf8),
            1006 => on(self.tracker.mouse_encoding() == MouseEncoding::Sgr),
            1015 => on(self.tracker.mouse_encoding() == MouseEncoding::Urxvt),
            // The mouse tracking modes, each on its own evidence: the
            // tracker keeps the set the application asked for (#151). vt100
            // collapses the four into one value, so before this a probe for
            // `1002` while `1003` was also on had to answer "not
            // recognized" — the only honest reply to a question the state
            // could not answer, and one crossterm's three-at-once enable
            // provoked on every run.
            9 => on(self.tracker.mouse_tracking().contains(MouseMode::Press)),
            1000 => on(self
                .tracker
                .mouse_tracking()
                .contains(MouseMode::PressRelease)),
            1002 => on(self
                .tracker
                .mouse_tracking()
                .contains(MouseMode::ButtonMotion)),
            1003 => on(self.tracker.mouse_tracking().contains(MouseMode::AnyMotion)),
            _ => ModeState::NotRecognized,
        }
    }

    fn set_size(&mut self, rows: u16, cols: u16) {
        self.copy_rows_a_resize_pushes(rows);
        // The parser's own resize, not the screen's: it reports each row it
        // drops off the front of a full history, which the capture below
        // counts on (#671). The shadow keeps no count and needs none.
        self.parser.set_size(nz(rows), nz(cols));
        self.shadow.set_size(rows, cols);
        self.tracker.set_size(rows, cols);
        // A resize can push rows into history on its own, drop rows from
        // its front, or pull rows back out of it.
        self.capture_scrolled_rows();
    }
}

/// What `DECSTR` resets of the state vt100 holds, as the sequences vt100
/// understands, following xterm's soft reset: cursor keys back to normal
/// (`DECCKM`), the cursor visible (`DECTCEM`), the rendition cleared
/// (`SGR 0`), the margins and origin mode cleared (`DECSTBM`, `DECOM`), and
/// the cursor saved at home with all of that (`DECSC`), which is where
/// xterm leaves its saved cursor. [`Vt100Emulator::soft_reset`] then puts
/// the cursor back.
///
/// Mouse tracking and its encoding, bracketed paste and the alternate
/// screen are left alone, as xterm leaves them: it resets the mouse modes
/// and bracketed paste only on `RIS` (#687). The keypad mode is not replayed, since nothing on
/// `Screen` reads it and a replay nothing can check is a claim nothing can
/// catch.
const SOFT_RESET_REPLAY: &[u8] = b"\x1b[?1l\x1b[?25h\x1b[0m\x1b[r\x1b[?6l\x1b7";

/// Build a [`Cell`] from the primary grid cell and its shadow counterpart,
/// whose bold/italic/underline flags are this cell's
/// blink/conceal/strikethrough (see `emu/shadow.rs`).
fn convert_cell(cell: &::vt100::Cell, shadow: Option<&::vt100::Cell>) -> Cell {
    let style = Style {
        fg: convert_color(cell.fgcolor()),
        bg: convert_color(cell.bgcolor()),
        bold: cell.bold(),
        dim: cell.dim(),
        italic: cell.italic(),
        underline: cell.underline(),
        reverse: cell.inverse(),
        blink: shadow.is_some_and(::vt100::Cell::bold),
        conceal: shadow.is_some_and(::vt100::Cell::italic),
        strikethrough: shadow.is_some_and(::vt100::Cell::underline),
    };
    Cell::new(
        restore_replacement(cell.contents()).into_owned(),
        style,
        cell.is_wide(),
        cell.is_wide_continuation(),
    )
}

/// A byte that draws: not a C0 control, not DEL, not ESC. Continuation
/// bytes of a multi-byte character count, so a run is measured whole.
fn is_printable(b: u8) -> bool {
    b >= 0x20 && b != 0x7f
}

/// What the reader substitutes for a byte it could not decode (`utf8.rs`).
const REPLACEMENT: &str = "\u{FFFD}";
/// What the grid is fed in its place. vt100's `print` drops U+FFFD outright
/// (0.16 through 0.18, `perform.rs`), so the replacement would vanish as the
/// invalid byte did (#217). U+FDD0 is a Unicode noncharacter — reserved for
/// internal use like this, never for interchange, so no application has any
/// business emitting one — and vt100 draws it as an ordinary width-1 cell.
/// The snapshot puts U+FFFD back wherever it appears.
const STAND_IN: &str = "\u{FDD0}";

/// `bytes` with every encoded U+FFFD swapped for the stand-in, allocating
/// only when there is one to swap. A stop never falls inside a character,
/// so the three bytes always arrive in one feed.
fn stand_in_for_replacement(bytes: &[u8]) -> Cow<'_, [u8]> {
    let from = REPLACEMENT.as_bytes();
    if !bytes.windows(from.len()).any(|window| window == from) {
        return Cow::Borrowed(bytes);
    }
    let to = STAND_IN.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Cow::Owned(out)
}

/// Grid text as the snapshot reports it: the stand-in reads as U+FFFD again.
fn restore_replacement(text: &str) -> Cow<'_, str> {
    if text.contains(STAND_IN) {
        Cow::Owned(text.replace(STAND_IN, REPLACEMENT))
    } else {
        Cow::Borrowed(text)
    }
}

fn convert_color(color: ::vt100::Color) -> Color {
    match color {
        ::vt100::Color::Default => Color::Default,
        ::vt100::Color::Idx(i) => Color::Indexed(i),
        ::vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// vt100 0.18 takes and returns sizes as `NonZeroU16`. termlens never asks
/// for a zero dimension (the builder's floor is 2, a parsed screen's 1), so
/// the clamp to 1 never fires; it keeps the conversion total.
pub(super) fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
}

/// The screen's `(rows, cols)` as plain numbers.
fn size(screen: &::vt100::Screen) -> (u16, u16) {
    let (rows, cols) = screen.size();
    (rows.get(), cols.get())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a whole stream, looping across frame-boundary stops.
    fn feed_all(emu: &mut Vt100Emulator, bytes: &[u8]) {
        let mut off = 0;
        while off < bytes.len() {
            off += emu.process(&bytes[off..]).consumed;
        }
    }

    fn emu_with_size(rows: u16, cols: u16, bytes: &[u8]) -> Vt100Emulator {
        let mut emu = Vt100Emulator::new(rows, cols, 0, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, bytes);
        emu
    }

    fn emu_with(bytes: &[u8]) -> Vt100Emulator {
        let mut emu = Vt100Emulator::new(4, 10, 0, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, bytes);
        emu
    }

    #[test]
    fn renders_plain_text_into_the_grid() {
        let emu = emu_with(b"hi\r\nthere");
        let screen = emu.snapshot();
        assert_eq!(screen.size(), (10, 4));
        assert_eq!(screen.text(), "hi\nthere\n\n");
        assert_eq!(screen.cursor(), (1, 5, true));
    }

    #[test]
    fn positions_wide_characters_with_continuations() {
        let emu = emu_with("汉x".as_bytes());
        let screen = emu.snapshot();
        let wide = screen.cell(0, 0).unwrap();
        assert!(wide.is_wide());
        assert_eq!(wide.contents(), "汉");
        assert!(screen.cell(0, 1).unwrap().is_wide_continuation());
        assert_eq!(screen.find("x"), Some((0, 2)));
    }

    #[test]
    fn a_wide_characters_continuation_carries_the_leading_cells_style() {
        // `ab汉cd` on one background: six columns, one span, no hole at the
        // continuation column (#218).
        let screen = emu_with(b"\x1b[48;2;30;30;46mab\xe6\xb1\x89cd\x1b[0m").snapshot();
        let bg = Color::Rgb(30, 30, 46);
        assert!(screen.cell(0, 3).unwrap().is_wide_continuation());
        for col in 0..6 {
            assert_eq!(screen.cell(0, col).unwrap().style().bg, bg, "col {col}");
        }
        let styled = screen.with_styles().to_string();
        assert!(styled.contains("0: 0-5 bg=#1e1e2e"), "{styled}");
        assert_eq!(screen.cell(0, 6).unwrap().style(), &Style::default());
    }

    #[test]
    fn a_replacement_character_is_drawn_like_any_other() {
        // The reader turns invalid UTF-8 into U+FFFD (#217); the grid has to
        // show it, or the byte is as invisible as before.
        let screen = emu_with("caf\u{FFFD} done".as_bytes()).snapshot();
        assert_eq!(screen.row_text(0).trim_end(), "caf\u{FFFD} done");
        assert_eq!(screen.cell(0, 3).unwrap().contents(), "\u{FFFD}");
        assert_eq!(screen.find("done"), Some((0, 5)));

        // Scrolled into history, it is still U+FFFD — never the stand-in.
        let mut emu = Vt100Emulator::new(4, 10, 100, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(
            &mut emu,
            "caf\u{FFFD} done\r\n1\r\n2\r\n3\r\n4\r\n5".as_bytes(),
        );
        let screen = emu.snapshot();
        let history = screen.scrollback_text();
        assert!(history.contains("caf\u{FFFD} done"), "{history:?}");
        assert!(
            !screen.full_text().contains(STAND_IN),
            "{}",
            screen.full_text()
        );
    }

    #[test]
    fn captures_sgr_styles() {
        let emu = emu_with(b"\x1b[1;3;4;7;31mX\x1b[0m");
        let screen = emu.snapshot();
        let style = *screen.cell(0, 0).unwrap().style();
        assert!(style.bold && style.italic && style.underline && style.reverse);
        assert_eq!(style.fg, Color::Indexed(1));
        assert_eq!(screen.cell(0, 1).unwrap().style(), &Style::default());
    }

    #[test]
    fn bold_and_dim_are_one_intensity_state_and_the_last_write_wins() {
        // Documented on `Style::bold` and `Style::dim` (#225): the backend
        // keeps one intensity, so a cell never reports both, and SGR 22
        // clears whichever is set.
        let screen = emu_with(b"\x1b[1;2mA\x1b[0m\x1b[2;1mB\x1b[0m\x1b[1m\x1b[22mC").snapshot();
        let style = |col| *screen.cell(0, col).unwrap().style();
        assert!(!style(0).bold && style(0).dim, "ESC[1;2m: {:?}", style(0));
        assert!(style(1).bold && !style(1).dim, "ESC[2;1m: {:?}", style(1));
        assert!(
            !style(2).bold && !style(2).dim,
            "ESC[1m ESC[22m: {:?}",
            style(2)
        );
    }

    #[test]
    fn blink_conceal_and_strikethrough_reach_the_cells() {
        // The three attributes vt100 drops. Recovered via the shadow parser
        // (see `emu/shadow.rs`).
        let emu = emu_with(b"\x1b[5mB\x1b[0m\x1b[8mC\x1b[0m\x1b[9mS\x1b[0mp");
        let s = emu.snapshot();
        let style = |col| *s.cell(0, col).unwrap().style();

        assert!(style(0).blink && !style(0).conceal && !style(0).strikethrough);
        assert!(style(1).conceal && !style(1).blink && !style(1).strikethrough);
        assert!(style(2).strikethrough && !style(2).blink && !style(2).conceal);
        // And a plain cell after the reset carries none of them.
        assert_eq!(style(3), Style::default());
    }

    #[test]
    fn the_new_attributes_coexist_with_the_old_ones() {
        // A real bold must not read as a blink, and vice versa: the two
        // parsers must not leak into each other.
        let emu = emu_with(b"\x1b[1;31mA\x1b[0m\x1b[5mB\x1b[0m\x1b[1;5;4mC");
        let s = emu.snapshot();
        let a = *s.cell(0, 0).unwrap().style();
        assert!(a.bold && !a.blink);
        assert_eq!(a.fg, Color::Indexed(1));

        let b = *s.cell(0, 1).unwrap().style();
        assert!(b.blink && !b.bold);

        let c = *s.cell(0, 2).unwrap().style();
        assert!(c.bold && c.blink && c.underline && !c.strikethrough);
    }

    #[test]
    fn each_attribute_has_its_own_reset() {
        let emu = emu_with(b"\x1b[5;8;9mX\x1b[25mY\x1b[28mZ\x1b[29mW");
        let s = emu.snapshot();
        let style = |col| *s.cell(0, col).unwrap().style();

        let x = style(0);
        assert!(x.blink && x.conceal && x.strikethrough);
        let y = style(1);
        assert!(!y.blink && y.conceal && y.strikethrough);
        let z = style(2);
        assert!(!z.blink && !z.conceal && z.strikethrough);
        assert_eq!(style(3), Style::default());
    }

    #[test]
    fn a_palette_colour_is_never_mistaken_for_an_attribute() {
        // `38;5;196` selects palette entry 196. Reading its `5` as blink
        // would mark a whole run with an attribute the application never
        // set — and 256-colour output is everywhere.
        let emu = emu_with(b"\x1b[38;5;196mX\x1b[0m\x1b[38;2;0;8;9mY");
        let s = emu.snapshot();
        let x = *s.cell(0, 0).unwrap().style();
        assert_eq!(x.fg, Color::Indexed(196));
        assert!(!x.blink && !x.conceal && !x.strikethrough);

        let y = *s.cell(0, 1).unwrap().style();
        assert_eq!(y.fg, Color::Rgb(0, 8, 9));
        assert!(!y.conceal && !y.strikethrough);
    }

    #[test]
    fn colon_form_rgb_colours_match_semicolon_form() {
        let emu = emu_with(b"\x1b[38;2;10;20;30mA\x1b[0m\x1b[38:2::10:20:30mB");
        let s = emu.snapshot();
        assert_eq!(s.cell(0, 0).unwrap().style().fg, Color::Rgb(10, 20, 30));
        assert_eq!(s.cell(0, 1).unwrap().style().fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn colon_form_colours_support_optional_colour_space_and_chunking() {
        let mut emu = Vt100Emulator::new(4, 10, 0, crate::graphics::DEFAULT_CAPTURE, false);
        emu.feed(b"\x1b[38:2:10:");
        emu.feed(b"20:30mA\x1b[48:5:196mB");
        let s = emu.snapshot();

        let foreground = s.cell(0, 0).unwrap().style();
        assert_eq!(foreground.fg, Color::Rgb(10, 20, 30));
        assert_eq!(foreground.bg, Color::Default);

        let background = s.cell(0, 1).unwrap().style();
        assert_eq!(background.fg, Color::Rgb(10, 20, 30));
        assert_eq!(background.bg, Color::Indexed(196));
    }

    #[test]
    fn attributes_survive_the_geometry_the_shadow_must_track() {
        // Erase fills cells with the current attributes, scrolling moves
        // rows, and the alternate screen swaps grids. The shadow follows the
        // same byte stream, so all three must line up — the snapshot's
        // debug assertion checks the grids match on every call here.
        let emu = emu_with(b"\x1b[8mmasked\r\nrow2\r\nrow3\r\nrow4\r\nrow5");
        let s = emu.snapshot();
        // "masked" scrolled off; every remaining cell is still concealed.
        assert!(s.cell(0, 0).unwrap().style().conceal, "{s}");
        assert!(s.cell(3, 0).unwrap().style().conceal, "{s}");

        let emu = emu_with(b"\x1b[9mstruck\x1b[?1049hALT");
        let s = emu.snapshot();
        assert!(s.cell(0, 0).unwrap().style().strikethrough, "{s}");
    }

    /// `ESC ( 0 l q q q k` is a box. Both parsers receive the translated
    /// stream — the snapshot's correspondence check would fail otherwise.
    #[test]
    fn dec_special_graphics_reaches_the_grid_as_the_glyphs_it_draws() {
        let emu = emu_with(b"\x1b(0lqqqk\x1b(B\r\nplain");
        let s = emu.snapshot();
        assert_eq!(
            s.text(),
            "\u{250c}\u{2500}\u{2500}\u{2500}\u{2510}\nplain\n\n"
        );
        assert_eq!(s.find("\u{2510}"), Some((0, 4)), "one cell per glyph");
        assert!(!s.contains("lqqqk"));

        // SO/SI with the set in G1, the vt100-terminfo shape.
        let emu = emu_with(b"\x1b)0\x0elqk\x0f ok");
        assert_eq!(
            emu.snapshot().row_text(0).trim_end(),
            "\u{250c}\u{2500}\u{2510} ok"
        );

        // G2 + SS2 and G3 + SS3: one graphic, then the locked set resumes.
        // `|` is itself a Special Graphics byte, so a stuck shift is `≠`.
        let emu = emu_with(b"\x1b*0\x1bNl\x1b(B|");
        assert_eq!(emu.snapshot().row_text(0).trim_end(), "\u{250c}|");
        let emu = emu_with(b"\x1b+0\x1bOl\x1b(B|");
        assert_eq!(emu.snapshot().row_text(0).trim_end(), "\u{250c}|");
    }

    /// A style set inside the graphics set travels with the glyph, and the
    /// attribute shadow stays in step: the rewrite happens before either
    /// parser, so the two grids are still the same shape.
    #[test]
    fn a_styled_line_drawing_run_keeps_its_style_and_the_shadow_in_step() {
        let emu = emu_with(b"\x1b(0\x1b[31;9mqq\x1b[0m\x1b(Bq");
        let s = emu.snapshot();
        assert_eq!(s.row_text(0).trim_end(), "\u{2500}\u{2500}q");
        let drawn = *s.cell(0, 0).unwrap().style();
        assert_eq!(drawn.fg, Color::Indexed(1));
        assert!(drawn.strikethrough, "the shadow-carried attribute too");
        assert_eq!(s.cell(0, 2).unwrap().style(), &Style::default());
    }

    /// A translated byte that also closes a frame, split across feeds: the
    /// staged glyph must reach the grid before the stop is reported, and a
    /// designation split across chunks must still apply.
    #[test]
    fn translation_survives_chunk_boundaries_and_frame_stops() {
        let mut emu = Vt100Emulator::new(4, 10, 0, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, b"\x1b(");
        feed_all(&mut emu, b"0\x1b[?2026hl");
        feed_all(&mut emu, b"q\x1b[?2026lk");
        let s = emu.snapshot();
        assert_eq!(s.row_text(0).trim_end(), "\u{250c}\u{2500}\u{2510}");
        assert!(!emu.in_sync_update());
    }

    #[test]
    fn hidden_cursor_is_reported() {
        let emu = emu_with(b"\x1b[?25l");
        assert_eq!(emu.snapshot().cursor(), (0, 0, false));
    }

    #[test]
    fn a_pending_wrap_cursor_stays_inside_the_grid_and_round_trips() {
        let mut emu = Vt100Emulator::new(3, 10, 0, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, b"0123456789");
        let screen = emu.snapshot();
        assert_eq!(screen.cursor(), (0, 9, true));
        let saved = screen.to_string();
        assert_eq!(
            Screen::parse(&saved)
                .expect("saved screen round-trips")
                .to_string(),
            saved
        );
    }

    #[test]
    fn mid_sequence_tracks_partial_escape() {
        let mut emu = Vt100Emulator::new(4, 10, 0, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, b"text\x1b[3");
        assert!(emu.mid_sequence());
        feed_all(&mut emu, b"1m");
        assert!(!emu.mid_sequence());
    }

    #[test]
    fn process_stops_at_the_end_of_a_synchronized_update() {
        let mut emu = Vt100Emulator::new(4, 10, 0, crate::graphics::DEFAULT_CAPTURE, false);
        let stream = b"\x1b[?2026hframe1\x1b[?2026lnext";

        let first = emu.process(stream);
        assert!(
            matches!(first.stop, Some(Stop::FrameComplete(_))),
            "stop: {:?}",
            first.stop
        );
        // Everything through the ESU is consumed; "next" is not.
        assert_eq!(
            &stream[..first.consumed],
            b"\x1b[?2026hframe1\x1b[?2026l" as &[u8]
        );
        // The screen at the stop is the complete frame, untouched by "next".
        assert_eq!(emu.snapshot().text(), "frame1\n\n\n");
        assert!(!emu.in_sync_update());

        let rest = emu.process(&stream[first.consumed..]);
        assert!(rest.stop.is_none());
        assert!(emu.snapshot().contains("frame1next"));
    }

    #[test]
    fn in_sync_update_is_true_between_bsu_and_esu() {
        let mut emu = Vt100Emulator::new(4, 10, 0, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, b"\x1b[?2026hpartial");
        assert!(emu.in_sync_update());
        assert!(!emu.mid_sequence()); // the escape itself is finished
        feed_all(&mut emu, b"\x1b[?2026l");
        assert!(!emu.in_sync_update());
    }

    /// urxvt mouse mode is honoured, so DECRQM can answer for it, and the
    /// encodings vt100 does know are answered from the same record: after
    /// `?1006h ?1015h ?1015l` xterm is back on the legacy form, where vt100
    /// alone still said SGR (#640).
    #[test]
    fn the_urxvt_mouse_encoding_is_tracked_and_reported() {
        let emu = emu_with(b"\x1b[?1000h\x1b[?1015h");
        assert_eq!(emu.input_modes().mouse_encoding, MouseEncoding::Urxvt);
        assert_eq!(emu.mode_state(1015), ModeState::Set);
        assert_eq!(emu.mode_state(1006), ModeState::Reset);

        let emu = emu_with(b"\x1b[?1006h\x1b[?1015h\x1b[?1015l");
        assert_eq!(emu.input_modes().mouse_encoding, MouseEncoding::Legacy);
        assert_eq!(emu.mode_state(1015), ModeState::Reset);
        assert_eq!(emu.mode_state(1006), ModeState::Reset);
    }

    /// After RIS, DECRQM, the input path and the frame clock all agree that
    /// focus reporting is off and no update is open (#623).
    #[test]
    fn a_hard_reset_reports_focus_and_synchronized_output_reset() {
        let mut emu = emu_with(b"\x1b[?1004h\x1b[?2026hhalf");
        assert_eq!(emu.mode_state(1004), ModeState::Set);
        assert_eq!(emu.mode_state(2026), ModeState::Set);
        feed_all(&mut emu, b"\x1bcREADY");
        assert_eq!(emu.mode_state(1004), ModeState::Reset);
        assert_eq!(emu.mode_state(2026), ModeState::Reset);
        assert!(!emu.input_modes().focus_events);
        assert!(!emu.in_sync_update());
        assert!(
            emu.frame_started.is_none(),
            "the half-drawn frame is dropped"
        );
        assert_eq!(emu.snapshot().row_text(0).trim_end(), "READY");
    }

    /// The grid and the tracker stay in step across a control inside an
    /// escape (#627). The CR runs and the escape goes on, so the first `q`
    /// ends it and the rest draw in the designated line art; before, the
    /// tracker staged them as UTF-8 that vte swallowed inside the escape,
    /// and the screen came out blank. And the query after a BEL there is
    /// one the grid's parser runs, so it must be one the responder sees.
    #[test]
    fn a_control_inside_an_escape_keeps_the_grid_and_the_tracker_in_step() {
        let emu = emu_with(b"\x1b(0\x1b\rqqqq\r\nxx");
        assert_eq!(
            emu.snapshot().text(),
            "\u{2500}\u{2500}\u{2500}\n\u{2502}\u{2502}\n\n"
        );

        let mut emu = Vt100Emulator::new(4, 10, 0, crate::graphics::DEFAULT_CAPTURE, false);
        let processed = emu.process(b"\x1b\x07[6nafter");
        assert_eq!(
            processed.stop,
            Some(Stop::Query(super::super::Query::CursorPosition {
                private: false
            }))
        );
        feed_all(&mut emu, &b"\x1b\x07[6nafter"[processed.consumed..]);
        let s = emu.snapshot();
        assert_eq!(s.bells(), 1);
        assert_eq!(s.row_text(0).trim_end(), "after");
    }

    #[test]
    fn snapshot_carries_out_of_band_terminal_state() {
        let emu = emu_with(b"\x1b]0;my app\x07\x1b[?1049h\x1b[?2004h\x1b[?1h\x1b[?1002h");
        let s = emu.snapshot();
        assert_eq!(s.title(), "my app");
        assert!(s.alternate_screen());
        assert!(s.bracketed_paste());
        assert!(s.application_cursor());
        assert_eq!(s.mouse_mode(), MouseMode::ButtonMotion);
    }

    #[test]
    fn snapshot_state_defaults_until_the_app_sets_it() {
        let emu = emu_with(b"plain");
        let s = emu.snapshot();
        assert_eq!(s.title(), "");
        assert!(!s.alternate_screen());
        assert!(!s.bracketed_paste());
        assert!(!s.application_cursor());
        assert_eq!(s.mouse_mode(), MouseMode::None);
    }

    /// That the one unsupported shape is `expected`, without printing 64 KiB
    /// of it when it is not.
    fn assert_one_shape(s: &Screen, expected: &str, at: &str) {
        let shapes: Vec<&str> = s.unsupported().iter().collect();
        let lengths: Vec<usize> = shapes.iter().map(|shape| shape.len()).collect();
        assert_eq!(lengths, [expected.len()], "{at}: shape lengths");
        assert!(shapes[0] == expected, "{at}: the shape differs");
    }

    /// `stream` fed in pieces cut at each of `cuts`, as reads would split it.
    fn emu_fed_in_pieces(stream: &[u8], cuts: &[usize]) -> Vt100Emulator {
        let mut emu = Vt100Emulator::new(4, 10, 0, crate::graphics::DEFAULT_CAPTURE, false);
        let mut from = 0;
        for &cut in cuts.iter().chain([&stream.len()]) {
            feed_all(&mut emu, &stream[from..cut]);
            from = cut;
        }
        emu
    }

    #[test]
    fn an_osc_body_past_the_capture_bound_never_reaches_the_parsers() {
        // vte keeps an OSC string whole until its terminator, in each of
        // the two parsers, so an unterminated one grew the process without
        // bound (#694). The tracker keeps 64 KiB of it; the parsers now get
        // exactly that much, which vt100 shows in the shape it reports.
        let cap = super::super::seq::OSC_CAPTURE_MAX;
        let kept = "a".repeat(cap - "777;".len());
        for terminator in [&b"\x07"[..], b"\x1b\\"] {
            let mut stream = b"\x1b]777;".to_vec();
            stream.resize(stream.len() + cap + 3 * 4096, b'a');
            let st = stream.len();
            stream.extend_from_slice(terminator);
            stream.extend_from_slice(b"DONE");
            // Whole; in reads of 8 KiB, as the reader takes them; at the
            // bound itself, on either side; and between the ESC and the
            // backslash of an ST.
            let at_bound = "\x1b]".len() + cap;
            for cuts in [
                vec![],
                (1..stream.len() / 8192).map(|n| n * 8192).collect(),
                vec![at_bound - 1, at_bound, at_bound + 1],
                vec![st + 1],
            ] {
                let s = emu_fed_in_pieces(&stream, &cuts).snapshot();
                let at = format!("{terminator:?}, cut at {cuts:?}");
                assert_one_shape(&s, &format!("^[]777;{kept}"), &at);
                assert_eq!(s.text(), "DONE\n\n\n", "{at}");
            }
        }
    }

    /// A 64 MiB OSC body, fed as the reader feeds it, in a child of this
    /// test binary whose data segment is capped at 48 MiB: each parser held
    /// the whole body before #694, about 128 MiB together, which the cap
    /// turns into a failed allocation rather than a machine out of memory.
    /// The child does the feeding only when it can see the cap in place.
    #[test]
    #[cfg(target_os = "linux")]
    fn an_unterminated_osc_body_leaves_memory_bounded() {
        const CHILD: &str = "TERMLENS_TEST_OSC_UNDER_CAP";
        const CAP_KIB: u32 = 48 * 1024;
        const NAME: &str = "emu::vt100::tests::an_unterminated_osc_body_leaves_memory_bounded";
        if std::env::var_os(CHILD).is_some() {
            let limits = std::fs::read_to_string("/proc/self/limits").unwrap();
            let data = limits
                .lines()
                .find(|line| line.starts_with("Max data size"))
                .unwrap();
            let soft = data.split_whitespace().nth(3).unwrap();
            assert_eq!(soft, (u64::from(CAP_KIB) * 1024).to_string(), "{data}");
            let mut emu = Vt100Emulator::new(24, 80, 1000, crate::graphics::DEFAULT_CAPTURE, false);
            feed_all(&mut emu, b"\x1b]0;");
            let chunk = [b'a'; 8192];
            for _ in 0..(64 << 20) / chunk.len() {
                feed_all(&mut emu, &chunk);
            }
            feed_all(&mut emu, b"\x07DONE");
            let s = emu.snapshot();
            assert_eq!(s.title().len(), super::super::seq::OSC_CAPTURE_MAX - 2);
            assert_eq!(s.row_text(0).trim_end(), "DONE");
            return;
        }
        let out = std::process::Command::new("sh")
            .args([
                "-c",
                r#"ulimit -c 0 && ulimit -d "$1" && shift && exec "$@""#,
                "sh",
            ])
            .arg(CAP_KIB.to_string())
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{}\n{stdout}\n{stderr}",
            out.status
        );
    }

    #[test]
    fn a_title_past_the_capture_bound_reads_back_capped_and_the_text_after_it_draws() {
        let cap = super::super::seq::OSC_CAPTURE_MAX;
        for terminator in [&b"\x07"[..], b"\x1b\\"] {
            let mut stream = b"\x1b]2;".to_vec();
            stream.resize(stream.len() + 2 * cap, b'T');
            stream.extend_from_slice(terminator);
            stream.extend_from_slice(b"DONE");
            let s = emu_fed_in_pieces(&stream, &[cap, cap + 4]).snapshot();
            assert_eq!(s.title(), "T".repeat(cap - "2;".len()), "{terminator:?}");
            assert_eq!(s.text(), "DONE\n\n\n", "{terminator:?}");
            assert_eq!(s.unsupported(), [] as [&str; 0], "{terminator:?}");
        }
    }

    #[test]
    fn an_osc_at_the_capture_bound_reaches_the_parsers_whole() {
        // Exactly the bound: nothing is withheld, and the shape is the
        // whole string, as it was before #694.
        let cap = super::super::seq::OSC_CAPTURE_MAX;
        let body = "b".repeat(cap - "777;".len());
        let stream = format!("\x1b]777;{body}\x07DONE");
        let s = emu_fed_in_pieces(stream.as_bytes(), &[]).snapshot();
        assert_one_shape(&s, &format!("^[]777;{body}"), "at the bound");
        assert_eq!(s.text(), "DONE\n\n\n");
    }

    /// Feed a stream into an emulator with `scrollback` rows of history.
    fn emu_with_history(scrollback: usize, bytes: &[u8]) -> Vt100Emulator {
        let mut emu =
            Vt100Emulator::new(4, 10, scrollback, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, bytes);
        emu
    }

    #[test]
    fn rows_scrolled_off_the_top_are_retained_in_order() {
        // A 4-row screen fed 7 lines: three scroll off.
        let emu = emu_with_history(100, b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven");
        let s = emu.snapshot();
        assert_eq!(s.scrollback_rows(), 3);
        assert_eq!(s.scrollback_text(), "one\ntwo\nthree");
        assert_eq!(s.text(), "four\nfive\nsix\nseven");
        // The assertion an author actually writes: content reached the
        // terminal, wherever it currently sits.
        assert_eq!(s.full_text(), "one\ntwo\nthree\nfour\nfive\nsix\nseven");
        // The visible-screen queries stay visible-screen queries.
        assert!(!s.contains("one"));
        assert!(s.contains("seven"));
    }

    #[test]
    fn history_is_bounded_and_drops_its_oldest_rows() {
        // Ten rows, each ending in a newline, on a 4-row screen: seven
        // scroll off (row1..row7) and the cap of 3 keeps the newest three.
        let mut emu = Vt100Emulator::new(4, 10, 3, crate::graphics::DEFAULT_CAPTURE, false);
        for n in 1..=10 {
            feed_all(&mut emu, format!("row{n}\r\n").as_bytes());
        }
        let s = emu.snapshot();
        assert_eq!(s.scrollback_rows(), 3);
        assert_eq!(s.scrollback_text(), "row5\nrow6\nrow7");
        assert_eq!(s.text(), "row8\nrow9\nrow10\n");
        // row1..row4 are past the bound and gone — the honest limit.
        assert!(!s.full_text().contains("row4"));
        assert!(s.full_text().contains("row5"));
    }

    #[test]
    fn a_history_kept_at_the_cap_keeps_advancing() {
        // Once at the cap vt100's length stops changing, so a naive "did
        // the length grow?" check would freeze the history at its first
        // full window: the rows it drops off the front are counted instead.
        let mut emu = Vt100Emulator::new(4, 10, 2, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
        assert_eq!(emu.snapshot().scrollback_text(), "b\nc");
        feed_all(&mut emu, b"g\r\nh\r\n");
        assert_eq!(emu.snapshot().scrollback_text(), "d\ne");
    }

    #[test]
    fn a_hard_reset_keeps_history_across_every_feed_boundary() {
        // The same stream split in different places, which is the whole
        // bug: the mark was compared by length alone, so a feed that put
        // the reset and the following rows together could land on the old
        // mark and skip every new row (#391). Rows from before the reset
        // are recoverable only while vt100 still holds them, so the
        // boundaries around the reset byte are the ones that matter.
        let stream =
            b"1|\r\n2|\r\n3|\r\n4|\r\n5|\r\n6|\r\n\x1bcA|\r\nB|\r\nC|\r\nD|\r\nE|\r\nF|\r\n";
        let expected = "1|\n2|\n3|\nA|\nB|\nC|";
        // All in one feed, inside the pre-reset lines, before the reset,
        // between `ESC` and `c`, right after the reset, inside the rows
        // after it, and split after the end.
        for split in [0, 6, 24, 25, 26, 40, stream.len()] {
            let mut emu = Vt100Emulator::new(4, 20, 20, crate::graphics::DEFAULT_CAPTURE, false);
            feed_all(&mut emu, &stream[..split]);
            feed_all(&mut emu, &stream[split..]);
            let screen = emu.snapshot();
            assert_eq!(screen.scrollback_text(), expected, "split at {split}");
            assert_eq!(screen.text(), "D|\nE|\nF|\n", "split at {split}");
        }
    }

    #[test]
    fn retention_off_keeps_nothing() {
        let emu = emu_with_history(0, b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let s = emu.snapshot();
        assert_eq!(s.scrollback_rows(), 0);
        assert_eq!(s.scrollback_text(), "");
        // full_text is then just the visible screen.
        assert_eq!(s.full_text(), s.text());
    }

    #[test]
    fn the_alternate_screen_accumulates_no_history() {
        // A full-screen TUI owns its viewport and should cost nothing for a
        // feature it does not use. vt100 gives the alternate grid zero
        // scrollback of its own, which is what makes retention safe to
        // default on.
        let emu = emu_with_history(
            100,
            b"\x1b[?1049h one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix",
        );
        assert_eq!(emu.snapshot().scrollback_rows(), 0);
    }

    #[test]
    fn leaving_the_alternate_screen_copies_no_history_twice() {
        // The alternate grid has no history, and its length of zero read as
        // the main screen's history emptied: the mark restarted, and every
        // return copied the whole history again (#576). Separate feeds, as
        // a shell, a full-screen program and the shell again write them.
        let mut emu = Vt100Emulator::new(4, 20, 100, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, b"line1\r\nline2\r\nline3\r\nline4\r\nline5\r\n");
        for _ in 0..2 {
            feed_all(&mut emu, b"\x1b[?1049halt");
            // The history stays on record while the program runs, too.
            assert_eq!(emu.snapshot().scrollback_text(), "line1\nline2");
            feed_all(&mut emu, b"\x1b[?1049l");
        }
        let s = emu.snapshot();
        assert_eq!(s.scrollback_text(), "line1\nline2");
        assert_eq!(s.text(), "line3\nline4\nline5\n");
    }

    #[test]
    fn rows_that_scroll_in_the_read_that_enters_the_alternate_screen_are_kept() {
        // Lines that scroll off and the switch to the alternate screen in
        // one write, as a full-screen program that starts right after
        // printing writes them. The capture ran once, after the whole feed,
        // with the alternate screen up, so it copied nothing — and a reset
        // before the main screen came back lost the rows for good (#622).
        for mode in ["47", "1047", "1049"] {
            for after in ["", "\x1bc"] {
                let stream = format!("l1\r\nl2\r\nl3\r\nl4\r\nl5\x1b[?{mode}hTUI{after}");
                let (whole, bytewise) = whole_and_bytewise(3, 10, 100, stream.as_bytes());
                let label = format!("?{mode}h then {after:?}");
                assert_eq!(whole.snapshot().scrollback_text(), "l1\nl2", "{label}");
                assert_eq!(observed(&whole), observed(&bytewise), "{label}");
            }
        }
    }

    #[test]
    fn a_taller_screen_takes_its_rows_back_from_history() {
        // vt100 0.18 resizes as xterm does: a shorter screen with the cursor
        // on its last row pushes rows into history, and a taller one pulls
        // them back. A row back on the grid leaves the history, or it would
        // be on record twice.
        let mut emu = Vt100Emulator::new(5, 10, 100, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, b"1\r\n2\r\n3\r\n4\r\nend");
        emu.set_size(3, 10);
        let s = emu.snapshot();
        assert_eq!(s.scrollback_text(), "1\n2");
        assert_eq!(s.text(), "3\n4\nend");
        emu.set_size(5, 10);
        let s = emu.snapshot();
        assert_eq!(s.scrollback_text(), "");
        assert_eq!(s.text(), "1\n2\n3\n4\nend");
        // Scrolling afterwards copies each row once.
        feed_all(&mut emu, b"\r\nf\r\ng");
        assert_eq!(emu.snapshot().full_text(), "1\n2\n3\n4\nend\nf\ng");
    }

    #[test]
    fn rows_back_from_history_keep_the_attributes_the_shadow_carries() {
        // Blink lives in the attribute shadow, a second parser. Its grid
        // must get the same rows back from history as the primary's, or a
        // snapshot after the resize trips the shadow's lockstep assertion
        // and reads attributes off the wrong rows. Styled history off, the
        // default: the shadow used to keep no history then.
        for styled in [false, true] {
            let mut emu = Vt100Emulator::new(5, 10, 100, crate::graphics::DEFAULT_CAPTURE, styled);
            feed_all(&mut emu, b"\x1b[5m1\x1b[0m\r\n2\r\n3\r\n4\r\nend");
            emu.set_size(3, 10);
            emu.set_size(5, 10);
            let s = emu.snapshot();
            assert_eq!(s.text(), "1\n2\n3\n4\nend", "styled history {styled}");
            assert!(
                s.cell(0, 0).is_some_and(|c| c.style().blink),
                "styled history {styled}"
            );
            assert!(
                s.cell(1, 0).is_some_and(|c| !c.style().blink),
                "styled history {styled}"
            );
        }
    }

    #[test]
    fn a_resize_round_trip_keeps_a_soft_wrap_and_the_shadow_in_step() {
        // Wider and shorter, then back: the rows the first resize pushes
        // into history are narrower than the screen, and reading them made
        // vt100 widen them and clear their wrap flags — in the primary
        // only, without styled history, so the second resize pulled
        // different rows back into the two grids and the lockstep
        // assertion fired; and with both moved, the soft wrap was gone
        // (#620).
        //
        // A history at its retention bound used to be read again whole by
        // every capture, the one the next output runs included, and that
        // read widened the narrow row too (#671): the last cases redraw a
        // row between the two resizes, which scrolls nothing.
        let at_bound = "x1\r\nx2\r\nx3\r\nx4\r\n";
        for (styled, scrollback, before, between) in [
            (false, 100, "", ""),
            (true, 100, "", ""),
            (false, 3, at_bound, ""),
            (false, 3, at_bound, "\rREADY"),
            (true, 3, at_bound, "\rREADY"),
        ] {
            let label = format!("styled history {styled}, {scrollback} rows, then {between:?}");
            let mut emu =
                Vt100Emulator::new(6, 10, scrollback, crate::graphics::DEFAULT_CAPTURE, styled);
            feed_all(
                &mut emu,
                format!("{before}l1\r\nl2\r\nAAAAAAAAAABBBBB\r\nl4\r\nREADY").as_bytes(),
            );
            assert!(emu.snapshot().row_wrapped(2), "{label}");
            emu.set_size(3, 20);
            let s = emu.snapshot();
            assert_eq!(s.scrollback_text(), "l1\nl2\nAAAAAAAAAA", "{label}");
            assert_eq!(s.text(), "BBBBB\nl4\nREADY", "{label}");
            feed_all(&mut emu, between.as_bytes());
            emu.set_size(6, 10);
            let s = emu.snapshot();
            assert_eq!(s.text(), "l1\nl2\nAAAAAAAAAA\nBBBBB\nl4\nREADY", "{label}");
            assert!(s.row_wrapped(2), "{label}: {s}");
            assert_eq!(
                s.logical_text(),
                "l1\nl2\nAAAAAAAAAABBBBB\nl4\nREADY",
                "{label}"
            );
        }
    }

    #[test]
    fn a_history_at_its_bound_copies_only_the_rows_that_scrolled() {
        // At the bound vt100 drops a row off the front for every row it
        // takes at the back, so its length says nothing; the rows it drops
        // are counted instead, and only the new ones are read (#671). The
        // narrow row a resize pushed is never read again, so it keeps its
        // soft wrap when a taller screen pulls it back — while rows keep
        // scrolling at the bound in between.
        for styled in [false, true] {
            let mut emu = Vt100Emulator::new(6, 10, 3, crate::graphics::DEFAULT_CAPTURE, styled);
            feed_all(
                &mut emu,
                b"x1\r\nx2\r\nx3\r\nx4\r\nl1\r\nl2\r\nAAAAAAAAAABBBBB\r\nl4\r\nREADY",
            );
            emu.set_size(3, 20);
            assert_eq!(emu.snapshot().scrollback_text(), "l1\nl2\nAAAAAAAAAA");
            // One row scrolls off at the bound, and `l1` is dropped for it.
            feed_all(&mut emu, b"\r\nnext");
            let s = emu.snapshot();
            assert_eq!(
                s.scrollback_text(),
                "l2\nAAAAAAAAAA\nBBBBB",
                "styled history {styled}"
            );
            assert_eq!(s.text(), "l4\nREADY\nnext", "styled history {styled}");
            emu.set_size(6, 10);
            let s = emu.snapshot();
            assert_eq!(s.scrollback_text(), "", "styled history {styled}");
            assert_eq!(
                s.text(),
                "l2\nAAAAAAAAAA\nBBBBB\nl4\nREADY\nnext",
                "styled history {styled}"
            );
            assert!(s.row_wrapped(1), "styled history {styled}: {s}");
            assert_eq!(
                s.logical_text(),
                "l2\nAAAAAAAAAABBBBB\nl4\nREADY\nnext",
                "styled history {styled}"
            );
        }
    }

    #[test]
    fn a_history_at_its_bound_keeps_rows_wider_than_a_narrowed_screen() {
        // Read again whole at the bound, the history was read at the width
        // the screen had then: after a narrower resize, the next output cut
        // every retained row to it. A row is read once now, as it scrolled
        // off (#671).
        let mut emu = Vt100Emulator::new(4, 10, 2, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, b"0123456789\r\nabcdefghij\r\nx\r\ny\r\nz\r\nw");
        assert_eq!(emu.snapshot().scrollback_text(), "0123456789\nabcdefghij");
        emu.set_size(4, 5);
        feed_all(&mut emu, b"\r\nv");
        assert_eq!(emu.snapshot().scrollback_text(), "abcdefghij\nx");
    }

    #[test]
    fn a_shorter_wider_resize_leaves_the_cursor_where_vt100_puts_it() {
        // The rows a shorter screen pushes are copied off the grid before
        // the resize, so the resize itself is vt100's, in one step. Made in
        // two — the height, then the width — it clamped a cursor pending
        // wrap to the old last column, and the next character overwrote the
        // one before it.
        let stream = b"1\r\n2\r\n3\r\nabcde";
        let mut emu = Vt100Emulator::new(4, 5, 100, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, stream);
        emu.set_size(3, 8);
        feed_all(&mut emu, b"X");
        let mut raw = ::vt100::Parser::new(nz(4), nz(5), 100);
        raw.process(stream);
        raw.set_size(nz(3), nz(8));
        raw.process(b"X");
        let s = emu.snapshot();
        assert_eq!(s.text(), "2\n3\nabcdeX");
        assert_eq!(s.text(), raw.screen().contents());
        assert_eq!(
            emu.parser.screen().cursor_position(),
            raw.screen().cursor_position()
        );
        assert_eq!(s.scrollback_text(), "1");
    }

    #[test]
    fn a_resize_while_the_alternate_screen_is_up_keeps_the_rows_still_in_history() {
        // At the bound, a shorter main screen pushed `g1` and dropped `a`,
        // and a taller one pulled `g1` back, all while a full-screen program
        // had the alternate screen up. The history came back one shorter,
        // and the copy took that as the newest row pulled back: it dropped
        // `c`, which vt100 still held, and kept `a`. Counting what vt100
        // drops from the front tells the two ends apart (#671).
        let mut emu = Vt100Emulator::new(4, 10, 3, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, b"a\r\nb\r\nc\r\ng1\r\ng2\r\ng3\r\ng4");
        assert_eq!(emu.snapshot().scrollback_text(), "a\nb\nc");
        feed_all(&mut emu, b"\x1b[?1049hTUI");
        emu.set_size(3, 10);
        emu.set_size(4, 10);
        feed_all(&mut emu, b"\x1b[?1049l");
        let s = emu.snapshot();
        // `a` left vt100's history while out of sight, and the copy keeps
        // it, as it keeps rows across a reset: nothing is on record twice
        // and nothing still in the terminal is missing.
        assert_eq!(s.scrollback_text(), "a\nb\nc");
        assert_eq!(s.full_text(), "a\nb\nc\ng1\ng2\ng3\ng4");
        // And the copy carries on from there.
        feed_all(&mut emu, b"\r\ng5");
        assert_eq!(emu.snapshot().scrollback_text(), "b\nc\ng1");
    }

    #[test]
    fn a_history_at_its_bound_is_the_terminal_s_own_through_any_scroll_or_resize() {
        // The copy against vt100's own history, read the slow way, after
        // every step of a seeded sweep that keeps the history at a small
        // bound: line feeds, wraps, scroll-ups, scroll regions (which keep
        // no history), resizes both ways, the alternate screen, RIS and
        // ED 3. A miscount of the rows vt100 dropped would put the copy out
        // of step (#671), and the snapshot's lockstep assertion checks the
        // shadow on the way.
        //
        // The copy outlives vt100's history by design across RIS and ED 3,
        // so from the first of those on vt100's history must be the copy's
        // newest rows rather than all of it. A resize while the alternate
        // screen hides the main one is settled only when it is back, from
        // the lengths and the count alone, so a row it pulled back and
        // pushed again at another width keeps the text it was copied with;
        // after one, only the bound is checked, until a reset starts over.
        const PIECES: &[&str] = &[
            "a",
            "bc",
            "\r\n",
            "\n",
            "AAAAAAAAAAAAAAAAAAAAAAA",
            "\x1b[S",
            "\x1b[3S",
            "\x1b[2;3r",
            "\x1b[r",
            "\x1b[H",
            "\x1b[99;1H",
            "\x1b[K",
            // The rest only in the second half of the sweep, so the first
            // half holds the copy to all of vt100's history throughout.
            "\x1b[?1049h",
            "\x1b[?1049l",
            "\x1bc",
            "\x1b[3J",
        ];
        let plain = PIECES.iter().position(|&p| p == "\x1b[?1049h").unwrap();
        // vt100's history as text, each row whole: a row keeps the width it
        // was pushed at, and the copy keeps the text it read then, so the
        // screen's width now is the wrong one to read it at.
        fn reference(parser: &mut ::vt100::Parser) -> Vec<String> {
            let screen = parser.screen_mut();
            let (rows, _) = size(screen);
            let len = screen.scrollback_len();
            let mut lines = Vec::new();
            let mut i = 0;
            while i < len {
                screen.set_scrollback(len - i);
                let take = (len - i).min(usize::from(rows));
                lines.extend(
                    screen
                        .rows(0, u16::MAX)
                        .take(take)
                        .map(|l| l.trim_end().to_owned()),
                );
                i += take;
            }
            screen.set_scrollback(0);
            lines
        }
        let mut seed: u64 = 0x0671_5eed;
        let mut next = move |n: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(seed >> 33).unwrap() % n
        };
        for case in 0..600 {
            let styled = case % 2 == 1;
            let pieces = if case < 300 { &PIECES[..plain] } else { PIECES };
            let (mut rows, mut cols) = (4, 8);
            let mut emu =
                Vt100Emulator::new(rows, cols, 5, crate::graphics::DEFAULT_CAPTURE, styled);
            let mut raw = ::vt100::Parser::new(nz(rows), nz(cols), 5);
            // `Some(true)`: the copy is vt100's history; `Some(false)`: it
            // ends with it; `None`: a resize was settled out of sight.
            let mut exact = Some(true);
            for step in 0..40 {
                let alternate = raw.screen().alternate_screen();
                if next(5) == 0 {
                    rows = 2 + u16::try_from(next(5)).unwrap();
                    cols = 3 + u16::try_from(next(10)).unwrap();
                    emu.set_size(rows, cols);
                    raw.set_size(nz(rows), nz(cols));
                    if alternate {
                        exact = None;
                    }
                } else {
                    let piece = pieces[next(pieces.len())];
                    feed_all(&mut emu, piece.as_bytes());
                    raw.process(piece.as_bytes());
                    if piece == "\x1bc" || (piece == "\x1b[3J" && !alternate) {
                        exact = Some(false);
                    }
                }
                let s = emu.snapshot();
                let ours = s.scrollback_text();
                let ours: Vec<&str> = if s.scrollback_rows() == 0 {
                    Vec::new()
                } else {
                    ours.split('\n').collect()
                };
                let theirs = reference(&mut raw);
                let theirs: Vec<&str> = theirs.iter().map(String::as_str).collect();
                let at = format!("case {case}, step {step}: {ours:?} against {theirs:?}");
                assert!(ours.len() <= 5, "{at}");
                // vt100's accessors read the grid on display, and the
                // alternate one has no history to compare with.
                match exact {
                    _ if raw.screen().alternate_screen() => {}
                    Some(true) => assert_eq!(ours, theirs, "{at}"),
                    Some(false) => assert!(ours.ends_with(&theirs), "{at}"),
                    None => {}
                }
                let grid: Vec<String> = raw
                    .screen()
                    .rows(0, cols)
                    .map(|l| l.trim_end().to_owned())
                    .collect();
                assert_eq!(s.text(), grid.join("\n"), "{at}");
            }
        }
    }

    #[test]
    fn a_resize_while_the_alternate_screen_is_up_is_settled_on_return() {
        // The resize reaches the main screen's grid too, out of sight: rows
        // come back out of its history while the alternate screen is shown,
        // and the copy catches up when the main screen is read again.
        let mut emu = Vt100Emulator::new(5, 10, 100, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, b"1\r\n2\r\n3\r\n4\r\nend");
        emu.set_size(3, 10);
        assert_eq!(emu.snapshot().scrollback_text(), "1\n2");
        feed_all(&mut emu, b"\x1b[?1049hfull screen");
        emu.set_size(5, 10);
        feed_all(&mut emu, b"\x1b[?1049l");
        let s = emu.snapshot();
        assert_eq!(s.scrollback_text(), "");
        assert_eq!(s.full_text(), "1\n2\n3\n4\nend");
    }

    #[test]
    fn leaving_the_alternate_screen_settles_a_taller_resize_before_new_rows() {
        // A taller resize while the alternate screen is up pulls `L1` back
        // out of the main screen's history. Leaving it and scrolling `NEW1`
        // off in one feed left the history its old length: the pop and the
        // push cancelled, so the copy kept `L1`, a row now on the grid and
        // then erased, and never took `NEW1` — unless the read happened to
        // split right after the exit (#682). Every split, every spelling.
        for (enter, exit) in [
            ("\x1b[?1049h", "\x1b[?1049l"),
            ("\x1b[?1047h", "\x1b[?1047l"),
            ("\x1b[?47h", "\x1b[?47l"),
            ("\x1b[?1049h", "\x1b[?25;1049l"),
        ] {
            let tail = format!("{exit}\x1b[2J\x1b[HNEW1\r\nNEW2\r\nNEW3\r\nNEW4\r\nNEW5");
            for styled in [false, true] {
                for split in 0..=tail.len() {
                    let mut emu =
                        Vt100Emulator::new(3, 10, 100, crate::graphics::DEFAULT_CAPTURE, styled);
                    feed_all(&mut emu, format!("L1\r\nL2\r\nL3\r\nL4{enter}").as_bytes());
                    emu.set_size(4, 10);
                    feed_all(&mut emu, &tail.as_bytes()[..split]);
                    feed_all(&mut emu, &tail.as_bytes()[split..]);
                    let s = emu.snapshot();
                    let at = format!("{exit:?}, styled {styled}, split at {split}");
                    assert_eq!(s.scrollback_text(), "NEW1", "{at}");
                    assert_eq!(s.text(), "NEW2\nNEW3\nNEW4\nNEW5", "{at}");
                }
            }
        }
    }

    #[test]
    fn erasing_the_saved_lines_restarts_the_mark_and_keeps_the_copy() {
        // ED 3 (`CSI 3 J`, which `clear` sends) empties vt100's history. More
        // rows scrolling in the same write than the mark held must all be
        // copied: read by length alone, the history looked like it had
        // grown past the mark, and the first rows after the erase were
        // skipped. The rows copied before the erase are kept, as for RIS.
        let stream = b"a\r\nb\r\nc\r\nd\r\ne\r\n\x1b[3J1\r\n2\r\n3\r\n4\r\n5\r\n6";
        let erase = stream.iter().position(|&b| b == 0x1b).unwrap();
        // One feed, split before the erase, inside it, and after it.
        for split in [stream.len(), erase, erase + 2, erase + 4, erase + 6] {
            let mut emu = Vt100Emulator::new(4, 10, 100, crate::graphics::DEFAULT_CAPTURE, false);
            feed_all(&mut emu, &stream[..split]);
            feed_all(&mut emu, &stream[split..]);
            let s = emu.snapshot();
            assert_eq!(
                s.scrollback_text(),
                "a\nb\nc\nd\ne\n1\n2",
                "split at {split}"
            );
            assert_eq!(s.text(), "3\n4\n5\n6", "split at {split}");
        }
    }

    /// DECSED 3 and the sub-parameter form erase vt100's history exactly as
    /// `CSI 3 J` does, and keep termlens's copy exactly as it does: before
    /// #621 the tracker missed both, read the emptied history as rows pulled
    /// back onto the screen, and popped the copy.
    #[test]
    fn every_spelling_of_erase_saved_lines_keeps_the_copy() {
        for erase in [&b"\x1b[3J"[..], b"\x1b[?3J", b"\x1b[3:1J"] {
            let mut emu = Vt100Emulator::new(3, 10, 100, crate::graphics::DEFAULT_CAPTURE, false);
            feed_all(&mut emu, b"l1\r\nl2\r\nl3\r\nl4\r\nl5");
            feed_all(&mut emu, erase);
            let s = emu.snapshot();
            let erase = String::from_utf8_lossy(erase);
            assert_eq!(s.scrollback_text(), "l1\nl2", "{erase:?}");
            assert_eq!(s.text(), "l3\nl4\nl5", "{erase:?}");
            // And the mark starts over: what scrolls next is copied once.
            feed_all(&mut emu, b"\r\nl6");
            assert_eq!(emu.snapshot().scrollback_text(), "l1\nl2\nl3", "{erase:?}");
        }
    }

    #[test]
    fn erasing_saved_lines_on_the_alternate_screen_leaves_the_main_history() {
        // ED 3 empties the history of the grid on display, and the alternate
        // grid has none: the main screen's history and its mark are untouched.
        let mut emu = Vt100Emulator::new(4, 10, 100, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, b"a\r\nb\r\nc\r\nd\r\ne\r\n");
        feed_all(&mut emu, b"\x1b[?1049h\x1b[3Jtui\x1b[?1049l");
        let s = emu.snapshot();
        assert_eq!(s.scrollback_text(), "a\nb");
        assert_eq!(s.text(), "c\nd\ne\n");
    }

    #[test]
    fn narrowing_through_a_wide_character_leaves_a_row_that_can_be_erased() {
        // vt100 0.16 kept the cut character marked wide after a resize took
        // its second column, and the next write that cleared the row read
        // past its end and panicked the emulator (#577). Each eraser is
        // a separate way of reaching that cell.
        for eraser in [
            &b"\r\x1b[K"[..],
            b"\x1b[J",
            b"\x1b[1;3H\x1b[P",
            b"\x1b[1;3H\x1b[X",
            b"\x1b[1;3Hx",
        ] {
            let mut emu = Vt100Emulator::new(3, 4, 0, crate::graphics::DEFAULT_CAPTURE, false);
            feed_all(&mut emu, "ab\u{6f22}".as_bytes());
            emu.set_size(3, 3);
            feed_all(&mut emu, eraser);
            let s = emu.snapshot();
            assert_eq!(s.cols(), 3, "{eraser:?}");
            assert!(
                !s.row_text(0).contains('\u{6f22}'),
                "{eraser:?}: {:?}",
                s.row_text(0)
            );
        }
    }

    #[test]
    fn rep_repeats_the_last_character_drawn() {
        // REP (`CSI n b`), which the xterm-256color terminfo advertises and
        // ncurses uses for runs outside a UTF-8 locale (#578): vt100 0.17
        // implements it. The character repeated is the one drawn, so a
        // DEC Special Graphics line repeats as the line.
        let emu = emu_with(b"x\x1b[5bEND");
        let s = emu.snapshot();
        assert_eq!(s.row_text(0), "xxxxxxEND ");
        assert_eq!(s.unsupported(), [] as [&str; 0]);
        let emu = emu_with(b"\x1b(0q\x1b[3b\x1b(B|");
        assert_eq!(
            emu.snapshot().row_text(0),
            "\u{2500}\u{2500}\u{2500}\u{2500}|     "
        );
        let emu = emu_with("\u{6f22}\x1b[2b".as_bytes());
        assert_eq!(emu.snapshot().row_text(0), "\u{6f22}\u{6f22}\u{6f22}    ");
    }

    #[test]
    fn the_width_model_is_the_grid_s_own_layout() {
        // `width::columns` predicts how far the grid moves the cursor for a
        // run of text; insert mode, `locate` and the diff all rely on that
        // prediction. Held against vt100 itself, one sample at a time.
        for sample in [
            "abc",
            "\u{6f22}\u{5b57}x",
            "e\u{301}x",
            "\u{644}\u{627}",
            "\u{1f468}\u{200d}\u{1f469}x",
            "\u{26a0}\u{fe0f}x",
            "\u{2714}\u{fe0f}\u{31}\u{fe0f}\u{20e3}x",
            "\u{1f44d}\u{1f3fd}x",
            "a\u{fe0f}b",
            "\u{1f1fa}\u{1f1f8}x",
        ] {
            let emu = emu_with_size(2, 30, sample.as_bytes());
            let (_, col, _) = emu.snapshot().cursor();
            assert_eq!(
                usize::from(col),
                crate::width::columns(sample),
                "{sample:?}"
            );
        }
    }

    #[test]
    fn an_underline_colour_is_one_parameter_not_three_attributes() {
        // vt100 does not model SGR 58 and read `58;5;n` a parameter at a
        // time: `5` blink, then `n` as an attribute — `0` reset the cell, `1`
        // made it bold (#579). crossterm writes exactly this form.
        let emu = emu_with(b"\x1b[1;31mA\x1b[58;5;0mB\x1b[0m \x1b[58;5;1mC\x1b[0m");
        let s = emu.snapshot();
        let style = |col| *s.cell(0, col).unwrap().style();
        for col in [0, 1] {
            assert_eq!(style(col).fg, Color::Indexed(1), "column {col}");
            assert!(style(col).bold, "column {col}");
            assert!(!style(col).blink, "column {col}");
        }
        assert_eq!(style(3), Style::default(), "C asked only for a colour");
        // The truecolour form's arguments are 0, 1 and 7: a reset, bold and
        // reverse if read as attributes.
        let emu = emu_with(b"\x1b[4mx\x1b[58;2;0;1;7my");
        let y = *emu.snapshot().cell(0, 1).unwrap().style();
        assert!(y.underline && !y.bold && !y.reverse && !y.dim, "{y:?}");
        // Still a sequence nobody honoured, so it is still on record.
        let shapes: Vec<String> = emu
            .snapshot()
            .unsupported()
            .iter()
            .map(String::from)
            .collect();
        assert_eq!(shapes, ["^[[58:2:0:1:7m"]);
    }

    #[test]
    fn insert_mode_reserves_the_columns_the_grid_gives_the_run() {
        // The reservation was the run's string width, and the grid lays a
        // ZWJ family out as two wide cells and lam-alef as two letters:
        // the difference was overwritten instead of pushed right (#580).
        for (run, expected) in [
            (
                "\u{1f468}\u{200d}\u{1f469}",
                "\u{1f468}\u{200d}\u{1f469}ABCDEF",
            ),
            ("\u{644}\u{627}", "\u{644}\u{627}ABCDEFGH"),
            ("\u{26a0}\u{fe0f}", "\u{26a0}\u{fe0f}ABCDEFGH"),
        ] {
            let stream = format!("ABCDEFGHIJ\r\x1b[4h{run}");
            let emu = emu_with_size(2, 10, stream.as_bytes());
            assert_eq!(emu.snapshot().row_text(0), expected, "{run:?}");
        }
    }

    #[test]
    fn locate_reports_the_column_the_grid_gave_a_history_row() {
        // History is text; `locate` turns the text before a match into a
        // column, and a column `scrollback_cell` can be asked about must be
        // the grid's (#580).
        let mut emu = Vt100Emulator::new(2, 20, 100, crate::graphics::DEFAULT_CAPTURE, true);
        feed_all(
            &mut emu,
            "\u{1f468}\u{200d}\u{1f469} SECRET\r\nb\r\nc\r\n".as_bytes(),
        );
        let s = emu.snapshot();
        assert_eq!(
            s.locate("SECRET"),
            Some(crate::screen::Location::History { row: 0, col: 5 })
        );
        assert_eq!(s.scrollback_cell(0, 5).map(Cell::contents), Some("S"));
    }

    #[test]
    fn a_selector_widened_emoji_survives_the_text_format() {
        // The grid gives `⚠️` two columns; the saved text reads back with
        // the same cells, so `find` agrees before and after (#580).
        let emu = emu_with_size(2, 10, "\u{26a0}\u{fe0f}X \u{2714}\u{fe0f}Y".as_bytes());
        let live = emu.snapshot();
        let back = Screen::parse(&live.to_string()).unwrap();
        for col in 0..10 {
            assert_eq!(back.cell(0, col), live.cell(0, col), "column {col}");
        }
        assert_eq!(live.find("Y"), Some((0, 6)));
        assert_eq!(back.find("Y"), Some((0, 6)));
    }

    /// An emulator simulating `support`, cells 10x20 pixels, fed `bytes`.
    fn graphics_emu(support: Graphics, rows: u16, cols: u16, bytes: &[u8]) -> Vt100Emulator {
        let mut emu = Vt100Emulator::new(rows, cols, 100, crate::graphics::DEFAULT_CAPTURE, false);
        emu.declare_graphics(support, Some((10, 20)));
        feed_all(&mut emu, bytes);
        emu
    }

    /// Where `X` landed.
    fn x_at(emu: &Vt100Emulator) -> Option<(u16, u16)> {
        emu.snapshot().find("X")
    }

    #[test]
    fn a_kitty_placement_moves_the_cursor_to_its_last_row_one_column_past_it() {
        // kitty moves the cursor right by the columns and down by the rows
        // less one (#588); `at` stays the top-left corner.
        let placed = b"\x1b_Ga=T,f=24,s=1,v=1,c=3,r=2;ESIz\x1b\\X";
        let emu = graphics_emu(Graphics::Kitty, 6, 10, placed);
        assert_eq!(x_at(&emu), Some((1, 3)));
        assert_eq!(emu.snapshot().graphics().payloads()[0].at(), (0, 0));
        // A terminal without the protocol ignores the escape: nothing moves.
        for support in [Graphics::None, Graphics::Sixel] {
            assert_eq!(
                x_at(&graphics_emu(support, 6, 10, placed)),
                Some((0, 0)),
                "{support:?}"
            );
        }
        // Asked not to (`C=1`), or only transmitted: no move either. A
        // transmission naming no action is `a=t`.
        for escape in [
            &b"\x1b_Ga=T,f=24,s=1,v=1,c=3,r=2,C=1;ESIz\x1b\\X"[..],
            b"\x1b_Gf=24,s=1,v=1,c=3,r=2;ESIz\x1b\\X",
        ] {
            assert_eq!(
                x_at(&graphics_emu(Graphics::Kitty, 6, 10, escape)),
                Some((0, 0))
            );
        }
    }

    #[test]
    fn a_kitty_placement_wraps_and_scrolls_as_kitty_does() {
        // Past the right edge: the start of the next row.
        let emu = graphics_emu(
            Graphics::Kitty,
            6,
            10,
            b"\x1b[1;9H\x1b_Ga=T,f=24,s=1,v=1,c=3,r=1;ESIz\x1b\\X",
        );
        assert_eq!(x_at(&emu), Some((1, 0)));
        // Below the bottom margin: the screen scrolls, the rows pushed out
        // go to history, and the cursor ends on the last row.
        let emu = graphics_emu(
            Graphics::Kitty,
            4,
            10,
            b"top\r\n\r\n\r\n\x1b_Ga=T,f=24,s=1,v=1,c=2,r=3;ESIz\x1b\\X",
        );
        let screen = emu.snapshot();
        assert_eq!(screen.find("X"), Some((3, 2)));
        assert_eq!(screen.scrollback_text(), "top\n");
    }

    #[test]
    fn a_kitty_placement_without_cells_is_sized_from_pixels_and_the_cell_size() {
        // A 20x40 image in 10x20 cells: 2x2. `a=p` of it, by id, later.
        let emu = graphics_emu(
            Graphics::Kitty,
            6,
            10,
            b"\x1b_Ga=t,f=24,s=20,v=40,i=7;ESIz\x1b\\\x1b_Ga=p,i=7\x1b\\X",
        );
        assert_eq!(x_at(&emu), Some((1, 2)));
        // Without a cell size there is nothing to size it by: no move.
        let mut emu = Vt100Emulator::new(6, 10, 0, crate::graphics::DEFAULT_CAPTURE, false);
        emu.declare_graphics(Graphics::Kitty, None);
        feed_all(&mut emu, b"\x1b_Ga=T,f=24,s=20,v=40;ESIz\x1b\\X");
        assert_eq!(x_at(&emu), Some((0, 0)));
    }

    #[test]
    fn a_kitty_placement_takes_its_size_from_the_newest_transmission_of_the_id() {
        // An id sent as 20x40 RGB and then again as a PNG: the image kitty
        // holds is the PNG (a retransmission replaces it), whose size the
        // escape does not state — so the placement cannot be sized and the
        // cursor stays, as it does for the PNG alone (#626). It moved by
        // the RGB image's 2x2 cells.
        let emu = graphics_emu(
            Graphics::Kitty,
            6,
            10,
            b"\x1b_Ga=t,f=24,s=20,v=40,i=1;AAAA\x1b\\\x1b_Ga=t,f=100,i=1;iVBORw0KGgo=\x1b\\\
              \x1b_Ga=p,i=1\x1b\\X",
        );
        assert_eq!(x_at(&emu), Some((0, 0)));
        // The other way round, the newest transmission is sized: 2x2.
        let emu = graphics_emu(
            Graphics::Kitty,
            6,
            10,
            b"\x1b_Ga=t,f=100,i=1;iVBORw0KGgo=\x1b\\\x1b_Ga=t,f=24,s=20,v=40,i=1;AAAA\x1b\\\
              \x1b_Ga=p,i=1\x1b\\X",
        );
        assert_eq!(x_at(&emu), Some((1, 2)));
        // A placement is no transmission: a second `a=p` of the same id is
        // sized by the image, not by the first placement, which states none.
        let emu = graphics_emu(
            Graphics::Kitty,
            6,
            10,
            b"\x1b_Ga=t,f=24,s=20,v=40,i=1;AAAA\x1b\\\x1b_Ga=p,i=1\x1b\\\x1b[H\
              \x1b_Ga=p,i=1\x1b\\X",
        );
        assert_eq!(x_at(&emu), Some((1, 2)));
    }

    /// A delete that frees an image makes kitty forget it, and a later
    /// `a=p` of it is refused ("Put command refers to non-existent image",
    /// `handle_put_command` in kitty's `graphics.c`): nothing is placed, so
    /// nothing moves, `c=` and `r=` or not (#672). It was sized from the
    /// image in the payload log. `d=I` frees the image with that id and
    /// `d=R` every id from `x=` to `y=`; the lower-case forms remove only
    /// placements and keep the image ("The uppercase variants will delete
    /// the image data as well", the protocol's "Deleting images").
    #[test]
    fn a_kitty_placement_of_an_image_a_delete_freed_does_not_move() {
        let sent = b"\x1b_Ga=t,f=24,s=20,v=40,i=7;AAAA\x1b\\";
        for (delete, at) in [
            (&b"\x1b_Ga=d,d=I,i=7\x1b\\"[..], (0, 0)),
            (b"\x1b_Ga=d,d=R,x=5,y=9\x1b\\", (0, 0)),
            (b"\x1b_Ga=d,d=i,i=7\x1b\\", (1, 2)),
            (b"\x1b_Ga=d,d=r,x=5,y=9\x1b\\", (1, 2)),
            (b"\x1b_Ga=d,d=I,i=8\x1b\\", (1, 2)),
            (b"\x1b_Ga=d,d=R,x=8,y=9\x1b\\", (1, 2)),
            (b"\x1b_Ga=d\x1b\\", (1, 2)),
        ] {
            let wire = [&sent[..], delete, b"\x1b_Ga=p,i=7\x1b\\X"].concat();
            let emu = graphics_emu(Graphics::Kitty, 6, 10, &wire);
            assert_eq!(x_at(&emu), Some(at), "{}", String::from_utf8_lossy(delete));
        }
        // Refused, the placement has no size to give: `c=` and `r=` do not
        // place what kitty has not got.
        let wire = [
            &sent[..],
            b"\x1b_Ga=d,d=I,i=7\x1b\\\x1b_Ga=p,i=7,c=3,r=2\x1b\\X",
        ]
        .concat();
        assert_eq!(
            x_at(&graphics_emu(Graphics::Kitty, 6, 10, &wire)),
            Some((0, 0))
        );
        // Sent again after the delete, it is an image kitty has: 2x2.
        let wire = [
            &sent[..],
            b"\x1b_Ga=d,d=I,i=7\x1b\\",
            sent,
            b"\x1b_Ga=p,i=7\x1b\\X",
        ]
        .concat();
        assert_eq!(
            x_at(&graphics_emu(Graphics::Kitty, 6, 10, &wire)),
            Some((1, 2))
        );
    }

    /// The delete facts are kept one for one with the payload log, through
    /// its eviction: a log that has evicted its oldest entries still pairs
    /// each delete with itself, so the newest transmission is not taken for
    /// a deleted one (#672).
    #[test]
    fn delete_facts_stay_paired_with_the_payloads_through_eviction() {
        let mut wire = Vec::new();
        for _ in 0..HISTORY + 10 {
            wire.extend_from_slice(b"\x1b_Ga=d,d=I,i=7\x1b\\");
        }
        wire.extend_from_slice(b"\x1b_Ga=t,f=24,s=20,v=40,i=8;AAAA\x1b\\\x1b_Ga=p,i=8\x1b\\X");
        let emu = graphics_emu(Graphics::Kitty, 6, 10, &wire);
        assert_eq!(x_at(&emu), Some((1, 2)));
        assert_eq!(emu.graphics.len(), emu.graphics_forgets.len());
    }

    /// A query that kitty took as an upload's last chunk completes the
    /// upload, and an `a=T` upload is then placed — `handle_put_command`
    /// runs for it in kitty's `grman_handle_command` — before the query is
    /// answered (#672).
    #[test]
    fn a_query_that_completes_an_upload_places_its_image_first() {
        let mut emu = Vt100Emulator::new(6, 10, 100, crate::graphics::DEFAULT_CAPTURE, false);
        emu.declare_graphics(Graphics::Kitty, Some((10, 20)));
        let wire = b"\x1b_Ga=T,f=24,s=1,v=1,c=3,r=2,i=5,m=1;AA\x1b\\\x1b_Ga=q,i=31;AA\x1b\\X";
        let processed = emu.process(wire);
        assert!(
            matches!(processed.stop, Some(Stop::Query(_))),
            "stops for the query: {:?}",
            processed.stop
        );
        feed_all(&mut emu, &wire[processed.consumed..]);
        assert_eq!(x_at(&emu), Some((1, 3)));
        let screen = emu.snapshot();
        let seen = screen.graphics();
        let image = seen.last().expect("the upload's image");
        assert_eq!(
            (image.id(), image.chunks(), image.at()),
            (Some(5), 2, (0, 0))
        );
    }

    #[test]
    fn a_sixel_moves_the_cursor_to_its_last_text_row_and_keeps_the_column() {
        // xterm, sixel scrolling on: 45 px over 20-px cells is three rows,
        // so the cursor goes down two, in the column the image started at.
        let sixel = b"\x1b[1;3H\x1bPq\"1;1;4;45#0;2;100;0;0~~~~-~~~~-~~~~\x1b\\X";
        assert_eq!(
            x_at(&graphics_emu(Graphics::Sixel, 6, 10, sixel)),
            Some((2, 2))
        );
        // An image shorter than a cell stays on its row.
        let short = b"\x1bPq\"1;1;4;6#0;2;100;0;0~~~~\x1b\\X";
        assert_eq!(
            x_at(&graphics_emu(Graphics::Sixel, 6, 10, short)),
            Some((0, 0))
        );
        // kitty ignores sixel, and the default ignores both.
        for support in [Graphics::None, Graphics::Kitty] {
            assert_eq!(
                x_at(&graphics_emu(support, 6, 10, sixel)),
                Some((0, 2)),
                "{support:?}"
            );
        }
    }

    /// The cursor moves past a sixel by its full height whatever the
    /// capture bound kept of it (#635): xterm's height is the larger of the
    /// raster's and the painted one, and both are measured as the stream
    /// arrives. Past the bound the move used to come from whatever was kept
    /// — at bound 0, nothing, so the cursor stayed.
    #[test]
    fn a_sixel_moves_the_cursor_the_same_at_every_capture_bound() {
        // Declared 45 px over 20-px cells: three rows, painted two.
        let declared = b"\x1b[1;2H\x1bPq\"1;1;8;45#0;2;100;0;0!8~-!8~\x1b\\X";
        // Undeclared, five bands painted: 30 px, two rows.
        let painted = b"\x1b[1;2H\x1bPq#0;2;100;0;0~-~-~-~-~\x1b\\X";
        for capture in [crate::graphics::DEFAULT_CAPTURE, 20, 16, 0] {
            for (wire, at, size) in [
                (&declared[..], (2, 1), Some((8, 45))),
                (&painted[..], (1, 1), None),
            ] {
                let mut emu = Vt100Emulator::new(6, 10, 100, capture, false);
                emu.declare_graphics(Graphics::Sixel, Some((10, 20)));
                feed_all(&mut emu, wire);
                assert_eq!(x_at(&emu), Some(at), "capture {capture}");
                let screen = emu.snapshot();
                assert_eq!(
                    screen.graphics().last().and_then(GraphicsPayload::size),
                    size,
                    "capture {capture}"
                );
            }
        }
    }

    /// `u32::MAX` in a placement's `c=`, `r=`, `s=` or `v=` is a number the
    /// program under test may send, and must neither panic the emulator nor
    /// wrap into a small move (#634). Each panicked in a debug build.
    #[test]
    fn a_kitty_placement_of_any_size_moves_the_cursor_without_overflowing() {
        for (keys, at) in [
            // So many rows the screen scrolls through, and so many columns
            // the cursor wraps: the last row's start.
            ("s=1,v=1,c=4294967295", (5, 0)),
            ("s=1,v=1,r=4294967295", (5, 0)),
            // 429496730 columns, one row: wraps to the next row.
            ("s=4294967295,v=1,X=5", (1, 0)),
            // One column, 214748366 rows: to the bottom, one column on.
            ("s=1,v=4294967295,Y=7", (5, 1)),
        ] {
            let wire = format!("\x1b_Ga=T,f=24,{keys};AAAA\x1b\\X");
            let emu = graphics_emu(Graphics::Kitty, 6, 10, wire.as_bytes());
            assert_eq!(x_at(&emu), Some(at), "{keys}");
        }
    }

    #[test]
    fn history_rows_keep_the_width_they_were_captured_at() {
        // Documented decision (`Terminal::resize`): history is not reflowed
        // and not discarded. Rows captured before a narrowing resize keep
        // their width; rows captured after it have the new one; both sit in
        // the same history.
        let mut emu = Vt100Emulator::new(2, 10, 100, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, b"0123456789\r\nabcdefghij\r\nnext");
        assert_eq!(emu.snapshot().scrollback_text(), "0123456789");
        emu.set_size(2, 4);
        assert_eq!(
            emu.snapshot().scrollback_text(),
            "0123456789",
            "a captured row keeps its width; narrowing the screen must not \
             retroactively rewrite history"
        );
        // Two more rows at the new width scroll off; the old row is still
        // there, still ten wide, above the four-wide ones.
        feed_all(&mut emu, b"\r\nwxyz\r\nlast");
        let history = emu.snapshot().scrollback_text();
        let rows: Vec<&str> = history.lines().collect();
        assert_eq!(rows[0], "0123456789", "captured at 10 columns: {history}");
        assert!(
            rows[1..].iter().all(|row| row.chars().count() <= 4),
            "captured at 4 columns: {history}"
        );
        assert!(rows.len() >= 3, "{history}");
    }

    #[test]
    fn mouse_tracking_modes_are_reset_until_one_is_enabled() {
        // The state every application is in when it probes at startup.
        // Nothing was collapsed, so "reset" is a fact rather than a guess —
        // and answering it is what lets capability detection succeed.
        let emu = emu_with(b"plain");
        for mode in [9, 1000, 1002, 1003] {
            assert_eq!(emu.mode_state(mode), ModeState::Reset, "mode {mode}");
        }
    }

    #[test]
    fn each_tracking_mode_is_answered_on_its_own_evidence() {
        // crossterm's EnableMouseCapture: three modes in one breath. vt100
        // keeps only the last, and before the tracker held the set (#151)
        // the other two had to be answered "not recognized" — the only
        // honest reply to a question the state could not answer.
        let emu = emu_with(b"\x1b[?1000h\x1b[?1002h\x1b[?1003h");
        for mode in [1000, 1002, 1003] {
            assert_eq!(emu.mode_state(mode), ModeState::Set, "mode {mode}");
        }
        assert_eq!(emu.mode_state(9), ModeState::Reset, "never asked for");
        // The set is what was asked for; the protocol is the one set last.
        let screen = emu.snapshot();
        assert_eq!(screen.mouse_mode(), MouseMode::AnyMotion);
        assert_eq!(
            screen.mouse_modes().iter().collect::<Vec<_>>(),
            [
                MouseMode::PressRelease,
                MouseMode::ButtonMotion,
                MouseMode::AnyMotion
            ]
        );

        // Releasing one member releases that member alone in the set, while
        // reporting turns off, as in xterm, where a reset of any member
        // ends it. Both facts are true; each is reported where it belongs.
        let emu = emu_with(b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1003l");
        assert_eq!(emu.mode_state(1003), ModeState::Reset);
        assert_eq!(emu.mode_state(1002), ModeState::Set);
        let screen = emu.snapshot();
        assert_eq!(screen.mouse_mode(), MouseMode::None);
        assert!(screen.mouse_modes().contains(MouseMode::ButtonMotion));

        // A list sets or clears every member it names, and a hard reset
        // empties the set.
        let emu = emu_with(b"\x1b[?1000;1002;1003h\x1b[?1000;1002l");
        assert_eq!(
            emu.snapshot().mouse_modes().iter().collect::<Vec<_>>(),
            [MouseMode::AnyMotion]
        );
        assert!(emu_with(b"\x1b[?1003h\x1bc")
            .snapshot()
            .mouse_modes()
            .is_empty());
    }

    #[test]
    fn turning_tracking_off_returns_every_mode_to_reset() {
        let emu = emu_with(b"\x1b[?1002h\x1b[?1002l");
        for mode in [9, 1000, 1002, 1003] {
            assert_eq!(emu.mode_state(mode), ModeState::Reset, "mode {mode}");
        }
    }

    /// A reset of any tracking mode turns reporting off, whichever mode is
    /// in force: xterm's `really_set_mousemode` sets `MOUSE_OFF` on a reset
    /// of 9, 1000, 1002 or 1003 alike, and `Screen::mouse_mode` says so.
    /// vt100 clears its mode only when the reset names the one it holds, so
    /// after `?1002h ?1000l` the input path went on reporting (#693). The
    /// set is a different fact and keeps what was asked for and not
    /// disabled, as `DECRQM` reads it.
    #[test]
    fn a_reset_of_another_tracking_mode_turns_reporting_off() {
        for (stream, asked) in [
            (&b"\x1b[?1002h\x1b[?1000l"[..], MouseMode::ButtonMotion),
            (b"\x1b[?9h\x1b[?1000l", MouseMode::Press),
            (b"\x1b[?1000h\x1b[?1003h\x1b[?1000l", MouseMode::AnyMotion),
            // In a list with a mode that is not a tracking mode.
            (b"\x1b[?1003h\x1b[?25;1002l", MouseMode::AnyMotion),
        ] {
            let emu = emu_with(stream);
            let at = String::from_utf8_lossy(stream).replace('\x1b', "^[");
            assert_eq!(emu.input_modes().mouse, MouseMode::None, "{at}");
            let screen = emu.snapshot();
            assert_eq!(screen.mouse_mode(), MouseMode::None, "{at}");
            assert_eq!(
                screen.mouse_modes().iter().collect::<Vec<_>>(),
                [asked],
                "{at}: asked for, and not disabled"
            );
        }
        // The last mode set is the one in force, in one list as across
        // sequences, and setting one again after a reset turns it back on.
        for (stream, in_force) in [
            (&b"\x1b[?1003h\x1b[?1000h"[..], MouseMode::PressRelease),
            (b"\x1b[?1003;9h", MouseMode::Press),
            (
                b"\x1b[?1002h\x1b[?1000l\x1b[?1002h",
                MouseMode::ButtonMotion,
            ),
            // A reset naming no tracking mode leaves reporting alone.
            (b"\x1b[?1002h\x1b[?1006l\x1b[?25l", MouseMode::ButtonMotion),
        ] {
            let emu = emu_with(stream);
            let at = String::from_utf8_lossy(stream).replace('\x1b', "^[");
            assert_eq!(emu.input_modes().mouse, in_force, "{at}");
            assert_eq!(emu.snapshot().mouse_mode(), in_force, "{at}");
        }
    }

    #[test]
    fn set_size_resizes_the_grid() {
        let mut emu = emu_with(b"hello");
        emu.set_size(2, 5);
        let screen = emu.snapshot();
        assert_eq!(screen.size(), (5, 2));
        assert_eq!(screen.text(), "hello\n");
    }

    /// A 24-column emulator, the width the issue's reproductions use.
    fn wide_emu(bytes: &[u8]) -> Vt100Emulator {
        let mut emu = Vt100Emulator::new(2, 24, 0, crate::graphics::DEFAULT_CAPTURE, false);
        feed_all(&mut emu, bytes);
        emu
    }

    /// Where the cursor ended up, which is the whole of what a tab does.
    fn cursor_col(bytes: &[u8]) -> u16 {
        wide_emu(bytes).snapshot().cursor().1
    }

    #[test]
    fn a_plain_tab_still_lands_on_the_default_eighth_column() {
        // The rewrite takes `HT` over from vt100 entirely, so the behaviour
        // that was already right has to keep being right.
        let s = wide_emu(b"a\tb").snapshot();
        assert_eq!(s.row_text(0).trim_end(), "a       b", "{s}");
        assert_eq!(s.find("b"), Some((0, 8)));
        assert_eq!(cursor_col(b"\t\t"), 16);
    }

    /// The scope note from the issue, and the one test that proves the two
    /// tab implementations are not disagreeing: `HTS` sets a stop and a
    /// *plain* `\t` — not `CHT` — is what honours it.
    #[test]
    fn a_plain_tab_honours_a_stop_set_by_hts() {
        // Column 4 (`CSI 4 G` is one-based), set a stop, back to column 1.
        let s = wide_emu(b"\x1b[4G\x1bH\x1b[1Ga\tb").snapshot();
        assert_eq!(s.row_text(0).trim_end(), "a  b", "{s}");
        assert_eq!(s.find("b"), Some((0, 3)));
    }

    #[test]
    fn tbc_clears_one_stop_and_csi_3_g_clears_them_all() {
        // Standing on the default stop at column 8 and clearing it sends the
        // next tab on to 16 instead.
        assert_eq!(cursor_col(b"\x1b[9G\x1b[g\x1b[1G\t"), 16);
        assert_eq!(cursor_col(b"\x1b[9G\x1b[0g\x1b[1G\t"), 16);
        // With every stop gone a tab runs to the last column and stays.
        assert_eq!(cursor_col(b"\x1b[3g\t"), 23);
        assert_eq!(cursor_col(b"\x1b[3g\t\t\t"), 23);
    }

    #[test]
    fn cht_and_cbt_move_by_whole_stops() {
        assert_eq!(cursor_col(b"\x1b[2I"), 16);
        assert_eq!(cursor_col(b"\x1b[I"), 8);
        // Back-tab from a stop goes to the one before it. Written without a
        // character in the way, because a character advances the cursor and
        // the back-tab would then return to the stop it was standing on —
        // which is what xterm and alacritty both do, and what the issue's
        // second reproduction reads past.
        assert_eq!(cursor_col(b"\t\t\x1b[1Z"), 8);
        assert_eq!(cursor_col(b"\t\tX\x1b[1Z"), 16);
        assert_eq!(cursor_col(b"\t\t\x1b[2Z"), 0);
        // Nowhere left to go is column 0, not a wrap.
        assert_eq!(cursor_col(b"\x1b[9Z"), 0);
    }

    /// Through the grid: `RIS` restores the default stops, and `DECSTR`
    /// keeps the custom one, as xterm's soft reset does (#687).
    #[test]
    fn ris_restores_the_default_stops_and_decstr_keeps_them() {
        for (reset, col) in [(&b"\x1bc"[..], 8), (b"\x1b[!p", 3)] {
            let mut stream = b"\x1b[3g\x1b[4G\x1bH".to_vec();
            stream.extend_from_slice(reset);
            stream.extend_from_slice(b"\x1b[1G\t");
            assert_eq!(cursor_col(&stream), col, "after {reset:?}");
        }
    }

    #[test]
    fn a_resize_extends_the_stops_into_the_new_columns() {
        let mut emu = wide_emu(b"\x1b[4G\x1bH");
        emu.set_size(2, 40);
        // The stop set before the resize is still there …
        feed_all(&mut emu, b"\x1b[1G\t");
        assert_eq!(emu.snapshot().cursor().1, 3);
        // … and the columns the grid did not have get the default pattern.
        feed_all(&mut emu, b"\x1b[25G\t");
        assert_eq!(emu.snapshot().cursor().1, 32);
    }

    /// The rewrite is fed to both parsers, so the attribute shadow moves
    /// with the primary grid. `snapshot` debug-asserts they agree, which is
    /// what this drives — a tab inside a styled span is the shape that would
    /// break if only one of them were told.
    #[test]
    fn a_tab_inside_a_styled_span_keeps_the_shadow_in_step() {
        let s = wide_emu(b"\x1b[5m\x1b[4G\x1bH\x1b[1Ga\tb\x1b[0m").snapshot();
        assert_eq!(s.find("b"), Some((0, 3)));
        assert!(s.cell(0, 3).unwrap().style().blink, "{s}");
    }

    /// The text, the cells, the cursor, the wrap flags and the history.
    type Observed = (String, Vec<Cell>, (u16, u16, bool), Vec<bool>, String);

    /// Everything a snapshot shows, as one comparable value.
    fn observed(emu: &Vt100Emulator) -> Observed {
        let s = emu.snapshot();
        let (cols, rows) = s.size();
        let cells = (0..rows)
            .flat_map(|row| (0..cols).map(move |col| (row, col)))
            .map(|(row, col)| s.cell(row, col).unwrap().clone())
            .collect();
        let wrapped = (0..rows).map(|row| s.row_wrapped(row)).collect();
        (s.text(), cells, s.cursor(), wrapped, s.scrollback_text())
    }

    /// `bytes` fed whole, and again one byte at a time: a screen must not
    /// depend on where the PTY split the output.
    fn whole_and_bytewise(
        rows: u16,
        cols: u16,
        scrollback: usize,
        bytes: &[u8],
    ) -> (Vt100Emulator, Vt100Emulator) {
        let mut whole = Vt100Emulator::new(
            rows,
            cols,
            scrollback,
            crate::graphics::DEFAULT_CAPTURE,
            false,
        );
        feed_all(&mut whole, bytes);
        let mut bytewise = Vt100Emulator::new(
            rows,
            cols,
            scrollback,
            crate::graphics::DEFAULT_CAPTURE,
            false,
        );
        // Split on character boundaries only, as the reader's UTF-8
        // sanitizer hands them over.
        let text = std::str::from_utf8(bytes).expect("test streams are UTF-8");
        let mut buf = [0; 4];
        for ch in text.chars() {
            feed_all(&mut bytewise, ch.encode_utf8(&mut buf).as_bytes());
        }
        (whole, bytewise)
    }

    #[test]
    fn a_selector_widened_emoji_at_the_bottom_right_moves_to_the_new_bottom_row() {
        // `⚠` lands in the last column of the bottom row, and its selector
        // widens it to two: it no longer fits, so it moves to the start of
        // the next row, which here is a new one scrolled in. vt100 0.18.1
        // took the cell to move before the scroll and moved a blank one,
        // leaving `⚠️` wide in the last column of the row above — which the
        // next write over it panicked on (#619).
        let status = format!("\x1b[4;1H{:>19}\u{26a0}\u{fe0f}", "warn");
        for styled in [false, true] {
            let mut emu = Vt100Emulator::new(4, 20, 100, crate::graphics::DEFAULT_CAPTURE, styled);
            feed_all(&mut emu, status.as_bytes());
            let s = emu.snapshot();
            assert_eq!(s.row_text(2).trim_end(), format!("{:>19}", "warn"), "{s}");
            assert_eq!(s.row_text(3).trim_end(), "\u{26a0}\u{fe0f}", "{s}");
            assert!(s.cell(3, 0).unwrap().is_wide(), "{s}");
            assert!(s.cell(3, 1).unwrap().is_wide_continuation(), "{s}");
            assert!(!s.cell(2, 19).unwrap().is_wide(), "{s}");
            assert_eq!(s.cursor(), (3, 2, true));
            // The redraw that panicked: one row up, over the whole row.
            feed_all(&mut emu, format!("\x1b[A\r{:<20}", "redrawn").as_bytes());
            let s = emu.snapshot();
            assert_eq!(s.row_text(2).trim_end(), "redrawn", "{s}");
            assert_eq!(s.row_text(3).trim_end(), "\u{26a0}\u{fe0f}", "{s}");
        }
        // Without the redraw, on the issue's 9x5: the row above keeps its
        // nine columns, and the emoji and what follows it are on the new row.
        let emu = emu_with_size(5, 9, "\x1b[5;9H\u{26a0}\u{fe0f}X".as_bytes());
        let s = emu.snapshot();
        assert_eq!(s.row_text(3), " ".repeat(9), "{s}");
        assert_eq!(s.row_text(4).trim_end(), "\u{26a0}\u{fe0f}X", "{s}");
        assert_eq!(s.find("X"), Some((4, 2)), "{s}");
        // Inside a scroll region the rows outside it stay put.
        let emu = emu_with_size(
            5,
            9,
            "\x1b[5;1Hfoot\x1b[2;4r\x1b[4;9H\u{26a0}\u{fe0f}X".as_bytes(),
        );
        let s = emu.snapshot();
        assert_eq!(s.row_text(2), " ".repeat(9), "{s}");
        assert_eq!(s.row_text(3).trim_end(), "\u{26a0}\u{fe0f}X", "{s}");
        assert_eq!(s.row_text(4).trim_end(), "foot", "{s}");
    }

    #[test]
    fn a_selector_in_the_last_column_lays_out_the_same_however_it_is_split() {
        // The bottom-right case and the one a row higher, where vt100 was
        // already right, whole and a character at a time.
        for at in ["\x1b[4;20H", "\x1b[2;20H"] {
            let stream = format!("{at}\u{26a0}\u{fe0f}after\x1b[A\r{:<20}", "over");
            let (whole, bytewise) = whole_and_bytewise(4, 20, 100, stream.as_bytes());
            assert_eq!(observed(&whole), observed(&bytewise), "{at:?}");
        }
    }

    #[test]
    fn a_selector_moved_off_the_last_column_lands_where_vt100_puts_it() {
        // Where the row does not scroll vt100 already moved the emoji
        // right, and termlens now moves it itself: the result must be
        // vt100's own, cell for cell, wrap flags and cursor included — over
        // a wide character, too, whose second half the emoji lands on.
        for stream in [
            "\x1b[2;20H\u{26a0}\u{fe0f}X",
            "\x1b[3;1H\u{6f22}bc\x1b[2;1H0123456789012345678\u{2764}\u{fe0f}",
            "\x1b[44m\x1b[1;20Ha\u{301}\u{fe0f}\u{26a0}\x1b[0m\u{fe0f}",
            "\x1b[44m\x1b[2;20H\u{26a0}\x1b[31m\u{fe0f}",
        ] {
            let mut raw = ::vt100::Parser::new(nz(4), nz(20), 0);
            raw.process(stream.as_bytes());
            let emu = emu_with_size(4, 20, stream.as_bytes());
            let ours = emu.parser.screen();
            assert_eq!(ours.contents(), raw.screen().contents(), "{stream:?}");
            assert_eq!(
                ours.cursor_position(),
                raw.screen().cursor_position(),
                "{stream:?}"
            );
            for row in 0..4 {
                assert_eq!(
                    ours.row_wrapped(row),
                    raw.screen().row_wrapped(row),
                    "{stream:?}"
                );
                for col in 0..20 {
                    assert_eq!(
                        ours.cell(row, col),
                        raw.screen().cell(row, col),
                        "{stream:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn insert_mode_lays_out_the_same_however_the_input_is_split() {
        // The issue's two streams on a 2x5 grid (#624), and the shapes
        // around them. A run that reaches the right margin wraps and pushes
        // the next row right, as xterm does, rather than overwriting it.
        for (stream, expected) in [
            ("\x1b[2;1H12345\x1b[1;4H\x1b[4hXYZW", "   XY\nZW123"),
            ("XYZ\x1b[H\x1b[4h\u{26a0}\u{fe0f}", "\u{26a0}\u{fe0f}XYZ\n"),
            // A wide character that does not fit in the last column.
            ("\x1b[2;1H12345\x1b[1;5H\x1b[4h\u{6f22}", "\n\u{6f22}123"),
            // The bottom row: the wrap scrolls, and the new row is blank.
            ("\x1b[2;1H12345\x1b[2;4H\x1b[4hXYZ", "123XY\nZ"),
            // A selector widening the last column's emoji in insert mode.
            (
                "\x1b[2;1H12345\x1b[1;5H\x1b[4h\u{26a0}\u{fe0f}",
                "\n\u{26a0}\u{fe0f}123",
            ),
        ] {
            let (whole, bytewise) = whole_and_bytewise(2, 5, 0, stream.as_bytes());
            assert_eq!(whole.snapshot().text(), expected, "{stream:?} whole");
            assert_eq!(observed(&whole), observed(&bytewise), "{stream:?}");
        }
    }

    #[test]
    fn an_inserted_character_that_wraps_keeps_the_row_marked_wrapped() {
        // The wrap stays vt100's, so the row it leaves is marked as a soft
        // wrap and `logical_text` joins the two — the last column is
        // redrawn in its own attributes to put the cursor back past it,
        // and both parsers keep theirs (blink lives in the shadow).
        let emu = emu_with_size(
            3,
            5,
            b"\x1b[2;1Habcde\x1b[1;1H\x1b[31m123\x1b[5;1m45\x1b[0;32m\x1b[4hXY",
        );
        let s = emu.snapshot();
        // `de` is pushed off the end of its row, as `ICH` pushes it.
        assert_eq!(s.text(), "12345\nXYabc\n", "{s}");
        assert!(s.row_wrapped(0), "{s}");
        assert!(s.logical_text().starts_with("12345XYabc\n"), "{s}");
        let five = *s.cell(0, 4).unwrap().style();
        assert!(
            five.blink && five.bold && five.fg == Color::Indexed(1),
            "{five:?}"
        );
        // The application's own attributes carry on after the redraw.
        let x = *s.cell(1, 0).unwrap().style();
        assert!(!x.blink && !x.bold && x.fg == Color::Indexed(2), "{x:?}");
    }

    #[test]
    fn wide_characters_selectors_and_insert_mode_never_depend_on_the_split() {
        // A seeded sweep over the shapes #619 and #624 came from: emoji with
        // and without selectors, wide and combining characters, insert mode
        // on and off, scroll regions, origin mode and cursor movement, the
        // alternate screen and resets, on small grids where everything
        // reaches a margin. Each stream must neither panic nor lay out —
        // or keep its history — differently fed a character at a time.
        const PIECES: &[&str] = &[
            "a",
            "bc",
            "\u{6f22}",
            "\u{26a0}",
            "\u{fe0f}",
            "\u{26a0}\u{fe0f}",
            "\u{301}",
            "\u{2764}",
            "\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}",
            "\r",
            "\n",
            "\x1b[A",
            "\x1b[B",
            "\x1b[C",
            "\x1b[H",
            "\x1b[4h",
            "\x1b[4l",
            "\x1b[2;3r",
            "\x1b[r",
            "\x1b[?6h",
            "\x1b[?6l",
            "\x1b[K",
            "\x1b[1K",
            "\x1b[@",
            "\x1b[P",
            "\x1b[3;5H",
            "\x1b[1;99H",
            "\x1b[99;1H",
            "\x1b[99;99H",
            "\x1b[5m",
            "\x1b[31;1m",
            "\x1b[0m",
            "\x1b[2b",
            "\x1b[?1049h",
            "\x1b[?1049l",
            "\x1b[?47h",
            "\x1b[?47l",
            "\x1bc",
            // DECSTR's replay reads the cursor to put it back (#687).
            "\x1b[!p",
            "\x1b7",
            "\x1b8",
        ];
        let mut seed: u64 = 0x5eed_0619_0624;
        let mut next = move |n: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(seed >> 33).unwrap() % n
        };
        for case in 0..3000 {
            let (rows, cols) = [(2, 5), (3, 4), (4, 7)][case % 3];
            let stream: String = (0..24).map(|_| PIECES[next(PIECES.len())]).collect();
            let (whole, bytewise) = whole_and_bytewise(rows, cols, 10, stream.as_bytes());
            assert_eq!(
                observed(&whole),
                observed(&bytewise),
                "case {case}: {stream:?}"
            );
        }
    }

    /// The character sets `DECSC` saves are kept per screen, as xterm keeps
    /// them (`screen->sc[screen->whichBuf]`), and `1049` saves and restores
    /// them around the alternate screen. A line-drawing set designated by a
    /// full-screen application used to leak out of it onto the main screen,
    /// so `quit` printed after it drew `─┤␋├` (#685).
    #[test]
    fn the_alternate_screens_character_sets_do_not_outlive_1049() {
        let s = emu_with(b"\x1b[?1049h\x1b(0lqk\x1b[?1049l\x1b[Hquit").snapshot();
        assert_eq!(s.row_text(0).trim_end(), "quit", "{s}");
    }

    /// `DECRC` on the main screen restores what `DECSC` saved there, not
    /// what the alternate screen saved last (#685).
    #[test]
    fn a_restore_on_the_main_screen_reads_the_main_screens_slot() {
        // 1049: its own save is the main slot, the alternate screen's
        // `ESC 7` goes to the other one, and the main screen's `ESC 8` reads
        // its own after a designation made since.
        let s = emu_with(b"\x1b[?1049h\x1b(0\x1b7\x1b(B\x1b[?1049l\x1b(0\x1b8q").snapshot();
        assert_eq!(s.row_text(0).trim_end(), "q", "after 1049: {s}");
        // 47 saves nothing itself: the main screen's slot is the `ESC 7`
        // made there before the switch.
        let s = emu_with(b"\x1b7\x1b[?47h\x1b(0\x1b7\x1b[?47l\x1b8q").snapshot();
        assert_eq!(s.row_text(0).trim_end(), "q", "after 47: {s}");
        // And the alternate screen's slot is still its own when it is back.
        let s = emu_with(b"\x1b[?47h\x1b(0\x1b7\x1b(B\x1b[?47l\x1b7\x1b[?47h\x1b8q").snapshot();
        assert_eq!(s.row_text(0).trim_end(), "\u{2500}", "back on 47: {s}");
    }

    /// `CSI s` / `CSI u` without parameters and `?1048h` / `?1048l` are
    /// xterm's other cursor saves, and save the character sets as `DECSC`
    /// does (#685). vt100 moves no cursor for any of them, which
    /// `unsupported()` goes on naming; the sets are termlens's own.
    #[test]
    fn scosc_and_1048_save_and_restore_the_character_sets() {
        for (save, restore) in [("\x1b[s", "\x1b[u"), ("\x1b[?1048h", "\x1b[?1048l")] {
            let stream = format!("\x1b(0{save}\x1b(B{restore}qqq");
            let s = emu_with(stream.as_bytes()).snapshot();
            assert_eq!(
                s.row_text(0).trim_end(),
                "\u{2500}".repeat(3),
                "{save:?}: {s}"
            );
        }
        // With a parameter `CSI u` is not a restore (kitty's keyboard
        // protocol owns the shapes around it), and neither is `CSI s`.
        let s = emu_with(b"\x1b(0\x1b[1s\x1b(B\x1b[1uq").snapshot();
        assert_eq!(s.row_text(0).trim_end(), "q", "{s}");
    }

    /// `DECSTR` keeps what xterm's soft reset keeps: mouse tracking, the
    /// mouse encoding, focus reporting, bracketed paste and the tab stops
    /// all survive it, since xterm resets them only on `RIS` (#687).
    #[test]
    fn a_soft_reset_keeps_the_modes_xterm_keeps() {
        let emu = emu_with(b"\x1b[?1000h\x1b[?1006h\x1b[?1004h\x1b[?2004h\x1b[!p");
        let modes = emu.input_modes();
        assert_eq!(modes.mouse, MouseMode::PressRelease);
        assert_eq!(modes.mouse_encoding, MouseEncoding::Sgr);
        assert!(modes.focus_events);
        assert!(modes.bracketed_paste);
        for mode in [1000, 1006, 1004, 2004] {
            assert_eq!(emu.mode_state(mode), ModeState::Set, "?{mode}");
        }
        assert_eq!(emu.snapshot().mouse_mode(), MouseMode::PressRelease);
        // A custom tab stop survives too.
        assert_eq!(cursor_col(b"\x1b[3g\x1b[4G\x1bH\x1b[!p\x1b[1G\t"), 3);
    }

    /// `DECSTR` clears the rendition, as xterm's soft reset does: a
    /// character written after it is plain (#687).
    #[test]
    fn a_soft_reset_clears_the_rendition() {
        let s = emu_with(b"\x1b[31;1;5;8;9m\x1b[!pX").snapshot();
        assert_eq!(s.find("X"), Some((0, 0)), "{s}");
        assert_eq!(*s.cell(0, 0).unwrap().style(), Style::default(), "{s}");
    }

    /// `DECSTR` clears the scroll margins and origin mode, and leaves the
    /// cursor where it was: vt100 homes it on both resets, so the replay
    /// moves it back (#687).
    #[test]
    fn a_soft_reset_clears_the_margins_and_origin_mode() {
        let s = emu_with(b"top\x1b[2;3r\x1b[?6h\x1b[!p\x1b[1;1HX").snapshot();
        assert_eq!(s.find("X"), Some((0, 0)), "{s}");
        // The margins are gone: a line feed on row 3 scrolls the whole
        // screen, not a region that ends there.
        let s = emu_with(b"top\x1b[2;3r\x1b[!p\x1b[3;1Hmid\x1b[4;1Hbot\r\nX").snapshot();
        assert_eq!(s.text(), "\nmid\nbot\nX", "{s}");
        // The cursor stays where the reset found it.
        let emu = emu_with(b"\x1b[2;4r\x1b[?6h\x1b[2;5H\x1b[!p");
        assert_eq!(emu.snapshot().cursor(), (2, 4, true));
    }

    /// `DECSTR` homes the saved cursor, and a restore after it finds the
    /// sets it reset (#687).
    #[test]
    fn a_soft_reset_homes_the_saved_cursor() {
        let s = emu_with(b"\x1b(0\x1b[3;5H\x1b7\x1b[!p\x1b[2;2H\x1b8q").snapshot();
        assert_eq!(s.find("q"), Some((0, 0)), "{s}");
    }

    /// The tracker follows vt100's margins and origin mode, which vt100
    /// keeps per grid and does not expose, so a report is checked here
    /// against what vt100 does with the `CUP` an application sends back:
    /// under origin mode `CSI H` lands on the top margin, and outside it on
    /// row 0, and the report must count from exactly that row (#689). The
    /// streams mix every sequence that moves either — regions valid and
    /// not, both screen switches, the saves, both resets — with resizes.
    #[test]
    fn the_report_origin_follows_vt100s_margins_and_origin_mode() {
        const PIECES: &[&str] = &[
            "\x1b[2;4r",
            "\x1b[3r",
            "\x1b[r",
            "\x1b[;3r",
            "\x1b[4;2r",
            "\x1b[2:1;5r",
            "\x1b[9;99r",
            "\x1b[?6h",
            "\x1b[?6l",
            "\x1b[?6;1049h",
            "\x1b[?1049;6h",
            "\x1b7",
            "\x1b8",
            "\x1b[s",
            "\x1b[u",
            "\x1b[?1048h",
            "\x1b[?1048l",
            "\x1b[?1049h",
            "\x1b[?1049l",
            "\x1b[?47h",
            "\x1b[?47l",
            "\x1b[?1047h",
            "\x1b[!p",
            "\x1bc",
            "ab\r\n",
            "\x1b[3;2H",
            "RESIZE",
        ];
        let mut seed: u64 = 0x0689_0687_0685;
        let mut next = move |n: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(seed >> 33).unwrap() % n
        };
        for case in 0..3000 {
            let ops: Vec<&str> = (0..16).map(|_| PIECES[next(PIECES.len())]).collect();
            let sizes = [(5, 8), (6, 8), (6, 9)];
            let run = |extra: &[u8]| {
                let mut emu = Vt100Emulator::new(5, 8, 0, crate::graphics::DEFAULT_CAPTURE, false);
                for (i, op) in ops.iter().enumerate() {
                    if *op == "RESIZE" {
                        let (rows, cols) = sizes[i % sizes.len()];
                        emu.set_size(rows, cols);
                    } else {
                        feed_all(&mut emu, op.as_bytes());
                    }
                }
                feed_all(&mut emu, extra);
                emu
            };
            let emu = run(b"");
            let homed = run(b"\x1b[H");
            assert_eq!(
                emu.tracker.report_origin_row(),
                homed.cursor_position().0,
                "case {case}: {ops:?}"
            );
        }
    }
}
