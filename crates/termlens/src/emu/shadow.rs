//! Recovering the three SGR attributes `vt100` drops.
//!
//! `vt100` 0.16's `Attrs` is `{ fgcolor, bgcolor, mode: u8 }` and its SGR
//! dispatch handles only `0 1 2 3 4 7 22 23 24 27` plus the colour params.
//! `5`/`6` (blink), `8` (conceal) and `9` (strikethrough), and their resets
//! `25`/`28`/`29`, never reach a cell — so three distinct renderings collapse
//! into one value, and a test asserting that a password field is masked
//! **passes against an application that prints the secret in clear**. That is
//! the one failure mode in this crate where a green test certifies the bug it
//! was written to catch.
//!
//! The fix is a second `vt100::Parser` fed the same byte stream with only SGR
//! sequences rewritten, so three attributes vt100 *does* keep act as carriers
//! for the three it drops:
//!
//! | dropped        | carrier     | off      |
//! |----------------|-------------|----------|
//! | `5`/`6` blink  | `1` bold    | `25`→`22`|
//! | `8` conceal    | `3` italic  | `28`→`23`|
//! | `9` strike     | `4` underline | `29`→`24`|
//!
//! Every other SGR parameter is dropped from the shadow stream, so nothing
//! else can disturb a carrier.
//!
//! **Why this is sound rather than clever.** In vt100, attributes never
//! influence geometry: `Attrs` is read in exactly two places — as the fill
//! value for `clear`/`erase`, and by the escape-code *output* functions.
//! Cursor movement, wrapping, scrolling, tabs and cell placement are entirely
//! attribute-independent, and SGR sequences never move the cursor. So a stream
//! that differs only by the replacement of complete plain-SGR sequences
//! produces an identically-shaped grid, and shadow cell `(r, c)` is primary
//! cell `(r, c)`. A debug assertion checks that correspondence on every
//! snapshot rather than leaving it to argument.
//!
//! This is deliberately *not* the option the issue behind this ruled out:
//! nothing here attributes styles to cells by hand. vt100 does the
//! attribution, twice — so there is no second cursor, no duplicated wrap,
//! scroll-region or alternate-screen logic, and nothing to diverge quietly.
//!
//! The honest alternative was vendoring a patched vt100: 3,950 lines of
//! someone else's code carried permanently to hold an 80-line patch. When
//! upstream gains the attributes, this module deletes and `convert_cell`
//! reads the three flags directly.
//!
//! Cost: one extra visible grid (no scrollback — history is text-only) and a
//! second parse pass. Not measurable end to end, since a terminal's
//! throughput is dominated by the PTY round trip rather than by parsing —
//! 40,000 lines through an 80x24 screen took 260ms with this and 263ms
//! without, and 284ms vs 282ms when every line carries four SGR sequences.

/// Where the rewriter is in the byte stream. Only enough to recognize a
/// complete plain SGR; everything else passes through untouched.
#[derive(Debug, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    Csi,
}

/// A parallel `vt100::Parser` carrying blink, conceal and strikethrough.
pub(super) struct AttrShadow {
    parser: ::vt100::Parser,
    state: State,
    /// Bytes held back while this might be a plain SGR. Always emitted in
    /// the end — rewritten if it is one, verbatim if it is not — so the
    /// shadow can never lose a byte that shapes the grid.
    pending: Vec<u8>,
}

impl AttrShadow {
    pub(super) fn new(rows: u16, cols: u16) -> Self {
        Self {
            // No scrollback: history is text-only, so the shadow is needed
            // for the visible grid alone.
            parser: ::vt100::Parser::new(rows, cols, 0),
            state: State::Ground,
            pending: Vec::new(),
        }
    }

    pub(super) fn set_size(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
    }

    /// The shadow cell at `(row, col)`, whose bold/italic/underline flags are
    /// the primary's blink/conceal/strikethrough.
    pub(super) fn cell(&self, row: u16, col: u16) -> Option<&::vt100::Cell> {
        self.parser.screen().cell(row, col)
    }

    /// Text of the shadow grid, for the correspondence check in `snapshot`.
    /// Not gated on `debug_assertions`: `debug_assert_eq!` still type-checks
    /// (and compiles) its arguments in release, where the branch is then
    /// eliminated.
    pub(super) fn contents(&self) -> String {
        self.parser.screen().contents()
    }

    pub(super) fn feed(&mut self, bytes: &[u8]) {
        let shadowed = self.rewrite_stream(bytes);
        self.parser.process(&shadowed);
    }

