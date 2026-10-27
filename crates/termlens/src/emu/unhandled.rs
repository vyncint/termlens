//! What the backend could not render, kept so a screen can say so.
//!
//! `vt100` calls a [`Callbacks`] method for every escape sequence outside
//! its dispatch table, and termlens used to install the no-op set and throw
//! the list away — so an application that asked for something the emulator
//! does not implement got a plausible-looking wrong grid and nothing
//! anywhere said why (#266). This is the collection half: distinct shapes,
//! first seen first, bounded with an overflow count, rendered the way the
//! unanswered-query record renders a sequence (`^[[20h`), and read back by
//! every snapshot through [`Screen::unsupported`](crate::Screen::unsupported).
//!
//! Not everything vt100 declines is unsupported. The sequence tracker
//! (`emu/seq.rs`) handles a set of them before or beside the backend — the
//! character sets, tab stops, insert mode, DECSTR, DECSCUSR, the modes it
//! tracks, every query the responder answers or names — and those still
//! reach vt100's callbacks, because the bytes are fed through. They are
//! filtered here by shape, so the list means what its name says: sequences
//! **termlens** did not honour.

use std::sync::Arc;

use vt100::Callbacks;

/// Distinct shapes kept before the record only counts.
pub(super) const MAX_UNSUPPORTED: usize = 32;

/// The callback set the primary parser is built with.
#[derive(Debug)]
pub(super) struct Unhandled {
    seen: Vec<Arc<str>>,
    /// The same shapes as one shared slice, rebuilt when a shape is added —
    /// at most 32 times in a terminal's life — so a snapshot pays one
    /// refcount rather than a copy.
    shared: Arc<[Arc<str>]>,
    overflow: u64,
    visual_bells: u64,
}

impl Default for Unhandled {
    fn default() -> Self {
        Self {
            seen: Vec::new(),
            shared: Arc::from([] as [Arc<str>; 0]),
            overflow: 0,
            visual_bells: 0,
        }
    }
}

impl Unhandled {
    /// The distinct shapes, first seen first.
    pub(super) fn shapes(&self) -> Arc<[Arc<str>]> {
        Arc::clone(&self.shared)
    }

    /// Distinct shapes beyond [`MAX_UNSUPPORTED`], counted only.
    pub(super) fn overflow(&self) -> u64 {
        self.overflow
    }

    /// `ESC g` seen.
    pub(super) fn visual_bells(&self) -> u64 {
        self.visual_bells
    }

    fn record(&mut self, shape: String) {
        if self.seen.iter().any(|s| **s == *shape) {
            return;
        }
        if self.seen.len() >= MAX_UNSUPPORTED {
            self.overflow = self.overflow.saturating_add(1);
            return;
        }
        self.seen.push(Arc::from(shape));
        self.shared = self.seen.iter().cloned().collect();
    }
}

/// `params` as they were written: `;` between parameters, `:` between the
/// sub-parameters of one.
fn params_text(params: &[&[u16]]) -> String {
    // vte exposes an omitted parameter as one zeroed accumulator. Omitted
    // and explicit zero have the same default meaning, so keep the recorded
    // shape parameterless instead of inventing a `0` the application did
    // not write.
    if params.len() == 1 && params[0].len() == 1 && params[0][0] == 0 {
        return String::new();
    }

    params
        .iter()
        .map(|p| p.iter().map(u16::to_string).collect::<Vec<_>>().join(":"))
        .collect::<Vec<_>>()
        .join(";")
}

/// DEC private modes the sequence tracker keeps itself, so a set or reset
/// of one is termlens's business even though vt100 declines it — plus 9001,
/// win32-input-mode, which is not the application's at all: on Windows the
/// console asks its host for it in the preamble every child gets, and
/// termlens's answer is to keep sending VT. Recording it would put the
/// console's handshake in every Windows snapshot's list (#149).
const TRACKED_PRIVATE_MODES: &[u16] = &[9, 1000, 1002, 1003, 1005, 1006, 1015, 1004, 2026, 9001];

/// DECAWM, autowrap. Setting it asks for the only behaviour the emulator
/// has, so `CSI ? 7 h` — which ncurses sends at every start (`smam`) — is
/// honoured; resetting it is not modelled, and stays on record (#581).
const AUTOWRAP: u16 = 7;

