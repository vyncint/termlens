//! Recovering the three SGR attributes `vt100` drops.
//!
//! `vt100`'s `Attrs` (0.16 through 0.18) is `{ fgcolor, bgcolor, mode: u8 }`
//! and its SGR dispatch handles only `0 1 2 3 4 7 22 23 24 27` plus the
//! colour params.
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
    /// Inside a plain CSI longer than [`CSI_HOLD_MAX`], whose parameters
    /// are held in vte's own form from there on — see [`LongParams`].
    CsiLong(LongParams),
}

/// The most bytes of one CSI a rewriter holds as written, from its `ESC [`
/// on, while it waits for the final byte that says whether it is an SGR.
///
/// The whole sequence used to be held, so a CSI that no final byte ended
/// grew `pending` with the child's output in both rewriters, about three
/// bytes of memory for each byte written (#730). Past this a CSI that is
/// still plain digits and separators is held as vte holds it, and any
/// other passes through as it came. No SGR an application writes comes
/// near it: 32 parameters of three digits each, the most vte reads, are
/// 130 bytes.
const CSI_HOLD_MAX: usize = 256;

/// The parameter values vte stores for one CSI. It ignores every digit and
/// separator after them, and vt100 dispatches the sequence on those it
/// kept.
const VTE_MAX_PARAMS: usize = 32;

/// A byte of a plain SGR's parameters: a digit, or the separator of two
/// parameters or of two sub-parameters. Anything else between `ESC [` and
/// the `m` (a private prefix, an intermediate) makes the sequence
/// something other than an SGR.
fn plain_param(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b';' | b':')
}

/// A plain CSI past [`CSI_HOLD_MAX`], read as vte reads it: each value
/// saturated at `u16::MAX`, and nothing after the 32nd. What stays in
/// `pending` is the same parameters written short (`ESC [`, then each
/// stored value and the separator after it), so the parser reads the
/// sequence the application wrote and the rewrite sees the values the
/// parser will, in at most 193 bytes however long the sequence runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LongParams {
    /// Values vte has stored: one for each separator so far.
    stored: usize,
    /// The value being read, or `None` before its first digit.
    current: Option<u16>,
}

impl LongParams {
    /// Rewrite `pending`, a plain CSI as written, into the short form.
    fn compact(pending: &mut Vec<u8>) -> Self {
        let params = pending.split_off(2);
        let mut long = Self {
            stored: 0,
            current: None,
        };
        for b in params {
            long.step(pending, b);
        }
        long
    }

    /// Read one more plain parameter byte.
    fn step(&mut self, pending: &mut Vec<u8>, b: u8) {
        if self.stored == VTE_MAX_PARAMS {
            return;
        }
        if b.is_ascii_digit() {
            let value = self.current.unwrap_or(0);
            self.current = Some(value.saturating_mul(10).saturating_add(u16::from(b - b'0')));
            return;
        }
        self.write_current(pending);
        self.stored += 1;
        // The separator after the 32nd value is left off: without it the
        // final byte stores that value instead, the same parameters
        // either way, where `rewrite_sgr` would read one more, empty, and
        // take it for a reset.
        if self.stored < VTE_MAX_PARAMS {
            pending.push(b);
        }
    }

    /// The short form complete, for the final byte to follow: the value
    /// being read written after the stored ones, unless vte has no room
    /// left for it.
    fn finish(mut self, pending: &mut Vec<u8>) {
        if self.stored < VTE_MAX_PARAMS {
            self.write_current(pending);
        }
    }

    /// The short form left for the parser to read the rest of the sequence
    /// as it comes. With all 32 values stored, the separator that
    /// [`step`](Self::step) left off goes back on, so the parser has no
    /// room left either and ignores the digits that follow, as it would
    /// have; without it they would be appended to the 32nd value.
    fn flush(self, pending: &mut Vec<u8>) {
        if self.stored == VTE_MAX_PARAMS {
            pending.push(b';');
        } else {
            self.finish(pending);
        }
    }

    fn write_current(&mut self, pending: &mut Vec<u8>) {
        if let Some(value) = self.current.take() {
            pending.extend_from_slice(value.to_string().as_bytes());
        }
    }
}

