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

/// What one byte did to the synchronized-update state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncEvent {
    /// No change.
    None,
    /// The byte completed a `CSI ? 2026 … h`: a synchronized update began.
    Begin,
    /// The byte completed a `CSI ? 2026 … l`: a frame is now complete.
    End,
}

#[derive(Debug)]
pub(crate) struct SeqTracker {
    state: State,
    /// Continuation bytes still expected for the current UTF-8 character.
    utf8_remaining: u8,
    /// True between a 2026 `h` and the matching `l`.
    sync_update: bool,
    // Incremental CSI parameter scanner (only what mode 2026 needs).
    csi_private: bool,
    csi_invalid: bool,
    csi_first: bool,
    csi_param: u32,
    csi_saw_2026: bool,
}

impl SeqTracker {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Ground,
            utf8_remaining: 0,
            sync_update: false,
            csi_private: false,
            csi_invalid: false,
            csi_first: true,
            csi_param: 0,
            csi_saw_2026: false,
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
        self.csi_private = false;
        self.csi_invalid = false;
        self.csi_first = true;
        self.csi_param = 0;
        self.csi_saw_2026 = false;
    }

    /// Track one CSI parameter/intermediate byte.
    fn scan_csi_byte(&mut self, b: u8) {
        match b {
            b'?' if self.csi_first => self.csi_private = true,
            b'0'..=b'9' => {
                self.csi_param = self
                    .csi_param
                    .saturating_mul(10)
                    .saturating_add(u32::from(b - b'0'));
            }
            b';' => {
                if self.csi_param == 2026 {
                    self.csi_saw_2026 = true;
                }
                self.csi_param = 0;
            }
            // Sub-parameters, intermediates, or other private markers:
            // whatever this sequence is, it is not a plain mode 2026 set.
            _ => self.csi_invalid = true,
        }
        self.csi_first = false;
    }

    /// The sync-state change (if any) implied by a CSI final byte.
    fn csi_final(&mut self, b: u8) -> SyncEvent {
        if !self.csi_private || self.csi_invalid {
            return SyncEvent::None;
        }
        if !(self.csi_saw_2026 || self.csi_param == 2026) {
            return SyncEvent::None;
        }
        match b {
            b'h' => {
                self.sync_update = true;
                SyncEvent::Begin
            }
            b'l' => {
                self.sync_update = false;
                SyncEvent::End
            }
            _ => SyncEvent::None,
        }
    }

    pub(crate) fn step(&mut self, b: u8) -> SyncEvent {
        const ESC: u8 = 0x1b;
        const CAN: u8 = 0x18;
        const SUB: u8 = 0x1a;
        const BEL: u8 = 0x07;

        let mut event = SyncEvent::None;
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
                BEL => State::Ground,
                ESC => State::OscEsc,
                CAN | SUB => State::Ground,
                _ => State::Osc,
            },
            State::Dcs => match b {
                ESC => State::DcsEsc,
                CAN | SUB => State::Ground,
                _ => State::Dcs, // BEL is data inside DCS-class strings
            },
            State::OscEsc | State::DcsEsc => match b {
                b'\\' => State::Ground, // ESC \ = ST
                ESC => self.state,
                // Anything else: the ESC aborted the string and starts a new
                // escape sequence; reprocess this byte in Esc state.
                _ => {
                    self.state = State::Esc;
                    return self.step(b);
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
        let events: Vec<SyncEvent> = b"\x1b[?2026h".iter().map(|&b| t.step(b)).collect();
        assert_eq!(*events.last().unwrap(), SyncEvent::Begin);
        assert!(t.in_sync_update());
        let events: Vec<SyncEvent> = b"\x1b[?2026l".iter().map(|&b| t.step(b)).collect();
        assert_eq!(*events.last().unwrap(), SyncEvent::End);
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