/// The SGR parameters [`shadow`](super::shadow) recovers: blink, conceal,
/// strikethrough and their three "off" forms.
///
/// vt100 drops these, so they arrive here as unhandled — and the shadow
/// parser then puts the attribute on the cell anyway, which is the whole
/// reason it exists. Recording them said the emulator did not implement a
/// sequence whose effect `Style::blink` reports on the same `Screen`
/// (#320): the accessor names *the emulator's* gaps, and this is the
/// backend's, in front of which termlens does the work.
///
/// Kept in step with `shadow::sgr_substitute` by a test.
pub(super) const SHADOW_SGR_PARAMS: &[u16] = &[5, 6, 8, 9, 25, 28, 29];

impl Callbacks for Unhandled {
    fn visual_bell(&mut self, _: &mut vt100::Screen) {
        self.visual_bells = self.visual_bells.saturating_add(1);
    }

    fn resize(&mut self, _: &mut vt100::Screen, (rows, cols): (u16, u16)) {
        // The grid's size is the test's to set, so the request is not
        // honoured — and that is exactly what makes it worth recording.
        self.record(format!("^[[8;{rows};{cols}t"));
    }

    fn unhandled_control(&mut self, _: &mut vt100::Screen, b: u8) {
        // SO and SI are the locking shifts the tracker handles.
        if matches!(b, 0x0e | 0x0f) {
            return;
        }
        self.record(match b {
            0x7f => "^?".to_owned(),
            0..=0x1f => format!("^{}", (b + 0x40) as char),
            other => format!("\\x{other:02x}"),
        });
    }

    fn unhandled_escape(&mut self, _: &mut vt100::Screen, i1: Option<u8>, i2: Option<u8>, b: u8) {
        // Designations (`ESC ( 0` …), HTS, SS2/SS3: the tracker's. And ST
        // (`ESC \`), which is not a sequence of its own but the terminator
        // of the OSC, DCS or APC string that came before it — the string
        // parser hands the ESC back and the backslash arrives here.
        let designation = matches!(i1, Some(b'(' | b')' | b'*' | b'+')) && i2.is_none();
        if designation || (i1.is_none() && matches!(b, b'H' | b'N' | b'O' | b'\\')) {
            return;
        }
        let mut shape = String::from("^[");
        shape.extend(i1.map(char::from));
        shape.extend(i2.map(char::from));
        shape.push(char::from(b));
        self.record(shape);
    }