/// Rewrites the SGR colour forms `vt100` misreads into ones it reads right:
/// colon-form extended colours into the semicolon form it understands, and
/// a semicolon-form underline colour into one colon parameter it skips.
/// Like [`AttrShadow`], it only holds a complete plain SGR and emits every
/// other sequence unchanged.
pub(super) struct ColorNormalizer {
    state: State,
    pending: Vec<u8>,
}

impl ColorNormalizer {
    pub(super) fn new() -> Self {
        Self {
            state: State::Ground,
            pending: Vec::new(),
        }
    }

    pub(super) fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        rewrite_stream(
            &mut self.state,
            &mut self.pending,
            bytes,
            normalize_for_grid,
        )
    }
}

/// A parallel `vt100::Parser` carrying blink, conceal and strikethrough.
pub(super) struct AttrShadow {
    parser: ::vt100::Parser,
    state: State,
    /// Bytes held back while this might be a plain SGR. Always emitted in
    /// the end — rewritten if it is one, verbatim if it is not — so the
    /// shadow can never lose a byte that shapes the grid. Never more than
    /// [`CSI_HOLD_MAX`] of them.
    pending: Vec<u8>,
}

impl AttrShadow {
    /// `scrollback` is the primary's. Styled history reads the shadow's
    /// scrolled rows in lockstep with the primary's (#146), and even without
    /// it the shadow's grid needs the same history: a resize that makes the
    /// screen taller pulls rows back out of history onto the grid, and the
    /// shadow must get the same rows back or the two grids diverge.
    pub(super) fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            parser: ::vt100::Parser::new(
                super::vt100::nz(rows),
                super::vt100::nz(cols),
                scrollback,
            ),
            state: State::Ground,
            pending: Vec::new(),
        }
    }

    /// Move the shadow's history view, exactly as the primary's is moved
    /// while scrolled rows are captured; always restored to 0 afterwards.
    pub(super) fn set_scrollback(&mut self, rows: usize) {
        self.parser.screen_mut().set_scrollback(rows);
    }

    /// Resized as the primary is, so the grids stay in step — see
    /// [`resize_as_xterm`](super::vt100::resize_as_xterm).
    pub(super) fn set_size(&mut self, rows: u16, cols: u16, between_sequences: bool) {
        super::vt100::resize_as_xterm(&mut self.parser, rows, cols, between_sequences);
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

    /// The bytes that draw `contents` with the cursor moved by `at`, in the
    /// attributes the shadow's cell `(row, col)` holds now, then set the
    /// shadow's current attributes back — the shadow's half of redrawing a
    /// cell as it stands. Its carriers are its only attributes, since every
    /// other SGR parameter is dropped on the way in.
    pub(super) fn redraw(&self, row: u16, col: u16, at: &str, contents: &str) -> String {
        let screen = self.parser.screen();
        let mut sgr = String::from("\x1b[0");
        if let Some(cell) = screen.cell(row, col) {
            for (on, param) in [
                (cell.bold(), ";1"),
                (cell.italic(), ";3"),
                (cell.underline(), ";4"),
            ] {
                if on {
                    sgr.push_str(param);
                }
            }
        }
        sgr.push('m');
        format!("{at}{sgr}{contents}{}", screen.attributes_formatted())
    }

    /// Feed bytes already in the shadow's own form, carriers and all, past
    /// the rewrite: what [`redraw`](Self::redraw) builds.
    pub(super) fn feed_verbatim(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// The shadow form of `bytes`: identical except that complete plain SGR
    /// sequences are replaced by their carrier form. Held-back bytes carry
    /// over to the next call, so a sequence split across chunks is neither
    /// lost nor half-applied.
    fn rewrite_stream(&mut self, bytes: &[u8]) -> Vec<u8> {
        rewrite_stream(&mut self.state, &mut self.pending, bytes, rewrite_sgr)
    }
}

/// `bytes` with every complete plain SGR replaced by what `rewrite` makes
/// of it, and everything else as it came, but for the parameters of a CSI
/// past [`CSI_HOLD_MAX`], which are written short ([`LongParams`]).
fn rewrite_stream(
    state: &mut State,
    pending: &mut Vec<u8>,
    bytes: &[u8],
    rewrite: fn(&[u8]) -> Option<Vec<u8>>,
) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    for &b in bytes {
        // An ESC anywhere abandons whatever was being collected and starts a
        // new sequence, so the held bytes were not an SGR.
        if b == 0x1b {
            if let State::CsiLong(long) = *state {
                long.flush(pending);
            }
            out.append(pending);
            pending.push(b);
            *state = State::Esc;
            continue;
        }
        match state {
            State::Ground => out.push(b),
            State::Esc => {
                pending.push(b);
                if b == b'[' {
                    *state = State::Csi;
                } else {
                    out.append(pending);
                    *state = State::Ground;
                }
            }
            State::Csi => {
                pending.push(b);
                match b {
                    // Parameter and intermediate bytes: keep collecting, as
                    // far as the bound.
                    0x20..=0x3f => {
                        if pending.len() >= CSI_HOLD_MAX {
                            if pending[2..].iter().all(|&b| plain_param(b)) {
                                *state = State::CsiLong(LongParams::compact(pending));
                            } else {
                                // Not an SGR, whatever ends it: the rest
                                // passes through as it comes.
                                out.append(pending);
                                *state = State::Ground;
                            }
                        }
                    }
                    // Final byte: an SGR is rewritten, anything else passes
                    // through.
                    0x40..=0x7e => {
                        *state = State::Ground;
                        dispatch(pending, b, rewrite, &mut out);
                    }
                    // A control byte inside a CSI (CAN, SUB, …). vte decides
                    // what that means; preserve it rather than guessing.
                    _ => {
                        out.append(pending);
                        *state = State::Ground;
                    }
                }
            }
            State::CsiLong(long) => {
                if plain_param(b) {
                    long.step(pending, b);
                    continue;
                }
                let long = *long;
                *state = State::Ground;
                if (0x40..=0x7e).contains(&b) {
                    long.finish(pending);
                    pending.push(b);
                    dispatch(pending, b, rewrite, &mut out);
                } else {
                    // As in a CSI held whole: what was read goes out, and the
                    // rest of the sequence passes through as it comes. The
                    // parser is in the same state either way, since the
                    // short form holds the same parameters.
                    long.flush(pending);
                    pending.push(b);
                    out.append(pending);
                }
            }
        }
    }
    out
}

