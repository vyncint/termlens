//! Invalid UTF-8 becomes U+FFFD before it reaches the emulator.
//!
//! The backend's parser drops a byte it cannot decode, so a stray `0xE9` in
//! a Latin-1 file name vanished from the grid and every column to its right
//! shifted left (#217) — a terminal shows a replacement character there and
//! keeps the columns where they are. Decoding happens here, once, on the
//! reader thread, so both parsers (the primary and the attribute shadow)
//! see the same valid stream.
//!
//! A multi-byte character can be split across two reads; the incomplete tail
//! is carried over rather than replaced, and only becomes U+FFFD when the
//! next bytes prove it was never going to complete — or at EOF.

/// Replacement character, as bytes.
const REPLACEMENT: &[u8] = "\u{FFFD}".as_bytes();

/// A streaming UTF-8 sanitizer: valid sequences pass through unchanged,
/// invalid ones become U+FFFD, and an incomplete trailing sequence waits for
/// the next chunk.
#[derive(Debug, Default)]
pub(crate) struct Utf8Sanitizer {
    /// The incomplete tail of the previous chunk — at most three bytes.
    carry: Vec<u8>,
}

impl Utf8Sanitizer {
    /// Sanitize one chunk. The result is always valid UTF-8; bytes that
    /// might still complete a character are held back for the next call.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut input: Vec<u8>;
        let mut rest: &[u8] = if self.carry.is_empty() {
            bytes
        } else {
            input = std::mem::take(&mut self.carry);
            input.extend_from_slice(bytes);
            &input
        };
        let mut out = Vec::with_capacity(rest.len());
        loop {
            match std::str::from_utf8(rest) {
                Ok(_) => {
                    out.extend_from_slice(rest);
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    out.extend_from_slice(&rest[..valid]);
                    match e.error_len() {
                        // Definitely invalid: one replacement per rejected
                        // sequence, then carry on after it.
                        Some(bad) => {
                            out.extend_from_slice(REPLACEMENT);
                            rest = &rest[valid + bad..];
                        }
                        // Could still complete: hold it for the next chunk.
                        None => {
                            self.carry.extend_from_slice(&rest[valid..]);
                            break;
                        }
                    }
                }
            }
        }
        out
    }

    /// True while a partial character is being held back — the stream ends
    /// mid-character, which `wait_idle` must not read as silence.
    pub(crate) fn pending(&self) -> bool {
        !self.carry.is_empty()
    }

    /// At end of stream: whatever is still held back was never completed.
    pub(crate) fn finish(&mut self) -> Vec<u8> {
        if self.carry.is_empty() {
            return Vec::new();
        }
        self.carry.clear();
        REPLACEMENT.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).expect("sanitizer output is valid UTF-8")
    }

    #[test]
    fn valid_input_passes_through_untouched() {
        let mut san = Utf8Sanitizer::default();
        assert_eq!(
            s(&san.feed("ascii and 汉字 and 🦀".as_bytes())),
            "ascii and 汉字 and 🦀"
        );
        assert!(!san.pending());
    }

    #[test]
    fn an_invalid_byte_becomes_one_replacement_and_the_columns_hold() {
        let mut san = Utf8Sanitizer::default();
        assert_eq!(s(&san.feed(b"caf\xe9 done")), "caf\u{FFFD} done");
        assert!(!san.pending());
    }

    #[test]
    fn a_character_split_across_two_reads_is_reassembled() {
        let mut san = Utf8Sanitizer::default();
        let bytes = "ab汉cd".as_bytes(); // 汉 is E6 B1 89
        let first = san.feed(&bytes[..3]); // "ab" + E6
        assert_eq!(s(&first), "ab");
        assert!(san.pending(), "the lead byte is held, not replaced");
        let second = san.feed(&bytes[3..]);
        assert_eq!(s(&second), "汉cd");
        assert!(!san.pending());
    }

    #[test]
    fn a_lead_byte_the_next_read_does_not_continue_is_replaced_then() {
        let mut san = Utf8Sanitizer::default();
        assert_eq!(s(&san.feed(b"ab\xe9")), "ab");
        assert!(san.pending());
        assert_eq!(s(&san.feed(b"cd")), "\u{FFFD}cd");
        assert!(!san.pending());
    }

    #[test]
    fn a_partial_character_at_eof_is_one_replacement() {
        let mut san = Utf8Sanitizer::default();
        assert_eq!(s(&san.feed(b"x\xf0\x9f")), "x");
        assert!(san.pending());
        assert_eq!(s(&san.finish()), "\u{FFFD}");
        assert!(!san.pending());
        assert!(san.finish().is_empty());
    }

    #[test]
    fn every_invalid_sequence_is_replaced_separately() {
        let mut san = Utf8Sanitizer::default();
        // Two stray continuation bytes, then a truncated three-byte lead
        // followed by ASCII.
        assert_eq!(
            s(&san.feed(b"a\x80\x80b\xe6\xb1c")),
            "a\u{FFFD}\u{FFFD}b\u{FFFD}c"
        );
    }
}