    fn unhandled_csi(
        &mut self,
        _: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        // vte hands the private marker (`?`, `>`, `=`) over as the first
        // intermediate; a real intermediate (`$`, `!`, `SP`) follows it or
        // stands alone.
        let (prefix, intermediate) = match (i1, i2) {
            (Some(p @ (b'?' | b'>' | b'=')), other) => (Some(p), other),
            (other, None) => (None, other),
            (a, b) => (a, b),
        };
        let first = params.first().and_then(|p| p.first()).copied();
        let handled = match (prefix, intermediate, c) {
            // Tab stops, insert mode, the queries the responder answers or
            // names, and the window reports it answers or names.
            (None, None, 'g' | 'I' | 'Z' | 'c' | 'n') => true,
            // The window reports the responder answers (14, 16, 18) and the
            // ones it names in a timeout (11, 13, 19, 20, 21) — `seq.rs`'s
            // table, single-parameter like it. Everything else in the family,
            // the title stack included, nobody honoured.
            (None, None, 't') => {
                params.len() == 1 && matches!(first, Some(11 | 13 | 14 | 16 | 18 | 19 | 20 | 21))
            }
            (None, None, 'h' | 'l') => first == Some(4),
            // DECSTR, DECSCUSR, DECRQM and the mode reports.
            (None, Some(b'!'), 'p') | (None, Some(b' '), 'q') => true,
            (_, Some(b'$'), 'p' | 'y') => true,
            // DA2, and the kitty keyboard probe the responder names.
            (Some(b'>'), None, 'c') | (Some(b'?'), None, 'u') => true,
            // The private modes the tracker keeps; any other private mode
            // vt100 declined is one nobody honoured.
            (Some(b'?'), None, 'h' | 'l') => params
                .iter()
                .flat_map(|p| p.iter())
                .all(|m| TRACKED_PRIVATE_MODES.contains(m) || (c == 'h' && *m == AUTOWRAP)),
            // An SGR every one of whose parameters the shadow recovers is
            // implemented, and naming it here would contradict the cell
            // (#320). One parameter outside the set and the whole sequence
            // is still recorded: dropping it would hide a genuine gap, and
            // `^[[5;59m` — blink recovered, underline colour modelled by
            // nobody — is exactly that shape.
            (None, None, 'm') => {
                let mut seen = false;
                params.iter().flat_map(|p| p.iter()).all(|p| {
                    seen = true;
                    SHADOW_SGR_PARAMS.contains(p)
                }) && seen
            }
            _ => false,
        };
        if handled {
            return;
        }
        let mut shape = String::from("^[[");
        shape.extend(prefix.map(char::from));
        shape.push_str(&params_text(params));
        shape.extend(intermediate.map(char::from));
        shape.push(c);
        self.record(shape);
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        // Links and the clipboard are the tracker's in every form. The
        // colour and palette codes are the tracker's only as questions —
        // the responder answers `10;?` and `11;?` and names `4;n;?` in a
        // timeout — because the same codes also *set* (`10;#ff0000`,
        // `4;1;rgb:…`), and a set is not a question: exempting the bare
        // number dropped the set from this record too, so an application
        // repainting its palette got the wrong colours with nothing saying
        // why (#392).
        // Exempt exactly what `seq.rs` claims, and in the same shapes it
        // claims them in. `10;?` and `11;?` are answered, so only that
        // two-operand form is the responder's: xterm also reads
        // `10;#ff0000;?` as set-then-query, and since the responder does
        // not answer *that*, it belongs here. A palette query is any
        // `4;…?`, which is the rule `seq.rs` applies to it.
        let colour_query = match params.first() {
            Some(&b"10" | &b"11") => params.len() == 2 && params[1] == b"?",
            Some(&b"4") => matches!(params.last(), Some(&b"?")),
            _ => false,
        };
        if matches!(params.first(), Some(&b"8" | &b"52")) || colour_query {
            return;
        }
        let mut shape = String::from("^[]");
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                shape.push(';');
            }
            shape.push_str(&String::from_utf8_lossy(p));
        }
        self.record(shape);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(bytes: &[u8]) -> vt100::Parser<Unhandled> {
        let mut parser = vt100::Parser::new_with_callbacks(
            super::super::vt100::nz(4),
            super::super::vt100::nz(20),
            0,
            Unhandled::default(),
        );
        parser.process(bytes);
        parser
    }

    fn shapes(bytes: &[u8]) -> Vec<String> {
        fed(bytes)
            .callbacks()
            .shapes()
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn autowrap_on_is_honoured_and_autowrap_off_is_on_record() {
        // Every ncurses program sends `CSI ? 7 h` at startup, and it asks
        // for the only behaviour there is: naming it made an empty
        // `unsupported()` impossible for any of them (#581). Turning wrap
        // off is not modelled, so that still is named.
        assert_eq!(shapes(b"\x1b[?7hready"), [] as [String; 0]);
        assert_eq!(shapes(b"\x1b[?7l"), ["^[[?7l"]);
        // In a list, the set is honoured with the others it is set with.
        assert_eq!(shapes(b"\x1b[?7;1000h"), [] as [String; 0]);
        assert_eq!(shapes(b"\x1b[?7;12h"), ["^[[?7;12h"]);
    }

    #[test]
    fn a_sequence_nobody_honours_is_recorded_once_in_its_written_form() {
        assert_eq!(
            shapes(b"\x1b[20h\x1b[20h\x1bD\x1b]9;hi\x07\x05\x1b[?69h"),
            ["^[[20h", "^[D", "^[]9;hi", "^E", "^[[?69h"]
        );
    }

    /// The `CSI … t` family splits three ways, and only one of them belongs
    /// in this record: the reports the responder answers and the ones it
    /// names in a timeout are already accounted for elsewhere, so listing
    /// them here would report the same sequence twice. The title stack is
    /// honoured by nobody, which is what `Screen::unsupported()` is for.
    #[test]
    fn title_stack_operations_are_reported_but_window_reports_are_not() {
        assert_eq!(shapes(b"\x1b[22;0t\x1b[23;0t"), ["^[[22;0t", "^[[23;0t"]);
        assert!(shapes(b"\x1b[14t\x1b[16t\x1b[18t").is_empty());
        // Named in the timeout note by `seq.rs`, so not unsupported here.
        assert!(shapes(b"\x1b[11t\x1b[13t\x1b[19t\x1b[20t\x1b[21t").is_empty());
        // Multi-parameter forms the responder does not claim.
        assert_eq!(shapes(b"\x1b[18;0t"), ["^[[18;0t"]);
    }

    #[test]
    fn parameterless_sequences_are_recorded_without_a_synthetic_zero() {
        assert_eq!(
            shapes(b"\x1b[s\x1b[>q\x1b[?h\x1b[20h\x1b[?69h\x1b[58;5;1m"),
            ["^[[s", "^[[>q", "^[[?h", "^[[20h", "^[[?69h", "^[[58;5;1m",]
        );
    }

    /// The colour and palette codes set as well as ask. The exemption is
    /// the responder's, not the number's: a set (`OSC 4;1;rgb:…`,
    /// `OSC 10;#ff0000`) is honoured by nobody and lands here, while the
    /// `?` forms stay out of the record — `10;?` and `11;?` are answered
    /// and `4;n;?` is named in a timeout — and a code nobody tracks at all
    /// still lands here as it always did (#392).
    #[test]
    fn a_colour_set_is_reported_but_its_query_is_not() {
        for (bytes, expected) in [
            (
                &b"\x1b]4;1;rgb:ff/00/00\x07"[..],
                vec!["^[]4;1;rgb:ff/00/00"],
            ),
            (&b"\x1b]10;#ff0000\x07"[..], vec!["^[]10;#ff0000"]),
            (&b"\x1b]11;#00ff00\x07"[..], vec!["^[]11;#00ff00"]),
            (&b"\x1b]10;?\x07"[..], Vec::<&str>::new()),
            (&b"\x1b]11;?\x07"[..], Vec::<&str>::new()),
            (&b"\x1b]4;1;?\x07"[..], Vec::<&str>::new()),
            // A palette query is any `4;…?`, which is what `seq.rs` names.
            (&b"\x1b]4;1;rgb:ff/00/00;2;?\x07"[..], Vec::<&str>::new()),
            // But `10`/`11` are answered only in their two-operand form.
            // xterm reads this one as set-then-query, and the responder
            // does not answer it, so it belongs in the record.
            (&b"\x1b]10;#ff0000;?\x07"[..], vec!["^[]10;#ff0000;?"]),
            (&b"\x1b]777;x\x07"[..], vec!["^[]777;x"]),
        ] {
            assert_eq!(
                shapes(bytes),
                expected,
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn what_the_tracker_handles_is_not_reported() {
        let ours = b"\x1bH\x1b[3g\x1b[2I\x1b[1Z\x1b[4h\x1b[4l\x1b[6n\x1b[c\x1b[>c\x1b[18t\
                     \x1b(0\x1b)0\x1bN\x1bO\x1b[!p\x1b[2 q\x1b[?2026$p\x1b[?1004h\x1b[?2026h\
                     \x1b[?2026l\x1b[?1000h\x1b[?u\x1b]8;;http://x\x1b\\\x1b]52;c;aGk=\x07\
                     \x1b]11;?\x07\x0e\x0f";
        assert_eq!(shapes(ours), Vec::<String>::new());
    }

    #[test]
    fn the_record_is_bounded_and_counts_the_rest() {
        let mut stream = Vec::new();
        for mode in 20..(20 + MAX_UNSUPPORTED as u16 + 5) {
            stream.extend_from_slice(format!("\x1b[{mode}h").as_bytes());
        }
        let parser = fed(&stream);
        assert_eq!(parser.callbacks().shapes().len(), MAX_UNSUPPORTED);
        assert_eq!(parser.callbacks().overflow(), 5);
    }

    #[test]
    fn a_visual_bell_and_a_resize_request_are_seen() {
        let parser = fed(b"\x1bg\x1b[8;10;40t");
        assert_eq!(parser.callbacks().visual_bells(), 1);
        assert_eq!(
            parser
                .callbacks()
                .shapes()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            ["^[[8;10;40t"]
        );
        assert_eq!(
            parser.screen().size(),
            (super::super::vt100::nz(4), super::super::vt100::nz(20)),
            "the request is recorded, not honoured"
        );
    }
}