/// Emit the CSI in `pending`, whose final byte is `fin`: rewritten if it is
/// an SGR `rewrite` has a form for, as it stands otherwise.
fn dispatch(
    pending: &mut Vec<u8>,
    fin: u8,
    rewrite: fn(&[u8]) -> Option<Vec<u8>>,
    out: &mut Vec<u8>,
) {
    let seq = std::mem::take(pending);
    match (fin == b'm').then(|| rewrite(&seq)).flatten() {
        Some(rewritten) => out.extend_from_slice(&rewritten),
        None => out.extend_from_slice(&seq),
    }
}

/// One plain SGR in the form `vt100` reads right, or `None` to pass it
/// through unchanged.
///
/// Two forms need it. Colon-form extended colours (`38:2::r:g:b`) become
/// the semicolon form. And an underline colour (`58`), which vt100 does not
/// model, in the semicolon form (`58;5;n`, `58;2;r;g;b`) becomes one colon
/// parameter (`58:5:n`): vt100 reads parameters one at a time, so it took
/// the arguments for attributes — `58;5;1` was blink and bold, and
/// `58;5;0` a reset. As one parameter it is skipped whole, and the
/// sequence is still recorded as one nobody honoured (#579). crossterm
/// writes the semicolon form for `SetUnderlineColor`.
fn normalize_for_grid(seq: &[u8]) -> Option<Vec<u8>> {
    let params = seq.get(2..seq.len().checked_sub(1)?)?;
    if !params.iter().all(|&b| plain_param(b)) {
        return None;
    }
    // The common case, checked without allocating: neither form is here.
    if !params.contains(&b':') && !params.split(|&b| b == b';').any(|group| group == b"58") {
        return None;
    }

    let groups: Vec<&[u8]> = params.split(|&b| b == b';').collect();
    let mut normalized = Vec::with_capacity(groups.len());
    let mut changed = false;
    let mut next = 0;
    while next < groups.len() {
        let group = groups[next];
        next += 1;
        if group == b"58" {
            let arguments = match groups.get(next).copied() {
                Some(b"5") => 2,
                Some(b"2") => 4,
                _ => 0,
            };
            if arguments > 0 {
                let end = (next + arguments).min(groups.len());
                let mut joined = group.to_vec();
                for argument in &groups[next..end] {
                    joined.push(b':');
                    joined.extend_from_slice(argument);
                }
                normalized.push(joined);
                next = end;
                changed = true;
                continue;
            }
        }
        let parts: Vec<&[u8]> = group.split(|&b| b == b':').collect();
        let replacement = match parts.as_slice() {
            [first @ (b"38" | b"48"), b"2", b"", red, green, blue]
            | [first @ (b"38" | b"48"), b"2", red, green, blue] => {
                Some(vec![*first, b"2", *red, *green, *blue])
            }
            [first @ (b"38" | b"48"), b"5", index] => Some(vec![*first, b"5", *index]),
            _ => None,
        };
        if let Some(replacement) = replacement {
            normalized.extend(replacement.into_iter().map(<[u8]>::to_vec));
            changed = true;
        } else {
            normalized.push(group.to_vec());
        }
    }
    if !changed {
        return None;
    }

    let mut out = Vec::with_capacity(seq.len());
    out.extend_from_slice(b"\x1b[");
    for (index, group) in normalized.iter().enumerate() {
        if index > 0 {
            out.push(b';');
        }
        out.extend_from_slice(group);
    }
    out.push(b'm');
    Some(out)
}

