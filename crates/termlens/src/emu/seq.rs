//! A minimal escape-sequence progress tracker.
//!
//! This is deliberately NOT a VT parser — the emulator interprets the
//! stream. It answers exactly one question for `wait_idle`: *did the byte
//! stream end in the middle of something?* — i.e. inside an escape/CSI/OSC
//! /DCS sequence or a partially received UTF-8 character. Declaring a
//! terminal "idle" between the two halves of a split `ESC [ 3 1 m` would
//! hand tests a torn frame.

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

#[derive(Debug)]
pub(crate) struct SeqTracker {
    state: State,
    /// Continuation bytes still expected for the current UTF-8 character.
    utf8_remaining: u8,
}

impl SeqTracker {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Ground,
            utf8_remaining: 0,
        }
    }

    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.step(b);
        }
    }

    pub(crate) fn mid_sequence(&self) -> bool {
        self.state != State::Ground || self.utf8_remaining > 0
    }

    fn step(&mut self, b: u8) {
        const ESC: u8 = 0x1b;
        const CAN: u8 = 0x18;
        const SUB: u8 = 0x1a;
        const BEL: u8 = 0x07;

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
                b'[' => State::Csi,
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
                0x40..=0x7e => State::Ground,
                ESC => State::Esc,
                CAN | SUB => State::Ground,
                // Parameter/intermediate bytes (and embedded C0 controls).
                _ => State::Csi,
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
                    self.step(b);
                    return;
                }
            },
        };
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