    /// The shadow form of `bytes`: identical except that complete plain SGR
    /// sequences are replaced by their carrier form. Held-back bytes carry
    /// over to the next call, so a sequence split across chunks is neither
    /// lost nor half-applied.
    fn rewrite_stream(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        for &b in bytes {
            // An ESC anywhere abandons whatever was being collected and
            // starts a new sequence, so the held bytes were not an SGR.
            if b == 0x1b {
                out.append(&mut self.pending);
                self.pending.push(b);
                self.state = State::Esc;
                continue;
            }
            match self.state {
                State::Ground => out.push(b),
                State::Esc => {
                    self.pending.push(b);
                    if b == b'[' {
                        self.state = State::Csi;
                    } else {
                        out.append(&mut self.pending);
                        self.state = State::Ground;
                    }
                }
                State::Csi => {
                    self.pending.push(b);
                    match b {
                        // Parameter and intermediate bytes: keep collecting.
                        0x20..=0x3f => {}
                        // Final byte: an SGR is rewritten, anything else
                        // passes through.
                        0x40..=0x7e => {
                            let seq = std::mem::take(&mut self.pending);
                            self.state = State::Ground;
                            match (b == b'm').then(|| rewrite_sgr(&seq)).flatten() {
                                Some(rewritten) => out.extend_from_slice(&rewritten),
                                None => out.extend_from_slice(&seq),
                            }
                        }
                        // A control byte inside a CSI (CAN, SUB, …). vte
                        // decides what that means; passing the bytes through
                        // unchanged means the shadow cannot diverge, at the
                        // cost of not rewriting this one sequence.
                        _ => {
                            out.append(&mut self.pending);
                            self.state = State::Ground;
                        }
                    }
                }
            }
        }
        out
    }
}

/// Rewrite one complete `ESC [ … m` into the carrier form.
///
/// `None` means "not a plain SGR — emit it verbatim": a private prefix
/// (`?`, `<`, `=`, `>`) or an intermediate byte makes it something else, and
/// guessing there is how a rewriter loses bytes that shape the grid.
/// `Some(bytes)` is what to emit, and may be empty when every parameter was
/// dropped.
fn rewrite_sgr(seq: &[u8]) -> Option<Vec<u8>> {
    // `ESC [ … m`
    let params = seq.get(2..seq.len().checked_sub(1)?)?;
    if params
        .iter()
        .any(|b| !matches!(b, b'0'..=b'9' | b';' | b':'))
    {
        return None;
    }

    /// The value vte would report for a parameter group: its first
    /// sub-parameter, with an empty group meaning zero.
    fn value(group: &[u8]) -> u32 {
        let first = group.split(|&b| b == b':').next().unwrap_or(b"");
        // Saturating: a parameter longer than u32 is not one we map.
        first.iter().fold(0u32, |acc, &d| {
            acc.saturating_mul(10).saturating_add(u32::from(d - b'0'))
        })
    }

    let groups: Vec<&[u8]> = params.split(|&b| b == b';').collect();
    let mut out: Vec<u32> = Vec::new();
    let mut i = 0;
    while i < groups.len() {
        let group = groups[i];
        let v = value(group);
        match v {
            // Extended colour. Its sub-parameters must be stepped over, or
            // the `5` in `38;5;196` reads as blink and paints a whole line
            // of text with an attribute the application never set.
            38 | 48 | 58 => {
                if group.contains(&b':') {
                    // Colon form (`38:2::r:g:b`) is one self-contained group.
                    i += 1;
                } else {
                    i += match groups.get(i + 1).map(|g| value(g)) {
                        Some(2) => 5, // 38;2;r;g;b
                        Some(5) => 3, // 38;5;n
                        _ => 1,       // malformed; skip the introducer only
                    };
                }
            }
            _ => {
                if let Some(carrier) = carrier(v) {
                    out.push(carrier);
                }
                i += 1;
            }
        }
    }

    let mut bytes = Vec::new();
    if !out.is_empty() {
        bytes.extend_from_slice(b"\x1b[");
        for (n, param) in out.iter().enumerate() {
            if n > 0 {
                bytes.push(b';');
            }
            bytes.extend_from_slice(param.to_string().as_bytes());
        }
        bytes.push(b'm');
    }
    Some(bytes)
}