/// Rewrite one complete `ESC [ … m` into the carrier form.
///
/// `None` means "not a plain SGR — emit it verbatim": a private prefix
/// (`?`, `<`, `=`, `>`) or an intermediate byte makes it something else, and
/// guessing there is how a rewriter loses bytes that shape the grid.
/// `Some(bytes)` is what to emit, always a complete sequence: when every
/// parameter was dropped the surrogate `ESC[39m` stands in so the sequence's
/// ESC survives (#390).
fn rewrite_sgr(seq: &[u8]) -> Option<Vec<u8>> {
    // `ESC [ … m`
    let params = seq.get(2..seq.len().checked_sub(1)?)?;
    if !params.iter().all(|&b| plain_param(b)) {
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
            // A group with a sub-parameter (`9:1`, `0:1`) carries nothing,
            // not even a reset. xterm reads sub-parameters only on 38 and
            // 48 and skips any other group that has one, and vt100 matches
            // no such group, so the primary does nothing either: carrying
            // the first value put an attribute on the cell that no terminal
            // shows, beside the record saying the sequence was dropped
            // (#688).
            _ if group.contains(&b':') => i += 1,
            _ => {
                if let Some(carrier) = carrier(v) {
                    out.push(carrier);
                }
                i += 1;
            }
        }
    }

    let mut bytes = Vec::new();
    if out.is_empty() {
        // #390: the ESC introducing this SGR may also be doing structural
        // work in the primary stream — terminating an unterminated OSC,
        // aborting a half-written CSI — so it must not disappear, or the
        // shadow's parser swallows the next printable byte as a final one.
        // `39` resets only the foreground colour, which no shadow cell is
        // ever read for; `0` would clear a bold/italic/underline carrier
        // mid-run and with it a blink, conceal or strikethrough.
        bytes.extend_from_slice(b"\x1b[39m");
        return Some(bytes);
    }
    bytes.extend_from_slice(b"\x1b[");
    for (n, param) in out.iter().enumerate() {
        if n > 0 {
            bytes.push(b';');
        }
        bytes.extend_from_slice(param.to_string().as_bytes());
    }
    bytes.push(b'm');
    Some(bytes)
}

