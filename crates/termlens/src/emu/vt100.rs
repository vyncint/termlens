//! The `vt100`-crate backend. Public types never leak from here: every
//! snapshot converts vt100's grid into termlens's own [`Screen`].

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::seq::{SeqEvent, SeqTracker};
use super::shadow::AttrShadow;
use super::{Emulator, FrameSpan, InputModes, ModeState, MouseEncoding, Processed, Stop};
use crate::graphics::{GraphicsPayload, GraphicsSeen, HISTORY};
use crate::screen::{Cell, Color, MouseMode, Screen, Style, TermState};

pub(crate) struct Vt100Emulator {
    parser: ::vt100::Parser,
    tracker: SeqTracker,
    /// Carries blink, conceal and strikethrough, which vt100 drops. See
    /// `emu/shadow.rs` for why this is a second parser rather than
    /// hand-rolled attribute tracking.
    shadow: AttrShadow,
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
    /// Rows of vt100's own scrollback already copied into `history`, so a
    /// read that scrolled nothing costs one length check.
    captured: usize,
    /// Inline graphics payloads, oldest first, behind an `Arc` so a
    /// snapshot costs one refcount rather than a copy of every image.
    graphics: Arc<Vec<GraphicsPayload>>,
    /// Payload bytes currently retained, so eviction is a subtraction
    /// rather than a walk.
    graphics_bytes: usize,
    /// The retention budget, from `TerminalBuilder::capture_graphics`.
    capture: usize,
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

    pub(crate) fn new(rows: u16, cols: u16, scrollback_len: usize, capture: usize) -> Self {
        Self {
            parser: ::vt100::Parser::new(rows, cols, scrollback_len),
            tracker: SeqTracker::new(capture),
            shadow: AttrShadow::new(rows, cols),
            scrollback_len,
            frame_started: None,
            history: VecDeque::new(),
            captured: 0,
            graphics: Arc::new(Vec::new()),
            graphics_bytes: 0,
            capture,
        }
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
        self.parser.process(bytes);
        self.shadow.feed(bytes);
        self.capture_scrolled_rows();
    }

    /// File a completed payload, stamped with where it landed.
    ///
    /// Neither protocol's escape moves the cursor, so the position once the
    /// terminator has been consumed *is* the image's top-left corner — the
    /// one fact about an image that lives in the grid rather than in the
    /// payload, and the one an application gets wrong when a picture drifts
    /// out from under its own labels.
    fn record_graphics(&mut self, mut payload: GraphicsPayload) {
        payload.place(self.parser.screen().cursor_position());
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
        let (rows, cols) = self.parser.screen().size();
        let screen = self.parser.screen_mut();

        // `set_scrollback` clamps to the real history length, so asking for
        // more than exists is how we learn how much exists.
        screen.set_scrollback(usize::MAX);
        let len = screen.scrollback();

        // Below the cap, history only grows: the new rows are exactly
        // `captured..len`, and an unchanged length means nothing scrolled.
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
                self.history.push_back(Arc::from(line.trim_end()));
            }
            i += take;
        }
        while self.history.len() > self.scrollback_len {
            self.history.pop_front();
        }
        self.captured = len;
        screen.set_scrollback(0);
    }
}

impl Emulator for Vt100Emulator {
    fn process(&mut self, bytes: &[u8]) -> Processed {
        // How much of this segment the grid has already been given. Only a
        // graphics payload moves it mid-segment; everything else is fed in
        // one go, exactly as before.
        let mut fed = 0;
        for (i, &byte) in bytes.iter().enumerate() {
            let stop = match self.tracker.step(byte) {
                SeqEvent::SyncEnd => Some(Stop::FrameComplete(self.close_frame())),
                SeqEvent::Query(query) => Some(Stop::Query(query)),
                SeqEvent::Graphics(payload) => {
                    self.feed(&bytes[fed..=i]);
                    fed = i + 1;
                    self.record_graphics(*payload);
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
                self.feed(&bytes[fed..=i]);
                return Processed {
                    consumed: i + 1,
                    stop: Some(stop),
                };
            }
        }
        self.feed(&bytes[fed..]);
        Processed {
            consumed: bytes.len(),
            stop: None,
        }
    }

    fn snapshot(&self) -> Screen {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let mut cells = Vec::with_capacity(usize::from(rows) * usize::from(cols));
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
                cells.push(screen.cell(row, col).map_or_else(
                    || Cell::new(String::new(), Style::default(), false, false),
                    |cell| convert_cell(cell, self.shadow.cell(row, col)),
                ));
            }
        }
        let (cursor_row, cursor_col) = screen.cursor_position();
        let state = TermState {
            title: self.tracker.title(),
            alternate_screen: screen.alternate_screen(),
            bracketed_paste: screen.bracketed_paste(),
            application_cursor: screen.application_cursor(),
            mouse: convert_mouse(screen.mouse_protocol_mode()),
            clipboard: self.tracker.clipboard(),
            bells: self.tracker.bells(),
            focus_events: self.tracker.focus_events(),
            graphics: GraphicsSeen::new(self.tracker.graphics(), Arc::clone(&self.graphics)),
            // Filled in by the terminal, which owns the frame count.
            repaints: 0,
            scrollback: self.history.iter().cloned().collect(),
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
            // The mouse tracking modes need care, because vt100 collapses
            // all four into one mutually exclusive value. Two cases, and
            // only one of them is ambiguous:
            //
            // - Nothing is tracking. Then nothing was collapsed, and every
            //   tracking mode is genuinely reset — a fact, not a guess. This
            //   is the state every application is in when it probes at
            //   startup, so it is the case that decides whether
            //   capability detection works at all.
            // - A *different* mode is tracking. The application may have set
            //   several (crossterm's EnableMouseCapture sends 1000, 1002 and
            //   1003 together) and vt100 kept only the last, so claiming the
            //   others are reset would be a guess dressed up as an answer.
            //   `NotRecognized` stays honest here.
            9 | 1000 | 1002 | 1003 => match screen.mouse_protocol_mode() {
                ::vt100::MouseProtocolMode::None => ModeState::Reset,
                ::vt100::MouseProtocolMode::Press if mode == 9 => ModeState::Set,
                ::vt100::MouseProtocolMode::PressRelease if mode == 1000 => ModeState::Set,
                ::vt100::MouseProtocolMode::ButtonMotion if mode == 1002 => ModeState::Set,
                ::vt100::MouseProtocolMode::AnyMotion if mode == 1003 => ModeState::Set,
                _ => ModeState::NotRecognized,
            },
            _ => ModeState::NotRecognized,
        }
    }

    fn set_size(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
        self.shadow.set_size(rows, cols);
        // A resize can push rows into history on its own.
        self.capture_scrolled_rows();
    }
}

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
        cell.contents().to_owned(),
        style,
        cell.is_wide(),
        cell.is_wide_continuation(),
    )
}

