//! A minimal escape-sequence progress tracker.
//!
//! This is deliberately NOT a VT parser — the emulator interprets the
//! stream. It answers two questions:
//!
//! 1. For `wait_idle`: *did the byte stream end in the middle of
//!    something?* — an escape/CSI/OSC/DCS sequence or a partial UTF-8
//!    character. Declaring a terminal "idle" between the two halves of a
//!    split `ESC [ 3 1 m` would hand tests a torn frame.
//! 2. For `wait_frame`: *where do synchronized updates begin and end?*
//!    DEC private mode 2026 (`CSI ? 2026 h` / `CSI ? 2026 l`) brackets a
//!    repaint; the byte that ends one marks a complete frame. Parameters
//!    are parsed incrementally in O(1) space, and `?2026` is recognized
//!    anywhere in a multi-mode list such as `CSI ? 2026 ; 25 h`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Plain text.
    Ground,
    /// Got ESC, awaiting the introducer or final byte.
    Esc,
    /// Inside `ESC <intermediate 0x20-0x2F>…`, awaiting a final byte.
    EscIntermediate,
    /// Inside a CSI sequence (`ESC [ …`), awaiting a final byte 0x40–0x7E.
    Csi,
    /// Inside an OSC string (`ESC ] …`), terminated by BEL or ST.
    Osc,
    /// Inside a DCS/SOS/PM/APC string, terminated by ST only.
    Dcs,
    /// Inside an OSC string and just saw ESC (potential `ESC \` = ST).
    OscEsc,
    /// Inside a DCS-class string and just saw ESC (potential ST).
    DcsEsc,
}

/// What one byte completed, if anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SeqEvent {
    /// Nothing actionable.
    None,
    /// A `CSI ? 2026 … h` completed: a synchronized update began.
    SyncBegin,
    /// A `CSI ? 2026 … l` completed: a frame is now complete.
    SyncEnd,
    /// The application asked the terminal a question.
    Query(Query),
}

/// A terminal query the application issued. The tracker classifies;
/// policy (answer vs record) lives with the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Query {
    /// DSR cursor position: `CSI 6 n` (or the DEC `CSI ? 6 n` form).
    CursorPosition {
        /// True for the `?`-prefixed DECXCPR form.
        private: bool,
    },
    /// DSR operating status: `CSI 5 n`.
    OperatingStatus,
    /// Primary device attributes: `CSI c` / `CSI 0 c`.
    PrimaryDa,
    /// Secondary device attributes: `CSI > c` / `CSI > 0 c`.
    SecondaryDa,
    /// Text-area size in characters: `CSI 18 t`.
    TextAreaSize,
    /// OSC color query (`OSC 10;?` foreground / `OSC 11;?` background).
    OscColor {
        /// 10 = foreground, 11 = background.
        code: u8,
        /// True when the query used ST; the reply must mirror it.
        st_terminated: bool,
    },
    /// Recognized as a question, but one termlens has no answer for
    /// (XTGETTCAP, kitty `CSI ? u`, other DSR/DA/XTWINOPS reports, …).
    /// Carries a printable rendering for diagnostics.
    Unanswerable(String),
}