/// The shadow parameter carrying `param`, if any.
///
/// Every `Some` here except `0` is a parameter vt100 drops and this parser
/// recovers, so `unhandled::SHADOW_SGR_PARAMS` must list exactly those —
/// otherwise `Screen::unsupported()` names a sequence whose effect is on
/// the cell (#320). A test in this module holds the two in step.
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

    /// The two lists that describe the same set, held together (#320).
    ///
    /// `carrier` decides what this parser recovers; `SHADOW_SGR_PARAMS`
    /// decides what the unhandled tracker declines to record. They were
    /// written apart and can drift apart, and the drift is silent in both
    /// directions: a parameter added here and not there is named as
    /// unimplemented while its attribute reaches the cell, and one added
    /// there and not here is dropped from the record while nothing carries
    /// it.
    #[test]
    fn the_recovered_parameters_are_exactly_what_the_tracker_declines() {
        let recovered: Vec<u16> = (0..=255u32)
            .filter(|&p| p != 0 && carrier(p).is_some())
            .map(|p| p as u16)
            .collect();
        let mut declined = crate::emu::unhandled::SHADOW_SGR_PARAMS.to_vec();
        declined.sort_unstable();
        assert_eq!(
            recovered, declined,
            "shadow::carrier recovers {recovered:?}, unhandled declines {declined:?}"
        );
    }

    /// The real rewriter's output, as a readable string.
    fn shadowed(bytes: &[u8]) -> String {
        let mut shadow = AttrShadow::new(4, 20, 0);
        let mut out = shadow.rewrite_stream(bytes);
        // Anything still held at the end of the stream is not an SGR.
        out.append(&mut shadow.pending);
        String::from_utf8_lossy(&out).replace('\x1b', "E")
    }

    /// The primary rewriter's output, as a readable string.
    fn normalized(bytes: &[u8]) -> String {
        let mut colors = ColorNormalizer::new();
        let mut out = colors.feed(bytes);
        // Anything still held at the end of the stream is not a complete SGR.
        out.append(&mut colors.pending);
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
        // Otherwise a real bold would read as a blink. A sequence left with
        // nothing keeps only the surrogate `39` (#390): the parameter is
        // still dropped, the ESC survives.
        assert_eq!(shadowed(b"\x1b[1mX"), "E[39mX");
        assert_eq!(shadowed(b"\x1b[3mX"), "E[39mX");
        assert_eq!(shadowed(b"\x1b[4mX"), "E[39mX");
        assert_eq!(shadowed(b"\x1b[7;31;44mX"), "E[39mX");
        // …including the resets, which would clear a carrier.
        assert_eq!(
            shadowed(b"\x1b[22m\x1b[23m\x1b[24m\x1b[27m"),
            "E[39mE[39mE[39mE[39m"
        );
        // Reset-all means the same in both streams.
        assert_eq!(shadowed(b"\x1b[0mX"), "E[0mX");
        assert_eq!(shadowed(b"\x1b[mX"), "E[0mX");
    }

    #[test]
    fn mixed_parameters_keep_only_the_carriers_in_order() {
        assert_eq!(shadowed(b"\x1b[1;5;31mX"), "E[1mX");
        assert_eq!(shadowed(b"\x1b[0;9;1;8mX"), "E[0;4;3mX");
    }

    /// #390: a sequence left with no shadow parameter must still emit one.
    ///
    /// The rewrite is not only about the attributes: the ESC introducing it
    /// may also be terminating an unterminated OSC or aborting a half-written
    /// CSI in the primary stream, and emitting zero bytes would strip that ESC
    /// from the shadow stream too. The shadow's parser would then consume the
    /// next printable byte as a final byte, and the two grids diverge.
    #[test]
    fn a_fully_dropped_sgr_still_emits_a_complete_sequence() {
        for seq in [
            &b"\x1b[31m"[..],
            &b"\x1b[7m"[..],
            &b"\x1b[38;5;1m"[..],
            &b"\x1b[1;2m"[..],
        ] {
            let rewritten = rewrite_sgr(seq).expect("a complete plain SGR");
            assert!(
                rewritten.starts_with(b"\x1b[") && rewritten.ends_with(b"m"),
                "{seq:?} rewrote to {rewritten:?}, which is not a complete sequence"
            );
        }
        assert_eq!(shadowed(b"\x1b[31mX"), "E[39mX", "the surrogate is emitted");
    }

    /// The module's own invariant, on the two payloads from the issue: the
    /// shadow parser and a primary parser fed the same stream must agree.
    #[test]
    fn a_dropped_sgr_keeps_a_structural_esc_in_step() {
        for payload in [
            &b"\x1b]0;title\x1b[31mA\x1b[5mB\x1b[0mC"[..],
            &b"\x1b[1;2\x1b[31mA"[..],
        ] {
            let mut primary =
                ::vt100::Parser::new(super::super::vt100::nz(4), super::super::vt100::nz(20), 0);
            let mut shadow = AttrShadow::new(4, 20, 0);
            primary.process(payload);
            shadow.feed(payload);
            assert_eq!(
                primary.screen().contents(),
                shadow.contents(),
                "payload {payload:?}"
            );
        }
    }

    #[test]
    fn an_extended_colour_never_looks_like_a_carrier() {
        // The trap: the `5` in `38;5;196` selects palette mode, not blink.
        // Reading it as blink would paint a whole run with an attribute the
        // application never set. A colour-only sequence keeps no carrier, so
        // it leaves only the `39` surrogate (#390).
        assert_eq!(shadowed(b"\x1b[38;5;196mX"), "E[39mX");
        assert_eq!(shadowed(b"\x1b[48;5;9mX"), "E[39mX");
        assert_eq!(shadowed(b"\x1b[38;2;255;0;8mX"), "E[39mX"); // the 8 is blue, not conceal
        assert_eq!(shadowed(b"\x1b[38;2;0;9;0;5mX"), "E[1mX"); // …trailing 5 IS blink
                                                               // Colon form is one self-contained parameter group.
        assert_eq!(shadowed(b"\x1b[38:5:196mX"), "E[39mX");
        assert_eq!(shadowed(b"\x1b[38:2::255:0:9mX"), "E[39mX");
        assert_eq!(shadowed(b"\x1b[4:3mX"), "E[39mX"); // curly underline, not strike
                                                       // A carrier still survives alongside one.
        assert_eq!(shadowed(b"\x1b[38;5;196;9mX"), "E[4mX");
    }

    #[test]
    fn a_group_with_a_sub_parameter_carries_nothing() {
        // xterm skips a group that has a sub-parameter unless it is 38 or
        // 48, and vt100 matches none; the first value used to be carried
        // as if the group were plain (#688).
        assert_eq!(shadowed(b"\x1b[5:1mX"), "E[39mX");
        assert_eq!(shadowed(b"\x1b[6:1mX"), "E[39mX");
        assert_eq!(shadowed(b"\x1b[8:0mX"), "E[39mX");
        assert_eq!(shadowed(b"\x1b[9:1mX"), "E[39mX");
        // Not a reset either: vt100 carries none out for `0:1`, and the
        // "off" forms would clear a carrier the primary still shows.
        assert_eq!(shadowed(b"\x1b[0:1mX"), "E[39mX");
        assert_eq!(shadowed(b"\x1b[25:1;28:0;29:1mX"), "E[39mX");
        // A plain parameter beside one is still carried, in order.
        assert_eq!(shadowed(b"\x1b[9;5:1;8mX"), "E[4;3mX");
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
    fn a_semicolon_underline_colour_becomes_one_colon_parameter() {
        // vt100 reads parameters one at a time and has no SGR 58, so its
        // arguments must not reach it as parameters of their own (#579).
        assert_eq!(normalized(b"\x1b[58;5;1m"), "E[58:5:1m");
        assert_eq!(normalized(b"\x1b[4;58;2;0;1;7;31m"), "E[4;58:2:0:1:7;31m");
        // Truncated: what is there is still one parameter.
        assert_eq!(normalized(b"\x1b[58;5m"), "E[58:5m");
        // Already one parameter, a 58 with no colour form after it, and a
        // reset of the underline colour: nothing to group.
        for seq in [
            &b"\x1b[58:5:1m"[..],
            b"\x1b[58m",
            b"\x1b[58;3m",
            b"\x1b[59m",
            b"\x1b[1;5m",
        ] {
            assert_eq!(
                normalized(seq),
                String::from_utf8_lossy(seq).replace('\x1b', "E")
            );
        }
    }

    #[test]
    fn colon_colour_normalization_preserves_unrecognized_bytes() {
        assert_eq!(normalized(b"\x1b[38:2::10:20:30m"), "E[38;2;10;20;30m");
        assert_eq!(normalized(b"\x1b[38:5:196m"), "E[38;5;196m");
        assert_eq!(normalized(b"\x1b[1;38:2::4:5:6;3m"), "E[1;38;2;4;5;6;3m");

        for seq in [
            &b"\x1b[4:3m"[..],
            &b"\x1b[38:2::9m"[..],
            &b"\x1b[38:2:1:10:20:30m"[..],
            &b"\x1b[?25l"[..],
            &b"\x1b]8;;http://x/\x1b\\"[..],
        ] {
            assert_eq!(
                normalized(seq),
                String::from_utf8_lossy(seq).replace('\x1b', "E"),
                "sequence {:?} must pass through untouched",
                String::from_utf8_lossy(seq)
            );
        }
    }

    /// A CSI of `len` bytes in all: `ESC [`, a run of `0`, then `tail`,
    /// whose last byte is the final one. The zeros pad the first parameter
    /// of `tail`, which keeps its value.
    fn padded_csi(len: usize, tail: &[u8]) -> Vec<u8> {
        let mut seq = b"\x1b[".to_vec();
        seq.resize(len - tail.len(), b'0');
        seq.extend_from_slice(tail);
        assert_eq!(seq.len(), len);
        seq
    }

    fn readable(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).replace('\x1b', "E")
    }

    #[test]
    fn a_csi_as_long_as_the_hold_bound_is_held_as_written() {
        let strike = padded_csi(CSI_HOLD_MAX, b"9m");
        assert_eq!(shadowed(&strike), "E[4m");
        let palette = padded_csi(CSI_HOLD_MAX, b"1;38:5:196m");
        let zeros = "0".repeat(CSI_HOLD_MAX - "E[1;38:5:196m".len());
        assert_eq!(normalized(&palette), format!("E[{zeros}1;38;5;196m"));
        let cup = padded_csi(CSI_HOLD_MAX, b"2;4H");
        assert_eq!(shadowed(&cup), readable(&cup));
    }

    #[test]
    fn a_plain_csi_past_the_hold_bound_is_rewritten_from_vtes_values() {
        // One byte past the bound the parameters are held as vte holds
        // them (#730), and both rewriters work on that short form: the
        // zeros padding a value are gone and the value is what it was.
        let long = CSI_HOLD_MAX + 1;
        assert_eq!(shadowed(&padded_csi(long, b"1;9m")), "E[4m");
        assert_eq!(normalized(&padded_csi(long, b"1;9m")), "E[1;9m");
        let palette = padded_csi(long, b"1;38:5:196m");
        assert_eq!(normalized(&palette), "E[1;38;5;196m");
        assert_eq!(shadowed(&palette), "E[39m");
        let truecolor = padded_csi(long, b";38:2::1:2:3m");
        assert_eq!(normalized(&truecolor), "E[0;38;2;1;2;3m");
        // Not an SGR: the same parameters, the same final byte.
        assert_eq!(shadowed(&padded_csi(long, b"2;4H")), "E[2;4H");
        assert_eq!(normalized(&padded_csi(long, b"2;4H")), "E[2;4H");
        // A value saturates where vte's does.
        let huge = padded_csi(long, b"99999999;5m");
        assert_eq!(normalized(&huge), "E[65535;5m");
        assert_eq!(shadowed(&huge), "E[1m");
    }

    #[test]
    fn a_long_csi_keeps_only_the_32_values_vte_stores() {
        // vte stores 32 values and ignores every digit and separator after
        // them, so the blink and the reset written past those are nobody's.
        let mut seq = padded_csi(CSI_HOLD_MAX, b"9");
        seq.extend_from_slice(";1".repeat(31).as_bytes());
        seq.extend_from_slice(b";5;0:7;22m");
        let kept = format!("9{}", ";1".repeat(31));
        assert_eq!(normalized(&seq), format!("E[{kept}m"));
        assert_eq!(shadowed(&seq), "E[4m");
        // Split at the separator after the 32nd value, and one byte at a
        // time, it reads the same.
        let mut shadow = AttrShadow::new(4, 20, 0);
        let mut out = Vec::new();
        for b in &seq {
            out.extend(shadow.rewrite_stream(std::slice::from_ref(b)));
        }
        assert_eq!(readable(&out), "E[4m");
    }

    #[test]
    fn a_csi_past_the_hold_bound_that_is_not_plain_passes_through() {
        let long = CSI_HOLD_MAX + 40;
        // Shaped otherwise before the bound: every byte as it came.
        for seq in [
            [&b"\x1b[>"[..], &padded_csi(long, b"4m")[2..]].concat(),
            [&b"\x1b[ "[..], &padded_csi(long, b"4m")[2..]].concat(),
        ] {
            assert_eq!(shadowed(&seq), readable(&seq));
            assert_eq!(normalized(&seq), readable(&seq));
        }
        // Shaped otherwise after it: what was read in the short form, then
        // the rest as it came. An intermediate makes it no SGR; CAN aborts
        // it, and its `m` is a letter; an ESC starts the next sequence,
        // which is held and rewritten as ever.
        for (tail, expected) in [
            (&b"4 m"[..], "E[4 m"),
            (b"4$m", "E[4$m"),
            (b"4\x18m", "E[4\x18m"),
            (b"4\x1b[9mX", "E[4E[4mX"),
        ] {
            assert_eq!(shadowed(&padded_csi(long, tail)), expected, "{tail:?}");
        }
    }

    #[test]
    fn an_unterminated_csi_is_held_only_as_far_as_the_bound() {
        // A megabyte of parameter bytes and no final byte, in reads of one
        // byte, of seven, and of the reader's 8 KiB: neither rewriter holds
        // more than the bound at any point (#730), and the split changes
        // nothing. The value saturates, as vte's does.
        let mut stream = b"\x1b[".to_vec();
        stream.resize(1 << 20, b'1');
        stream.extend_from_slice(b"mX\x1b[9mY");
        for read in [1, 7, 8192, stream.len()] {
            let mut shadow = AttrShadow::new(4, 20, 0);
            let mut colors = ColorNormalizer::new();
            let (mut shadowed, mut normalized) = (Vec::new(), Vec::new());
            for chunk in stream.chunks(read) {
                shadowed.extend(shadow.rewrite_stream(chunk));
                normalized.extend(colors.feed(chunk));
                for pending in [&shadow.pending, &colors.pending] {
                    assert!(pending.len() <= CSI_HOLD_MAX, "{}", pending.len());
                    assert!(pending.capacity() <= 2 * CSI_HOLD_MAX);
                }
            }
            assert_eq!(readable(&shadowed), "E[39mXE[4mY", "reads of {read}");
            assert_eq!(readable(&normalized), "E[65535mXE[9mY", "reads of {read}");
        }
    }

    #[test]
    fn a_long_csi_left_to_the_parser_leaves_it_as_full_as_it_was() {
        // A BEL inside the CSI is executed and the sequence goes on, past
        // the rewriters, to the parser. With 32 values stored vte ignores
        // the `0` after it, so the 32nd value is still the italic `3`; the
        // short form must leave the parser as full, or the `0` is appended
        // to that value and makes it `30`, a foreground colour.
        let mut seq = padded_csi(CSI_HOLD_MAX, b"0");
        seq.extend_from_slice(";0".repeat(30).as_bytes());
        seq.extend_from_slice(b";3;\x070mX");
        let italic = |bytes: &[u8]| {
            let mut parser =
                ::vt100::Parser::new(super::super::vt100::nz(2), super::super::vt100::nz(10), 0);
            parser.process(bytes);
            parser.screen().cell(0, 0).unwrap().italic()
        };
        assert!(italic(&seq));
        for split in [1, seq.len()] {
            let mut colors = ColorNormalizer::new();
            let mut shadow = AttrShadow::new(2, 10, 0);
            let (mut normalized, mut shadowed) = (Vec::new(), Vec::new());
            for chunk in seq.chunks(split) {
                normalized.extend(colors.feed(chunk));
                shadowed.extend(shadow.rewrite_stream(chunk));
            }
            assert!(italic(&normalized), "{}", readable(&normalized));
            assert!(italic(&shadowed), "{}", readable(&shadowed));
        }
    }

    #[test]
    fn the_short_form_never_outgrows_the_bound() {
        // The longest short form: 32 values of five digits each, and
        // `u16::MAX` is the longest a value gets.
        let mut seq = b"\x1b[".to_vec();
        seq.extend_from_slice("000099999;".repeat(40).as_bytes());
        let mut colors = ColorNormalizer::new();
        let out = colors.feed(&seq);
        assert!(out.is_empty());
        assert_eq!(colors.pending.len(), 193);
        let mut out = colors.feed(b"m");
        out.truncate(out.len() - 1);
        assert_eq!(readable(&out), format!("E[{}65535", "65535;".repeat(31)));
    }

    #[test]
    fn a_sequence_split_across_feeds_is_not_lost() {
        let mut shadow = AttrShadow::new(2, 10, 0);
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
