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
            tracker: SeqTracker::new(capture, cols),
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
                    // A placement of an image sent earlier: its size is the
                    // last transmission of that id still in the log.
                    GraphicsAction::Place => {
                        let id = payload.id()?;
                        self.graphics.iter().rev().find_map(|earlier| {
                            (earlier.protocol() == GraphicsProtocol::Kitty
                                && earlier.id() == Some(id))
                            .then(|| earlier.size())
                            .flatten()
                        })
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
    fn record_graphics(&mut self, mut payload: GraphicsPayload) {
        payload.place(self.cursor_position());
        let kept = payload.data().map_or(0, <[u8]>::len);
        let log = Arc::make_mut(&mut self.graphics);
        log.push(payload);
        self.graphics_bytes += kept;
        // Evict oldest-first, on both bounds. A snapshot already taken keeps
        // its own view: it holds an `Arc` to the vector as it stood.
        while log.len() > HISTORY || (self.graphics_bytes > self.capture && log.len() > 1) {
            let dropped = log.remove(0);
            self.graphics_bytes -= dropped.data().map_or(0, <[u8]>::len);
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
        // until the main screen is back.
        if self.parser.screen().alternate_screen() {
            return;
        }
        let (rows, cols) = size(self.parser.screen());
        let screen = self.parser.screen_mut();
        let len = screen.scrollback_len();

        // Rows only scroll *into* history, so below the cap the new rows
        // are exactly `captured..len`, and an unchanged length means nothing
        // scrolled. Three writes shorten it instead. RIS and ED 3 empty it,
        // while the rows already copied here survive: the mark starts over,
        // and the rows now at the front are read from the start (#391). A
        // resize that makes the screen taller pulls the newest rows back
        // out of history onto the grid (vt100 0.18, as xterm does): those
        // rows are on screen again, so they leave the copy too, newest
        // first, and the mark follows them down. Nothing else shortens it,
        // and a resize applied while the alternate screen was up is settled
        // here, the first time the main screen is read again.
        if std::mem::take(&mut self.history_reset) {
            self.captured = 0;
        } else if len < self.captured {
            for _ in len..self.captured {
                self.history.pop_back();
                if let Some(cells) = &mut self.history_cells {
                    cells.pop_back();
                }
            }
            self.captured = len;
        }
        //
        // At the cap, vt100 evicts from the front and the length stops
        // changing, so it no longer reveals growth — and an unchanged
        // length is no longer evidence of an unchanged history. There is no
        // sound cheap test for "did it scroll?" either: consecutive
        // identical rows are ordinary output, so comparing the ends of the
        // history would miss real scrolls. So at the cap we re-read the
        // window vt100 still holds, which is by definition the newest `cap`
        // rows — exactly what we should be retaining. That costs O(cap) row
        // reads per chunk, and only for a run that has already overflowed
        // its history. Measured on 50,000 lines through an 80x24 screen:
        // 352ms with retention off, 327ms below the cap (free, within
        // noise), 639ms on this path — under 2x, for a workload well past
        // what a test drives.
        let at_cap = len == self.scrollback_len;
        if !at_cap && len == self.captured {
            screen.set_scrollback(0);
            return;
        }
        let from = if at_cap {
            self.history.clear();
            if let Some(cells) = &mut self.history_cells {
                cells.clear();
            }
            0
        } else {
            self.captured
        };

        // At offset `k` the visible window starts at history row `len - k`,
        // so `set_scrollback(len - i)` puts history row `i` at the top and
        // the next `rows` rows follow. `Screen::rows` walks the window once,
        // which matters: `Screen::cell` is O(row) per lookup.
        let mut i = from;
        while i < len {
            screen.set_scrollback(len - i);
            let take = (len - i).min(usize::from(rows));
            for line in screen.rows(0, cols).take(take) {
                self.history
                    .push_back(Arc::from(restore_replacement(line.trim_end()).as_ref()));
            }
            // Styled history: the same rows as cells, the shadow moved to
            // the same offset so blink, conceal and strikethrough come
            // along — the correspondence invariant extended from the grid
            // to history. Cell reads are O(row) each in vt100, so this is
            // the cost the builder's knob documents.
            if let Some(cells) = &mut self.history_cells {
                self.shadow.set_scrollback(len - i);
                for row in 0..u16::try_from(take).unwrap_or(u16::MAX) {
                    let mut converted: Vec<Cell> = Vec::with_capacity(usize::from(cols));
                    for col in 0..cols {
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
                self.shadow.set_scrollback(0);
            }
            i += take;
        }
        while self.history.len() > self.scrollback_len {
            self.history.pop_front();
        }
        if let Some(cells) = &mut self.history_cells {
            while cells.len() > self.scrollback_len {
                cells.pop_front();
            }
        }
        self.captured = len;
        screen.set_scrollback(0);
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
}

impl Emulator for Vt100Emulator {
    fn process(&mut self, bytes: &[u8]) -> Processed {
        // How much of this segment the grid has already been given. A
        // graphics payload moves it mid-segment, and so does a byte the
        // invoked character set redefines: that byte is staged as the glyph
        // it draws instead of being sliced through, and everything else is
        // fed in one go, exactly as before.
        let mut fed = 0;
        // Where the current run of printable bytes ends while insert mode is
        // on, so the room is reserved once per run rather than per byte.
        let mut run_until = 0;
        for (i, &byte) in bytes.iter().enumerate() {
            // Insert mode: a printable run pushes the rest of the row right
            // instead of overwriting it. vt100 does not model IRM, so the
            // room is reserved with the `ICH` it does dispatch, sized in the
            // columns the grid will give the run — its own layout, which no
            // `unicode-width` measure is (#580) — and never past the right
            // margin (#261). The bytes owed to the grid go first, so the
            // cursor column the reservation is made at is current.
            if i >= run_until
                && self.tracker.insert_mode()
                && !self.tracker.mid_sequence()
                && is_printable(byte)
            {
                let end = bytes[i..]
                    .iter()
                    .position(|&b| !is_printable(b))
                    .map_or(bytes.len(), |p| i + p);
                run_until = end;
                let width = crate::width::columns(&String::from_utf8_lossy(&bytes[i..end]));
                self.feed_staged(&bytes[fed..i]);
                fed = i;
                let screen = self.parser.screen();
                let (_, col) = screen.cursor_position();
                let room = usize::from(size(screen).1.saturating_sub(col));
                let reserve = width.min(room);
                if reserve > 0 {
                    self.feed(format!("\x1b[{reserve}@").as_bytes());
                }
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
                    // Sized before the payload joins the log, so a placement
                    // finds the earlier transmission and not itself; moved
                    // after it is stamped, so `at` is the top-left corner.
                    let step = self.image_step(&payload, &placement);
                    self.record_graphics(*payload);
                    if let Some(step) = step {
                        self.move_past_image(step);
                    }
                    None
                }
                SeqEvent::SoftReset => {
                    // The tracker has reset what it holds; the grid's
                    // modes are vt100's, which does not implement DECSTR,
                    // so it is handed the sequences that reset them one by
                    // one. The soft reset itself goes through first, in
                    // stream order, then the replay.
                    self.feed_staged(&bytes[fed..=i]);
                    fed = i + 1;
                    self.feed(SOFT_RESET_REPLAY);
                    None
                }
                SeqEvent::HardReset => {
                    self.feed_history_reset(&bytes[fed..i], &bytes[i..=i], true);
                    fed = i + 1;
                    None
                }
                SeqEvent::ScrollbackErased => {
                    self.feed_history_reset(&bytes[fed..i], &bytes[i..=i], false);
                    fed = i + 1;
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
            mouse: convert_mouse(screen.mouse_protocol_mode()),
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

    fn mid_sequence(&self) -> bool {
        self.tracker.mid_sequence()
    }

    fn in_sync_update(&self) -> bool {
        self.tracker.in_sync_update()
    }

    fn input_modes(&self) -> InputModes {
        let screen = self.parser.screen();
        InputModes {
            mouse: convert_mouse(screen.mouse_protocol_mode()),
            mouse_encoding: match screen.mouse_protocol_encoding() {
                ::vt100::MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
                ::vt100::MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
                ::vt100::MouseProtocolEncoding::Default => MouseEncoding::Legacy,
            },
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
            1006 => on(matches!(
                screen.mouse_protocol_encoding(),
                ::vt100::MouseProtocolEncoding::Sgr
            )),
            1005 => on(matches!(
                screen.mouse_protocol_encoding(),
                ::vt100::MouseProtocolEncoding::Utf8
            )),
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
        self.parser.screen_mut().set_size(nz(rows), nz(cols));
        self.shadow.set_size(rows, cols);
        self.tracker.set_cols(cols);
        // A resize can push rows into history on its own.
        self.capture_scrolled_rows();
    }
}

/// What `DECSTR` resets of the state vt100 holds, as the sequences vt100
/// understands: cursor keys back to normal (`DECCKM`), bracketed paste off,
/// every mouse tracking mode and encoding off, and the cursor visible
/// (`DECTCEM`). The rest of the specified list — attributes, margins,
/// origin and insert modes, the keypad — is deliberately not replayed:
/// none of it is observable through `Screen` today, and a replay nothing
/// can check is a claim nothing can catch. The alternate screen is left
/// alone, as the spec says.
const SOFT_RESET_REPLAY: &[u8] =
    b"\x1b[?1l\x1b[?2004l\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?25h";

fn convert_mouse(mode: ::vt100::MouseProtocolMode) -> MouseMode {
    match mode {
        ::vt100::MouseProtocolMode::None => MouseMode::None,
        ::vt100::MouseProtocolMode::Press => MouseMode::Press,
        ::vt100::MouseProtocolMode::PressRelease => MouseMode::PressRelease,
        ::vt100::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
        ::vt100::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
    }
}

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
        // The rebuild path: once at the cap vt100's length stops changing,
        // so a naive "did the length grow?" check would freeze the history
        // at its first full window.
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
        // The set is what was asked for; the protocol is vt100's collapse.
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
        // vt100 — like xterm — turns reporting off, since the protocol it
        // was reporting in is gone. Both facts are true; each is reported
        // where it belongs.
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

    #[test]
    fn ris_and_decstr_restore_the_default_stops() {
        for reset in [&b"\x1bc"[..], b"\x1b[!p"] {
            let mut stream = b"\x1b[3g\x1b[4G\x1bH".to_vec();
            stream.extend_from_slice(reset);
            stream.extend_from_slice(b"\x1b[1G\t");
            assert_eq!(
                cursor_col(&stream),
                8,
                "the custom stop must not survive {reset:?}"
            );
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
}