/// The shadow parameter carrying `param`, if any.
fn carrier(param: u32) -> Option<u32> {
    match param {
        0 => Some(0),     // reset all: means the same in both streams
        5 | 6 => Some(1), // blink (slow, rapid) -> bold
        8 => Some(3),     // conceal -> italic
        9 => Some(4),     // strikethrough -> underline
        25 => Some(22),   // blink off -> normal intensity
        28 => Some(23),   // conceal off -> italic off
        29 => Some(24),   // strikethrough off -> underline off
        _ => None,        // everything else belongs to the primary alone
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real rewriter's output, as a readable string.
    fn shadowed(bytes: &[u8]) -> String {
        let mut shadow = AttrShadow::new(4, 20);
        let mut out = shadow.rewrite_stream(bytes);
        // Anything still held at the end of the stream is not an SGR.
        out.append(&mut shadow.pending);
        String::from_utf8_lossy(&out).replace('\x1b', "E")
    }

    #[test]
    fn the_three_dropped_attributes_get_carriers() {
        assert_eq!(shadowed(b"\x1b[5mX"), "E[1mX"); // blink -> bold
        assert_eq!(shadowed(b"\x1b[6mX"), "E[1mX"); // rapid blink too
        assert_eq!(shadowed(b"\x1b[8mX"), "E[3mX"); // conceal -> italic
        assert_eq!(shadowed(b"\x1b[9mX"), "E[4mX"); // strike -> underline
        assert_eq!(shadowed(b"\x1b[25m"), "E[22m");
        assert_eq!(shadowed(b"\x1b[28m"), "E[23m");
        assert_eq!(shadowed(b"\x1b[29m"), "E[24m");
    }

    #[test]
    fn the_primarys_own_attributes_are_dropped_from_the_shadow() {
        // Otherwise a real bold would read as a blink.
        assert_eq!(shadowed(b"\x1b[1mX"), "X");
        assert_eq!(shadowed(b"\x1b[3mX"), "X");
        assert_eq!(shadowed(b"\x1b[4mX"), "X");
        assert_eq!(shadowed(b"\x1b[7;31;44mX"), "X");
        // …including the resets, which would clear a carrier.
        assert_eq!(shadowed(b"\x1b[22m\x1b[23m\x1b[24m\x1b[27m"), "");
        // Reset-all means the same in both streams.
        assert_eq!(shadowed(b"\x1b[0mX"), "E[0mX");
        assert_eq!(shadowed(b"\x1b[mX"), "E[0mX");
    }

    #[test]
    fn mixed_parameters_keep_only_the_carriers_in_order() {
        assert_eq!(shadowed(b"\x1b[1;5;31mX"), "E[1mX");
        assert_eq!(shadowed(b"\x1b[0;9;1;8mX"), "E[0;4;3mX");
    }

    #[test]
    fn an_extended_colour_never_looks_like_a_carrier() {
        // The trap: the `5` in `38;5;196` selects palette mode, not blink.
        // Reading it as blink would paint a whole run with an attribute the
        // application never set.
        assert_eq!(shadowed(b"\x1b[38;5;196mX"), "X");
        assert_eq!(shadowed(b"\x1b[48;5;9mX"), "X");
        assert_eq!(shadowed(b"\x1b[38;2;255;0;8mX"), "X"); // the 8 is blue, not conceal
        assert_eq!(shadowed(b"\x1b[38;2;0;9;0;5mX"), "E[1mX"); // …trailing 5 IS blink
                                                               // Colon form is one self-contained parameter group.
        assert_eq!(shadowed(b"\x1b[38:5:196mX"), "X");
        assert_eq!(shadowed(b"\x1b[38:2::255:0:9mX"), "X");
        assert_eq!(shadowed(b"\x1b[4:3mX"), "X"); // curly underline, not strike
                                                  // A carrier still survives alongside one.
        assert_eq!(shadowed(b"\x1b[38;5;196;9mX"), "E[4mX");
    }

    #[test]
    fn anything_that_is_not_a_plain_sgr_passes_through_verbatim() {
        // Byte-for-byte passthrough is what keeps the shadow's geometry
        // identical to the primary's: only complete plain SGRs are touched.
        for seq in [
            &b"\x1b[?2026h"[..],
            &b"\x1b[2J"[..],
            &b"\x1b[H"[..],
            &b"\x1b[3;7Hhi"[..],
            &b"\x1b[?25l"[..],
            &b"\x1b[>4;2m"[..], // private-prefix 'm': XTMODKEYS, not SGR
            &b"\x1b[4$p"[..],   // intermediate byte
            &b"\x1b]0;title\x07"[..],
            &b"\x1b7\x1b8"[..],
            &b"plain text\r\n\t"[..],
        ] {
            assert_eq!(
                shadowed(seq),
                String::from_utf8_lossy(seq).replace('\x1b', "E"),
                "sequence {:?} must pass through untouched",
                String::from_utf8_lossy(seq)
            );
        }
    }

    #[test]
    fn a_sequence_split_across_feeds_is_not_lost() {
        let mut shadow = AttrShadow::new(2, 10);
        shadow.feed(b"\x1b[8");
        // Mid-sequence: nothing applied yet, exactly as the primary sees it.
        assert!(!shadow.cell(0, 0).is_some_and(::vt100::Cell::italic));
        shadow.feed(b"mX");
        assert!(
            shadow.cell(0, 0).is_some_and(::vt100::Cell::italic),
            "the conceal carrier must survive a chunk boundary"
        );
    }

    #[test]
    fn an_aborted_csi_keeps_its_bytes() {
        // CAN inside a CSI: vte decides what it means, and passing the bytes
        // through unchanged is what guarantees we cannot diverge.
        assert_eq!(shadowed(b"\x1b[31\x18X"), "E[31\u{18}X");
        // A fresh sequence afterwards is still rewritten.
        assert_eq!(shadowed(b"\x1b[31\x18\x1b[9mX"), "E[31\u{18}E[4mX");
        // An ESC inside a CSI abandons it and starts over.
        assert_eq!(shadowed(b"\x1b[31\x1b[9mX"), "E[31E[4mX");
    }
}