/// Render a captured escape sequence printably (`ESC` becomes `^[`).
fn printable(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            0x1b => out.push_str("^["),
            0x07 => out.push_str("^G"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

#[derive(Debug)]
pub(crate) struct SeqTracker {
    state: State,
    /// Continuation bytes still expected for the current UTF-8 character.
    utf8_remaining: u8,
    /// True between a 2026 `h` and the matching `l`.
    sync_update: bool,
    // Incremental CSI scanner: enough to recognize mode 2026 and the
    // handful of query shapes, in O(1) space.
    csi_prefix: u8,
    csi_invalid: bool,
    csi_first: bool,
    csi_param: u32,
    csi_has_digits: bool,
    csi_first_param: u32,
    csi_param_count: u8,
    csi_saw_2026: bool,
    /// Raw capture of the current sequence (from ESC), for diagnostics
    /// and OSC/DCS query recognition. Bounded; long sequences truncate.
    seq_buf: [u8; 24],
    seq_len: u8,
}

impl SeqTracker {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Ground,
            utf8_remaining: 0,
            sync_update: false,
            csi_prefix: 0,
            csi_invalid: false,
            csi_first: true,
            csi_param: 0,
            csi_has_digits: false,
            csi_first_param: 0,
            csi_param_count: 0,
            csi_saw_2026: false,
            seq_buf: [0; 24],
            seq_len: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.step(b);
        }
    }

    pub(crate) fn mid_sequence(&self) -> bool {
        self.state != State::Ground || self.utf8_remaining > 0
    }

    /// True while the stream is inside a DEC 2026 synchronized update.
    pub(crate) fn in_sync_update(&self) -> bool {
        self.sync_update
    }

    fn reset_csi_scanner(&mut self) {
        self.csi_prefix = 0;
        self.csi_invalid = false;
        self.csi_first = true;
        self.csi_param = 0;
        self.csi_has_digits = false;
        self.csi_first_param = 0;
        self.csi_param_count = 0;
        self.csi_saw_2026 = false;
    }

    fn push_seq(&mut self, b: u8) {
        if usize::from(self.seq_len) < self.seq_buf.len() {
            self.seq_buf[usize::from(self.seq_len)] = b;
            self.seq_len += 1;
        }
    }

    fn seq_printable(&self) -> String {
        printable(&self.seq_buf[..usize::from(self.seq_len)])
    }

    /// Close the parameter currently being accumulated.
    fn end_csi_param(&mut self) {
        if self.csi_param == 2026 {
            self.csi_saw_2026 = true;
        }
        if self.csi_param_count == 0 {
            self.csi_first_param = self.csi_param;
        }
        self.csi_param_count = self.csi_param_count.saturating_add(1);
        self.csi_param = 0;
        self.csi_has_digits = false;
    }

    /// Track one CSI parameter/intermediate byte.
    fn scan_csi_byte(&mut self, b: u8) {
        match b {
            b'?' | b'>' | b'=' if self.csi_first => self.csi_prefix = b,
            b'0'..=b'9' => {
                self.csi_param = self
                    .csi_param
                    .saturating_mul(10)
                    .saturating_add(u32::from(b - b'0'));
                self.csi_has_digits = true;
            }
            b';' => self.end_csi_param(),
            // Sub-parameters or intermediates: none of the sequences we
            // recognize use them.
            _ => self.csi_invalid = true,
        }
        self.csi_first = false;
    }

    /// The event (if any) implied by a CSI final byte.
    fn csi_final(&mut self, b: u8) -> SeqEvent {
        if self.csi_invalid {
            return SeqEvent::None;
        }
        if self.csi_has_digits {
            self.end_csi_param();
        }
        let params_empty = self.csi_param_count == 0;
        let single = |v: u32| self.csi_param_count == 1 && self.csi_first_param == v;

        // DEC private mode 2026 (synchronized output).
        if self.csi_prefix == b'?' && self.csi_saw_2026 {
            match b {
                b'h' => {
                    self.sync_update = true;
                    return SeqEvent::SyncBegin;
                }
                b'l' => {
                    self.sync_update = false;
                    return SeqEvent::SyncEnd;
                }
                _ => {}
            }
        }

        // Queries. Classification only — answering policy lives upstream.
        let query = match (self.csi_prefix, b) {
            (0, b'n') if single(6) => Some(Query::CursorPosition { private: false }),
            (b'?', b'n') if single(6) => Some(Query::CursorPosition { private: true }),
            (0, b'n') if single(5) => Some(Query::OperatingStatus),
            (0, b'c') if params_empty || single(0) => Some(Query::PrimaryDa),
            (b'>', b'c') if params_empty || single(0) => Some(Query::SecondaryDa),
            (0, b't') if single(18) => Some(Query::TextAreaSize),
            // Questions we can recognize but not answer.
            (_, b'n') | (b'=', b'c') => Some(Query::Unanswerable(self.seq_printable())),
            (b'?', b'u') if params_empty => {
                // kitty keyboard probe. Its protocol pairs this with DA1;
                // our DA1 answer unblocks the probe like any non-kitty
                // terminal, but the probe itself is still unanswered.
                Some(Query::Unanswerable(self.seq_printable()))
            }
            (0, b't')
                if matches!(self.csi_first_param, 11 | 13 | 14 | 16 | 19 | 20 | 21)
                    && self.csi_param_count == 1 =>
            {
                Some(Query::Unanswerable(self.seq_printable()))
            }
            _ => None,
        };
        query.map_or(SeqEvent::None, SeqEvent::Query)
    }

    /// The event (if any) implied by a completed OSC or DCS string.
    fn string_final(&mut self, was_osc: bool, st_terminated: bool) -> SeqEvent {
        let body = &self.seq_buf[..usize::from(self.seq_len)];
        if was_osc {
            // body is `ESC ] <content> [terminator…]`; a color query is
            // exactly `10;?` or `11;?` (12 = cursor color: unanswerable).
            let content_end = if st_terminated {
                body.len().saturating_sub(2) // strip ESC \
            } else {
                body.len().saturating_sub(1) // strip BEL
            };
            let content = body.get(2..content_end).unwrap_or(b"");
            match content {
                b"10;?" => {
                    return SeqEvent::Query(Query::OscColor {
                        code: 10,
                        st_terminated,
                    })
                }
                b"11;?" => {
                    return SeqEvent::Query(Query::OscColor {
                        code: 11,
                        st_terminated,
                    })
                }
                b"12;?" => return SeqEvent::Query(Query::Unanswerable(self.seq_printable())),
                _ => return SeqEvent::None,
            }
        }
        // DCS: XTGETTCAP is `ESC P + q … ST` — a capability question.
        if body.get(2..4) == Some(b"+q") {
            return SeqEvent::Query(Query::Unanswerable(self.seq_printable()));
        }
        SeqEvent::None
    }

    /// Feed one byte: capture it if it belongs to a sequence, then run
    /// the state machine.
    pub(crate) fn step(&mut self, b: u8) -> SeqEvent {
        if self.state == State::Ground {
            if b == 0x1b {
                self.seq_len = 0;
                self.push_seq(b);
            }
        } else {
            self.push_seq(b);
        }
        self.transition(b)
    }

    fn transition(&mut self, b: u8) -> SeqEvent {
        const ESC: u8 = 0x1b;
        const CAN: u8 = 0x18;
        const SUB: u8 = 0x1a;
        const BEL: u8 = 0x07;

        let mut event = SeqEvent::None;
        self.state = match self.state {
            State::Ground => {
                if b == ESC {
                    self.utf8_remaining = 0;
                    State::Esc
                } else {
                    self.track_utf8(b);
                    State::Ground
                }
            }
            State::Esc => match b {
                b'[' => {
                    self.reset_csi_scanner();
                    State::Csi
                }
                b']' => State::Osc,
                // DCS, SOS, PM, APC — string sequences terminated by ST.
                b'P' | b'X' | b'^' | b'_' => State::Dcs,
                0x20..=0x2f => State::EscIntermediate,
                ESC => State::Esc,
                CAN | SUB => State::Ground,
                // Final byte of a two-character sequence (ESC c, ESC 7, …).
                _ => State::Ground,
            },
            State::EscIntermediate => match b {
                0x20..=0x2f => State::EscIntermediate,
                ESC => State::Esc,
                CAN | SUB => State::Ground,
                _ => State::Ground,
            },
            State::Csi => match b {
                0x40..=0x7e => {
                    event = self.csi_final(b);
                    State::Ground
                }
                ESC => State::Esc,
                CAN | SUB => State::Ground,
                // Parameter/intermediate bytes (and embedded C0 controls).
                _ => {
                    self.scan_csi_byte(b);
                    State::Csi
                }
            },
            State::Osc => match b {
                BEL => {
                    event = self.string_final(true, false);
                    State::Ground
                }
                ESC => State::OscEsc,
                CAN | SUB => State::Ground,
                _ => State::Osc,
            },
            State::Dcs => match b {
                ESC => State::DcsEsc,
                CAN | SUB => State::Ground,
                _ => State::Dcs, // BEL is data inside DCS-class strings
            },
            State::OscEsc => match b {
                b'\\' => {
                    event = self.string_final(true, true); // ESC \ = ST
                    State::Ground
                }
                ESC => State::OscEsc,
                // The ESC aborted the string and starts a new sequence;
                // reprocess this byte in Esc state (capture already done).
                _ => {
                    self.state = State::Esc;
                    return self.transition(b);
                }
            },
            State::DcsEsc => match b {
                b'\\' => {
                    event = self.string_final(false, true);
                    State::Ground
                }
                ESC => State::DcsEsc,
                _ => {
                    self.state = State::Esc;
                    return self.transition(b);
                }
            },
        };
        event
    }

    fn track_utf8(&mut self, b: u8) {
        if self.utf8_remaining > 0 && (0x80..=0xbf).contains(&b) {
            self.utf8_remaining -= 1;
            return;
        }
        self.utf8_remaining = match b {
            0xc2..=0xdf => 1,
            0xe0..=0xef => 2,
            0xf0..=0xf4 => 3,
            // ASCII, stray continuation, or invalid lead: not mid-character.
            _ => 0,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(bytes: &[u8]) -> SeqTracker {
        let mut t = SeqTracker::new();
        t.feed(bytes);
        t
    }

    #[test]
    fn plain_text_is_ground() {
        assert!(!fed(b"hello world\r\n").mid_sequence());
    }

    #[test]
    fn split_csi_is_mid_sequence_until_final_byte() {
        let mut t = SeqTracker::new();
        t.feed(b"\x1b[3");
        assert!(t.mid_sequence());
        t.feed(b"1");
        assert!(t.mid_sequence());
        t.feed(b"m");
        assert!(!t.mid_sequence());
    }

    #[test]
    fn two_char_escape_completes() {
        assert!(!fed(b"\x1b7").mid_sequence()); // DECSC
        assert!(fed(b"\x1b").mid_sequence());
    }

    #[test]
    fn esc_intermediate_completes_on_final() {
        assert!(fed(b"\x1b(").mid_sequence()); // charset designation, unfinished
        assert!(!fed(b"\x1b(B").mid_sequence());
    }

    #[test]
    fn osc_terminated_by_bel_or_st() {
        assert!(fed(b"\x1b]0;title").mid_sequence());
        assert!(!fed(b"\x1b]0;title\x07").mid_sequence());
        assert!(!fed(b"\x1b]0;title\x1b\\").mid_sequence());
    }

    #[test]
    fn dcs_terminated_by_st_only() {
        assert!(fed(b"\x1bPdata").mid_sequence());
        assert!(fed(b"\x1bPdata\x07").mid_sequence()); // BEL is DCS payload
        assert!(!fed(b"\x1bPdata\x1b\\").mid_sequence());
    }

    #[test]
    fn esc_inside_string_starts_new_sequence() {
        // ESC c aborts the OSC and completes as its own two-char escape.
        assert!(!fed(b"\x1b]0;title\x1bc").mid_sequence());
        // ESC [ aborts the OSC and leaves us inside a CSI.
        assert!(fed(b"\x1b]0;title\x1b[3").mid_sequence());
    }

    #[test]
    fn can_aborts_sequences() {
        assert!(!fed(b"\x1b[31\x18").mid_sequence());
    }

    #[test]
    fn sync_update_events_fire_on_2026_set_and_reset() {
        let mut t = SeqTracker::new();
        let events: Vec<SeqEvent> = b"\x1b[?2026h".iter().map(|&b| t.step(b)).collect();
        assert_eq!(*events.last().unwrap(), SeqEvent::SyncBegin);
        assert!(t.in_sync_update());
        let events: Vec<SeqEvent> = b"\x1b[?2026l".iter().map(|&b| t.step(b)).collect();
        assert_eq!(*events.last().unwrap(), SeqEvent::SyncEnd);
        assert!(!t.in_sync_update());
    }

    #[test]
    fn sync_2026_is_recognized_anywhere_in_a_multi_mode_list() {
        let mut t = SeqTracker::new();
        t.feed(b"\x1b[?2026;25h");
        assert!(t.in_sync_update());
        let mut t = SeqTracker::new();
        t.feed(b"\x1b[?25;2026h");
        assert!(t.in_sync_update());
    }

    #[test]
    fn lookalike_sequences_do_not_toggle_sync() {
        let mut t = SeqTracker::new();
        t.feed(b"\x1b[2026h"); // not private (no '?')
        assert!(!t.in_sync_update());
        t.feed(b"\x1b[?2026m"); // wrong final byte
        assert!(!t.in_sync_update());
        t.feed(b"\x1b[?2026:1h"); // sub-parameter form: not a plain mode set
        assert!(!t.in_sync_update());
        t.feed(b"\x1b[?20260h"); // different mode number
        assert!(!t.in_sync_update());
    }

    #[test]
    fn sync_survives_an_aborted_csi_inside_the_update() {
        let mut t = SeqTracker::new();
        t.feed(b"\x1b[?2026h\x1b[31\x18"); // CAN aborts the SGR, not the frame
        assert!(t.in_sync_update());
        t.feed(b"\x1b[?2026l");
        assert!(!t.in_sync_update());
    }

    fn queries_of(bytes: &[u8]) -> Vec<Query> {
        let mut t = SeqTracker::new();
        bytes
            .iter()
            .filter_map(|&b| match t.step(b) {
                SeqEvent::Query(q) => Some(q),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn recognizes_the_answerable_queries() {
        assert_eq!(
            queries_of(b"\x1b[6n"),
            vec![Query::CursorPosition { private: false }]
        );
        assert_eq!(
            queries_of(b"\x1b[?6n"),
            vec![Query::CursorPosition { private: true }]
        );
        assert_eq!(queries_of(b"\x1b[5n"), vec![Query::OperatingStatus]);
        assert_eq!(queries_of(b"\x1b[c"), vec![Query::PrimaryDa]);
        assert_eq!(queries_of(b"\x1b[0c"), vec![Query::PrimaryDa]);
        assert_eq!(queries_of(b"\x1b[>c"), vec![Query::SecondaryDa]);
        assert_eq!(queries_of(b"\x1b[18t"), vec![Query::TextAreaSize]);
        assert_eq!(
            queries_of(b"\x1b]11;?\x07"),
            vec![Query::OscColor {
                code: 11,
                st_terminated: false
            }]
        );
        assert_eq!(
            queries_of(b"\x1b]10;?\x1b\\"),
            vec![Query::OscColor {
                code: 10,
                st_terminated: true
            }]
        );
    }

    #[test]
    fn recognizes_unanswerable_questions_with_their_shape() {
        let q = queries_of(b"\x1b[?u");
        assert_eq!(q, vec![Query::Unanswerable("^[[?u".into())]);
        let q = queries_of(b"\x1b[14t");
        assert_eq!(q, vec![Query::Unanswerable("^[[14t".into())]);
        let q = queries_of(b"\x1bP+q544e\x1b\\"); // XTGETTCAP
        assert_eq!(q, vec![Query::Unanswerable("^[P+q544e^[\\".into())]);
        let q = queries_of(b"\x1b[=c"); // DA3
        assert_eq!(q, vec![Query::Unanswerable("^[[=c".into())]);
        let q = queries_of(b"\x1b]12;?\x07"); // cursor color
        assert_eq!(q, vec![Query::Unanswerable("^[]12;?^G".into())]);
        // Any CSI …n is a DSR-family status request by definition.
        let q = queries_of(b"\x1b[6;1n");
        assert_eq!(q, vec![Query::Unanswerable("^[[6;1n".into())]);
    }

    #[test]
    fn ordinary_output_is_not_a_query() {
        assert!(queries_of(b"\x1b[31m").is_empty()); // SGR
        assert!(queries_of(b"\x1b[2J\x1b[H").is_empty()); // clear+home
        assert!(queries_of(b"\x1b[8;30;100t").is_empty()); // resize command
        assert!(queries_of(b"\x1b]0;title\x07").is_empty()); // set title
        assert!(queries_of(b"\x1b[1;6H").is_empty()); // cursor move
        assert!(queries_of(b"plain text").is_empty());
    }

    #[test]
    fn split_utf8_is_mid_sequence() {
        let bytes = "汉".as_bytes(); // 3 bytes
        let mut t = SeqTracker::new();
        t.feed(&bytes[..1]);
        assert!(t.mid_sequence());
        t.feed(&bytes[1..2]);
        assert!(t.mid_sequence());
        t.feed(&bytes[2..]);
        assert!(!t.mid_sequence());
    }
}