fn convert_color(color: ::vt100::Color) -> Color {
    match color {
        ::vt100::Color::Default => Color::Default,
        ::vt100::Color::Idx(i) => Color::Indexed(i),
        ::vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
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

    fn emu_with(bytes: &[u8]) -> Vt100Emulator {
        let mut emu = Vt100Emulator::new(4, 10, 0, crate::graphics::DEFAULT_CAPTURE);
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
    fn captures_sgr_styles() {
        let emu = emu_with(b"\x1b[1;3;4;7;31mX\x1b[0m");
        let screen = emu.snapshot();
        let style = *screen.cell(0, 0).unwrap().style();
        assert!(style.bold && style.italic && style.underline && style.reverse);
        assert_eq!(style.fg, Color::Indexed(1));
        assert_eq!(screen.cell(0, 1).unwrap().style(), &Style::default());
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

    #[test]
    fn hidden_cursor_is_reported() {
        let emu = emu_with(b"\x1b[?25l");
        assert_eq!(emu.snapshot().cursor(), (0, 0, false));
    }

    #[test]
    fn mid_sequence_tracks_partial_escape() {
        let mut emu = Vt100Emulator::new(4, 10, 0, crate::graphics::DEFAULT_CAPTURE);
        feed_all(&mut emu, b"text\x1b[3");
        assert!(emu.mid_sequence());
        feed_all(&mut emu, b"1m");
        assert!(!emu.mid_sequence());
    }

    #[test]
    fn process_stops_at_the_end_of_a_synchronized_update() {
        let mut emu = Vt100Emulator::new(4, 10, 0, crate::graphics::DEFAULT_CAPTURE);
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
        let mut emu = Vt100Emulator::new(4, 10, 0, crate::graphics::DEFAULT_CAPTURE);
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
        let mut emu = Vt100Emulator::new(4, 10, scrollback, crate::graphics::DEFAULT_CAPTURE);
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
        let mut emu = Vt100Emulator::new(4, 10, 3, crate::graphics::DEFAULT_CAPTURE);
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
        let mut emu = Vt100Emulator::new(4, 10, 2, crate::graphics::DEFAULT_CAPTURE);
        feed_all(&mut emu, b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
        assert_eq!(emu.snapshot().scrollback_text(), "b\nc");
        feed_all(&mut emu, b"g\r\nh\r\n");
        assert_eq!(emu.snapshot().scrollback_text(), "d\ne");
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
    fn history_rows_keep_the_width_they_were_captured_at() {
        // Documented limit: resize does not reflow.
        let mut emu = Vt100Emulator::new(2, 10, 100, crate::graphics::DEFAULT_CAPTURE);
        feed_all(&mut emu, b"0123456789\r\nabcdefghij\r\nnext");
        assert_eq!(emu.snapshot().scrollback_text(), "0123456789");
        emu.set_size(2, 4);
        assert_eq!(
            emu.snapshot().scrollback_text(),
            "0123456789",
            "a captured row keeps its width; narrowing the screen must not \
             retroactively rewrite history"
        );
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
    fn an_active_tracking_mode_reports_itself_and_stays_silent_on_the_rest() {
        let emu = emu_with(b"\x1b[?1002h");
        assert_eq!(emu.mode_state(1002), ModeState::Set);
        // Genuinely ambiguous: crossterm's EnableMouseCapture sends 1000,
        // 1002 and 1003 together and vt100 keeps only the last, so calling
        // the others reset would be a guess dressed up as an answer.
        for mode in [9, 1000, 1003] {
            assert_eq!(
                emu.mode_state(mode),
                ModeState::NotRecognized,
                "mode {mode}"
            );
        }
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
}
