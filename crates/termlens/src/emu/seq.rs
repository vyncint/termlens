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
//!
//! It also tracks the one piece of screen state the vt100 backend does not
//! expose: the **window title** (`OSC 0`/`OSC 2`), kept whole in its own
//! buffer — the diagnostic capture below truncates at 24 bytes, real titles
//! don't fit.

use std::sync::Arc;

use crate::graphics::{GraphicsBuilder, GraphicsCounts, GraphicsPayload};
use crate::screen::{Clipboard, Link};

/// OSC strings are captured whole (titles must not truncate), but bounded:
/// a buggy or hostile stream must not grow memory without limit. No real
/// title comes anywhere near this.
const OSC_CAPTURE_MAX: usize = 64 * 1024;

/// How many `OSC 8` spans to keep, oldest evicted.
///
/// Bounded rather than unbounded because a TUI redraws: an application that
/// links five things every frame would otherwise grow this forever. Sixty-four
/// holds many screens' worth of links, so the current frame's are always
/// present, which is what a test asserts on.
const LINK_HISTORY: usize = 64;

/// How many label bytes one span may accumulate.
///
/// This is the bound that stops an *unterminated* link from bleeding into the
/// rest of the stream: without it, one missing `OSC 8 ; ; ST` makes every
/// byte the application writes afterwards part of one label. Past it the
/// label is reported as unknown rather than as a prefix — a prefix of the
/// wrong length is still a wrong answer.
const LINK_LABEL_MAX: usize = 4 * 1024;

/// Decode standard base64 (`OSC 52` payloads). `None` for anything that is
/// not valid: an out-of-alphabet byte, a bad length, or padding in the
/// wrong place. Returning `None` rather than a best effort is the point —
/// a partially decoded clipboard would be indistinguishable from a
/// correct one, and a test asserting on it would pass while proving
/// nothing.
pub(crate) fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    fn value(b: u8) -> Option<u32> {
        match b {
            b'A'..=b'Z' => Some(u32::from(b - b'A')),
            b'a'..=b'z' => Some(u32::from(b - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(b - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    // Padding is optional in the wild but must be trailing and must not
    // exceed two bytes; what remains has to be a whole number of quanta.
    let body = input.strip_suffix(b"==").map_or_else(
        || input.strip_suffix(b"=").unwrap_or(input),
        |stripped| stripped,
    );
    let pad = input.len() - body.len();
    if pad > 0 && input.len() % 4 != 0 {
        return None;
    }
    if body.len() % 4 == 1 {
        return None; // one leftover character encodes nothing
    }

    let mut out = Vec::with_capacity(body.len() / 4 * 3 + 2);
    for quantum in body.chunks(4) {
        let mut bits = 0u32;
        for &b in quantum {
            bits = (bits << 6) | value(b)?;
        }
        // A short final quantum carries 1 or 2 bytes; shift it up to a
        // full 24-bit group and keep only the bytes it actually encodes.
        let carried = match quantum.len() {
            4 => 3,
            3 => 2,
            _ => 1,
        };
        bits <<= 6 * (4 - quantum.len());
        for i in 0..carried {
            #[allow(clippy::cast_possible_truncation)]
            out.push((bits >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

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
    /// An inline graphics payload completed. Carried out of the tracker
    /// rather than stored in it because the placement — where the cursor
    /// stood — is a fact about the grid, which only the emulator holds.
    Graphics(Box<GraphicsPayload>),
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
    /// DECRQM: "is private mode `n` set?" — `CSI ? n $ p`. Answering
    /// this truthfully lets an application that *probes* before using a
    /// mode (synchronized output above all) turn it on against termlens.
    RequestMode(u32),
    /// Window size in pixels: `CSI 14 t`.
    WindowSizePixels,
    /// Character cell size in pixels: `CSI 16 t`.
    CellSizePixels,
    /// The kitty graphics capability probe (`APC _G … a=q … ST`), carrying
    /// the image id it named so a reply can echo it back.
    ///
    /// Classified rather than refused outright: whether it is answerable
    /// depends on what the test declared the terminal supports, and that is
    /// policy, which lives with the caller.
    KittyGraphics {
        /// The `i=` value, if the probe gave one.
        id: Option<u32>,
        /// Printable rendering, for the timeout note when unanswered.
        shape: String,
    },
    /// XTGETTCAP: "what is capability `name`?" — `DCS + q <hex names> ST`,
    /// carrying the hex-encoded names exactly as asked, since the reply must
    /// echo each one back.
    RequestTermcap {
        /// Hex-encoded names, `;`-separated, as the application wrote them.
        names: String,
        /// Printable rendering, for the timeout note when unanswered.
        shape: String,
    },
    /// Recognized as a question, but one termlens has no answer for
    /// (XTGETTCAP, kitty `CSI ? u`, DECRQSS, `OSC 4`/`OSC 52`, other
    /// DSR/DA/XTWINOPS reports, …). Carries a printable rendering for
    /// diagnostics.
    Unanswerable(String),
}

/// The numeric value of `key` in a kitty control block (`i=3,a=q`), if it
/// carries one. Keys are comma-separated `name=value` pairs.
fn kitty_key(control: &[u8], key: &[u8]) -> Option<u32> {
    control
        .split(|&b| b == b',')
        .find_map(|pair| pair.strip_prefix(key))
        .and_then(|digits| std::str::from_utf8(digits).ok())
        .and_then(|digits| digits.parse().ok())
}

/// Whether a kitty control block sets `key` to exactly `value`.
fn key_is(control: &[u8], key: &[u8], value: &[u8]) -> bool {
    control.split(|&b| b == b',').any(|pair| {
        pair.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(b"="))
            == Some(value)
    })
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
    /// Intermediate byte seen in the current CSI (only `$` matters).
    csi_intermediate: u8,
    csi_invalid: bool,
    csi_first: bool,
    csi_param: u32,
    csi_has_digits: bool,
    csi_first_param: u32,
    csi_param_count: u8,
    csi_saw_2026: bool,
    /// Set when the current CSI's parameter list contains 1004 (focus
    /// reporting), the same trick `csi_saw_2026` uses — a mode can arrive
    /// anywhere in a multi-mode list, so scanning for it beats assuming it
    /// is the only parameter.
    csi_saw_1004: bool,
    /// Raw capture of the current sequence (from ESC), for diagnostics
    /// and DCS query recognition. Bounded; long sequences truncate.
    seq_buf: [u8; 24],
    seq_len: u8,
    /// Content bytes of the current OSC string (no `ESC ]`, no
    /// terminator), kept whole so titles never truncate.
    osc_buf: Vec<u8>,
    /// Set when the current OSC string hit [`OSC_CAPTURE_MAX`]. What was
    /// captured is then a prefix, and a prefix of base64 can still decode —
    /// to the wrong thing. Reporting the truncation is the only honest
    /// option, so it is tracked rather than inferred.
    osc_truncated: bool,
    /// The window title as most recently set via `OSC 0`/`OSC 2`; empty
    /// until the application sets one. Shared so snapshots clone for free.
    title: Arc<str>,
    /// The most recent `OSC 52` clipboard write, decoded.
    clipboard: Option<Arc<Clipboard>>,
    /// `OSC 8` spans seen, oldest first. A span is pushed when it *opens* —
    /// the URI is known then, and a test waiting for a link should not have
    /// to wait for the application to close it — and completed in place when
    /// it closes.
    links: Arc<Vec<Link>>,
    /// Label bytes of the span currently open. Held outside `links` so the
    /// shared vector is touched twice per span (open, close) rather than
    /// once per character written.
    link_label: Vec<u8>,
    /// True while a span is open, so printable bytes know where to go.
    link_open: bool,
    /// Set when the open span's label passed [`LINK_LABEL_MAX`].
    link_label_truncated: bool,
    /// Bells rung in ground state. A `BEL` closing an OSC string is a
    /// terminator and one inside a DCS-class string is payload; neither is
    /// a bell, and both are handled by the state machine rather than here.
    bells: u64,
    /// Inline graphics transmitted, counted as images rather than as
    /// escapes: [`GraphicsBuilder`] joins a chunked kitty transmission
    /// before either is incremented.
    counts: GraphicsCounts,
    /// The payload being assembled, if a kitty transmission is mid-chunk.
    building: GraphicsBuilder,
    /// How many payload bytes may be kept for inspection. Counting is
    /// unaffected by it; only the data is dropped.
    capture: usize,
    /// Printable characters written since the current frame began. Reset by
    /// the Begin, read by the End — so it measures what one repaint drew,
    /// which is the other half of "did this repaint get more expensive?".
    frame_printable: u32,
    /// True while the application has focus reporting (mode 1004) enabled.
    /// Tracked here because vt100 does not model 1004 at all — the same
    /// reason the window title is tracked here.
    focus_events: bool,
    /// The raw `DECSCUSR` parameter the application last asked for, or
    /// `None` while it has never asked. Kept as the parameter rather than
    /// as a decoded shape so the one place that knows what `5` means is
    /// the accessor on `Screen`, and "never asked" stays distinguishable
    /// from every value it could have asked for.
    cursor_style: Option<u8>,
    /// Which introducer opened the current DCS-class string: `P` (DCS),
    /// `X` (SOS), `^` (PM) or `_` (APC). Sixel and kitty graphics differ
    /// only by this, so consuming all four alike — which is all the tracker
    /// needed before — cannot tell them apart.
    dcs_introducer: u8,
    /// Final byte of the current DCS header, 0 until seen. Sixel's is `q`
    /// with no intermediate; XTGETTCAP and DECRQSS reach `q` through `+`
    /// and `$`, which is what keeps them from being counted as pictures.
    dcs_final: u8,
    /// Intermediate byte in the current DCS header (`+` or `$` in practice).
    dcs_intermediate: u8,
    /// Payload bytes after the header, so a payload's size is reportable
    /// without keeping the payload.
    dcs_data_len: u64,
    /// The opening bytes of a DCS-class payload: enough for a kitty control
    /// block (`G` plus `key=value` pairs up to the first `;`) and for an
    /// XTGETTCAP name list, whose hex names run about six bytes each. A
    /// longer list truncates, and the names that survive are still answered
    /// — a partial answer beats none, and the ones we drop get no reply,
    /// which is what an application already has to handle.
    dcs_head: [u8; 128],
    dcs_head_len: u8,
    /// Every byte of the current DCS-class string, up to the capture bound —
    /// the image data itself, which the 128-byte head deliberately is not.
    /// Kept only while the bound allows; `dcs_body_full` says whether it is
    /// the whole payload or the start of one.
    dcs_body: Vec<u8>,
    dcs_body_full: bool,
}

impl SeqTracker {
    pub(crate) fn new(capture: usize) -> Self {
        Self {
            state: State::Ground,
            utf8_remaining: 0,
            sync_update: false,
            csi_prefix: 0,
            csi_intermediate: 0,
            csi_invalid: false,
            csi_first: true,
            csi_param: 0,
            csi_has_digits: false,
            csi_first_param: 0,
            csi_param_count: 0,
            csi_saw_2026: false,
            csi_saw_1004: false,
            seq_buf: [0; 24],
            seq_len: 0,
            osc_buf: Vec::new(),
            osc_truncated: false,
            title: Arc::from(""),
            clipboard: None,
            links: Arc::new(Vec::new()),
            link_label: Vec::new(),
            link_open: false,
            link_label_truncated: false,
            bells: 0,
            counts: GraphicsCounts::default(),
            building: GraphicsBuilder::default(),
            capture,
            frame_printable: 0,
            focus_events: false,
            cursor_style: None,
            dcs_introducer: 0,
            dcs_final: 0,
            dcs_intermediate: 0,
            dcs_data_len: 0,
            dcs_head: [0; 128],
            dcs_head_len: 0,
            dcs_body: Vec::new(),
            dcs_body_full: true,
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

    /// The most recent `OSC 52` clipboard write, or `None` if the
    /// application has not copied anything.
    pub(crate) fn clipboard(&self) -> Option<Arc<Clipboard>> {
        self.clipboard.clone()
    }

    /// The window title as most recently set via `OSC 0`/`OSC 2` (empty
    /// until the application sets one).
    pub(crate) fn title(&self) -> Arc<str> {
        Arc::clone(&self.title)
    }

    /// Bells rung in ground state so far.
    pub(crate) fn bells(&self) -> u64 {
        self.bells
    }

    /// The `OSC 8` hyperlinks seen so far, oldest first.
    pub(crate) fn links(&self) -> Arc<Vec<Link>> {
        Arc::clone(&self.links)
    }

    /// Begin an `OSC 8` span. Any span still open is closed first: a new
    /// URI supersedes the current one in a real terminal, so treating it as
    /// nested would attribute the new label to the old target.
    fn open_link(&mut self, params: &[u8], uri: &[u8]) {
        self.close_link();
        // `id=` is the one standard parameter; the list is `:`-separated.
        let id = params
            .split(|&b| b == b':')
            .find_map(|pair| pair.strip_prefix(b"id="))
            .map(|value| String::from_utf8_lossy(value).into_owned());
        let uri = String::from_utf8_lossy(uri).into_owned();
        let links = Arc::make_mut(&mut self.links);
        links.push(Link::open(&uri, id.as_deref()));
        while links.len() > LINK_HISTORY {
            links.remove(0);
        }
        self.link_open = true;
        self.link_label.clear();
        self.link_label_truncated = false;
    }

    /// Close the open `OSC 8` span, if any, recording the text it wrapped.
    fn close_link(&mut self) {
        if !self.link_open {
            return;
        }
        self.link_open = false;
        let raw = std::mem::take(&mut self.link_label);
        let label = if self.link_label_truncated {
            None
        } else {
            // Invalid UTF-8 is refused rather than replaced: a label with
            // U+FFFD in it is not the text the user sees.
            String::from_utf8(raw).ok()
        };
        self.link_label_truncated = false;
        // The open span is the newest, and eviction only ever drops the
        // oldest, so the one to complete is the last.
        if let Some(link) = Arc::make_mut(&mut self.links).last_mut() {
            link.close(label);
        }
    }

    /// Inline graphics counted so far.
    pub(crate) fn graphics(&self) -> GraphicsCounts {
        self.counts
    }

    /// True while the application has focus reporting (mode 1004) enabled.
    pub(crate) fn focus_events(&self) -> bool {
        self.focus_events
    }

    /// The raw `DECSCUSR` parameter last requested, `None` if never.
    pub(crate) fn cursor_style(&self) -> Option<u8> {
        self.cursor_style
    }

    /// Printable characters written since the current frame began, cleared
    /// for the next one.
    pub(crate) fn take_frame_printable(&mut self) -> u32 {
        std::mem::replace(&mut self.frame_printable, 0)
    }

    fn reset_dcs_scanner(&mut self, introducer: u8) {
        self.dcs_introducer = introducer;
        self.dcs_final = 0;
        self.dcs_intermediate = 0;
        self.dcs_data_len = 0;
        self.dcs_head_len = 0;
        self.dcs_body.clear();
        self.dcs_body_full = true;
    }

    fn reset_csi_scanner(&mut self) {
        self.csi_prefix = 0;
        self.csi_intermediate = 0;
        self.csi_invalid = false;
        self.csi_first = true;
        self.csi_param = 0;
        self.csi_has_digits = false;
        self.csi_first_param = 0;
        self.csi_param_count = 0;
        self.csi_saw_2026 = false;
        self.csi_saw_1004 = false;
    }

    fn push_seq(&mut self, b: u8) {
        if usize::from(self.seq_len) < self.seq_buf.len() {
            self.seq_buf[usize::from(self.seq_len)] = b;
            self.seq_len += 1;
        }
    }

    fn push_osc(&mut self, b: u8) {
        if self.osc_buf.len() < OSC_CAPTURE_MAX {
            self.osc_buf.push(b);
        } else {
            self.osc_truncated = true;
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
        if self.csi_param == 1004 {
            self.csi_saw_1004 = true;
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
            // `$` is the intermediate of the DECRQM request (`CSI ? n $ p`)
            // and `SP` of DECSCUSR (`CSI Ps SP q`); recording them keeps
            // those sequences classifiable instead of discarding them as
            // unrecognized.
            b'$' | b' ' => self.csi_intermediate = b,
            // Sub-parameters or other intermediates: none of the sequences
            // we recognize use them.
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

        // DECRQM (`CSI ? n $ p`) and DECRQSS-adjacent `$`-intermediate
        // requests. Handled before the plain-CSI table below, which
        // assumes no intermediate.
        if self.csi_intermediate == b'$' {
            return match (self.csi_prefix, b) {
                (b'?', b'p') if self.csi_param_count == 1 => {
                    SeqEvent::Query(Query::RequestMode(self.csi_first_param))
                }
                // ANSI-mode DECRQM and mode *reports* we cannot answer.
                (_, b'p' | b'y') => SeqEvent::Query(Query::Unanswerable(self.seq_printable())),
                _ => SeqEvent::None,
            };
        }

        // DECSCUSR (`CSI Ps SP q`): the shape of the cursor, and whether it
        // blinks. vt100 models neither, so a modal editor switching to a bar
        // for insert mode is invisible without this — as is the program that
        // switches and never switches back, which leaves the user's terminal
        // wrong after exit.
        //
        // Also reached by the other `SP`-intermediate sequences (SL, SR).
        // They are not ours to act on, and returning here keeps them out of
        // the plain-CSI table below, which assumes no intermediate — exactly
        // as they were kept out by `csi_invalid` before `SP` was accepted.
        if self.csi_intermediate == b' ' {
            if b == b'q' && self.csi_prefix == 0 && self.csi_param_count <= 1 {
                // An omitted parameter means 0. Values above 6 are undefined
                // and xterm ignores them; so do we, leaving the last style
                // the application actually asked for rather than inventing
                // one it did not.
                let ps = if self.csi_param_count == 0 {
                    0
                } else {
                    self.csi_first_param
                };
                if let Ok(style @ 0..=6) = u8::try_from(ps) {
                    self.cursor_style = Some(style);
                }
            }
            return SeqEvent::None;
        }

        // DEC private mode 1004 (focus reporting). vt100 does not model it,
        // so an application that enables it is invisible without this — and
        // `focus_in`/`focus_out` refuse to send events the application never
        // asked for, exactly as `click` refuses without mouse tracking.
        if self.csi_prefix == b'?' && self.csi_saw_1004 {
            match b {
                b'h' => self.focus_events = true,
                b'l' => self.focus_events = false,
                _ => {}
            }
        }

        // DEC private mode 2026 (synchronized output).
        if self.csi_prefix == b'?' && self.csi_saw_2026 {
            match b {
                b'h' => {
                    self.sync_update = true;
                    self.frame_printable = 0;
                    return SeqEvent::SyncBegin;
                }
                // Only an End that closes a Begin we saw ends a frame.
                // An unmatched End is ordinary application behaviour, not a
                // malformed stream: programs defensively reset terminal
                // modes at startup and on crash, and such a reset string
                // naturally contains `?2026l`. Treating it as a frame would
                // manufacture one out of whatever happened to be on the
                // grid — and, worse, push `frames_seen` off zero, which is
                // what gates the "never emitted a synchronized update"
                // diagnosis. Silently not ending a frame is the whole
                // correct response.
                b'l' if self.sync_update => {
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
            // Pixel geometry. Arithmetic, not rendering: answering claims
            // nothing the emulator cannot do, and DA1 goes on declining
            // graphics either way.
            (0, b't') if single(14) => Some(Query::WindowSizePixels),
            (0, b't') if single(16) => Some(Query::CellSizePixels),
            // Questions we can recognize but not answer.
            (_, b'n') | (b'=', b'c') => Some(Query::Unanswerable(self.seq_printable())),
            (b'?', b'u') if params_empty => {
                // kitty keyboard probe. Its protocol pairs this with DA1;
                // our DA1 answer unblocks the probe like any non-kitty
                // terminal, but the probe itself is still unanswered.
                Some(Query::Unanswerable(self.seq_printable()))
            }
            (0, b't')
                if matches!(self.csi_first_param, 11 | 13 | 19 | 20 | 21)
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
        if was_osc {
            // `osc_buf` holds exactly the content (`ESC ]` and terminator
            // stripped). A color query is exactly `10;?` or `11;?`
            // (12 = cursor color: unanswerable).
            match self.osc_buf.as_slice() {
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
                content
                    if content.ends_with(b"?")
                        && (content.starts_with(b"4;") || content.starts_with(b"52;")) =>
                {
                    // Palette queries (`OSC 4;n;?`) and clipboard reads
                    // (`OSC 52;…;?`), recognized so a blocked application
                    // is diagnosed rather than left silently hanging. The
                    // `?` matters: both codes also *set* — a palette
                    // colour, or the clipboard from base64 — and a set is
                    // not a question.
                    return SeqEvent::Query(Query::Unanswerable(self.seq_printable()));
                }
                content => {
                    // OSC 0 (icon + title) / OSC 2 (title) set the window
                    // title — state the vt100 backend does not track.
                    // OSC 1 (icon only) is deliberately ignored.
                    if let Some(title) = content
                        .strip_prefix(b"0;")
                        .or_else(|| content.strip_prefix(b"2;"))
                    {
                        self.title = Arc::from(String::from_utf8_lossy(title));
                    } else if let Some(rest) = content.strip_prefix(b"8;") {
                        // `OSC 8 ; params ; URI` opens a hyperlink span and
                        // `OSC 8 ; ; ` closes it. Captured rather than
                        // answered, exactly as OSC 52 is below: the label
                        // renders as ordinary text, so the URL exists
                        // nowhere else and "did it link the right place?"
                        // is otherwise unanswerable.
                        //
                        // Parameters cannot contain `;`, so the first one
                        // ends them and everything after is the URI —
                        // which may itself contain `;` in a query string.
                        // No separator at all is malformed, and a malformed
                        // sequence is not a close: silently ending the span
                        // would attribute the text after it to nothing.
                        if let Some(sep) = rest.iter().position(|&b| b == b';') {
                            // A truncated OSC cannot be acted on in either
                            // direction: the URI we hold is a prefix, and
                            // opening a span on it would record a link the
                            // application never emitted.
                            if !self.osc_truncated {
                                // Owned before the call: both helpers take
                                // `&mut self`, and these slices point into
                                // `osc_buf`, which is part of it.
                                let params = rest[..sep].to_vec();
                                let uri = rest[sep + 1..].to_vec();
                                if uri.is_empty() {
                                    self.close_link();
                                } else {
                                    self.open_link(&params, &uri);
                                }
                            }
                        }
                    } else if let Some(rest) = content.strip_prefix(b"52;") {
                        // OSC 52 write: `targets ; base64`. The reads were
                        // classified above, so anything here is a write.
                        // Captured rather than answered — "did it copy the
                        // right thing?" is otherwise unanswerable, since the
                        // only evidence a test can see is the app's own
                        // toast, which proves the code path ran and nothing
                        // about the payload.
                        //
                        // Base64 has no `;`, so the first one is the
                        // separator. No separator at all is not a write.
                        if let Some(sep) = rest.iter().position(|&b| b == b';') {
                            let targets = String::from_utf8_lossy(&rest[..sep]).into_owned();
                            let payload = &rest[sep + 1..];
                            let text = if self.osc_truncated {
                                None
                            } else {
                                decode_base64(payload)
                                    .and_then(|bytes| String::from_utf8(bytes).ok())
                            };
                            self.clipboard = Some(Arc::new(Clipboard::new(&targets, text)));
                        }
                    }
                    return SeqEvent::None;
                }
            }
        }
        // An APC opening with `G` is the kitty graphics protocol — or a
        // continuation of one, which carries `m=` and nothing else.
        if self.dcs_introducer == b'_'
            && (self.dcs_head.first() == Some(&b'G') || self.building.in_progress())
        {
            let control = self.dcs_control_block().to_vec();
            // The control block is `G` followed by the comma-separated keys.
            let keys = control.strip_prefix(b"G").unwrap_or(&control);
            let is_query = keys.split(|&b| b == b',').any(|pair| pair == b"a=q");
            if !is_query {
                // One image, however many escapes it took. `m=1` says
                // another follows; the transmission is not an image until
                // one arrives without it, and counting each escape instead
                // reported a 4.9 KB chart as two pictures.
                self.building.kitty(keys);
                let at = self.dcs_payload_start();
                self.building.chunk(
                    self.dcs_data_len,
                    &self.dcs_body[at..],
                    self.dcs_body_full,
                    self.capture,
                );
                if key_is(keys, b"m", b"1") {
                    return SeqEvent::None;
                }
                return match self.building.finish() {
                    Some(payload) => {
                        self.counts.record(&payload);
                        SeqEvent::Graphics(Box::new(payload))
                    }
                    None => SeqEvent::None,
                };
            }
            // Only an explicit `a=q` is a question. A transmission is an
            // instruction, and classifying one as a query would put "the
            // application queried the terminal" into the next timeout of
            // every application that draws — a false diagnosis, and a loud
            // one.
            let id = kitty_key(keys, b"i=");
            return SeqEvent::Query(Query::KittyGraphics {
                id,
                shape: self.seq_printable(),
            });
        }
        // Sixel: `DCS <params> q <data> ST`, with no intermediate byte. The
        // intermediate is the whole distinction from the two DCS questions
        // below, which reach the same `q` final through `+` and `$`.
        if self.dcs_introducer == b'P' && self.dcs_final == b'q' && self.dcs_intermediate == 0 {
            self.building.sixel();
            let at = self.dcs_payload_start();
            self.building.chunk(
                self.dcs_data_len,
                &self.dcs_body[at..],
                self.dcs_body_full,
                self.capture,
            );
            return match self.building.finish() {
                Some(payload) => {
                    self.counts.record(&payload);
                    SeqEvent::Graphics(Box::new(payload))
                }
                None => SeqEvent::None,
            };
        }
        // XTGETTCAP (`ESC P + q <hex names> ST`). The names are needed to
        // answer, so they come out of the header capture rather than the
        // 24-byte diagnostic buffer.
        if self.dcs_introducer == b'P' && self.dcs_intermediate == b'+' && self.dcs_final == b'q' {
            let head = &self.dcs_head[..usize::from(self.dcs_head_len)];
            // `head` starts at `+`; the names follow the `q`.
            let names = head
                .iter()
                .position(|&b| b == b'q')
                .map(|at| String::from_utf8_lossy(&head[at + 1..]).into_owned())
                .unwrap_or_default();
            return SeqEvent::Query(Query::RequestTermcap {
                names,
                shape: self.seq_printable(),
            });
        }
        // DECRQSS (`ESC P $ q … ST`, "what is the current setting of …?").
        let body = &self.seq_buf[..usize::from(self.seq_len)];
        if matches!(body.get(2..4), Some(b"+q" | b"$q")) {
            return SeqEvent::Query(Query::Unanswerable(self.seq_printable()));
        }
        SeqEvent::None
    }

    /// The image data inside the captured body: for kitty everything after
    /// the `;` that closes the control block, and for sixel everything after
    /// the final byte that closes the DCS header. The protocol's own framing
    /// is metadata, already parsed; what is left is what a decoder wants.
    fn dcs_payload_start(&self) -> usize {
        let separator = if self.dcs_introducer == b'_' {
            b';'
        } else {
            self.dcs_final
        };
        self.dcs_body.iter().position(|&b| b == separator).map_or(
            // No separator: a control-only escape — a delete, or a
            // placement of something transmitted earlier — with no data.
            self.dcs_body.len(),
            |at| at + 1,
        )
    }

    /// The captured head of a DCS-class payload, cut at the first `;`.
    ///
    /// A kitty payload is `G <key=value>,… ; <base64>`; only the control
    /// block before the `;` is worth reading, and stopping there keeps a
    /// base64 blob from being scanned for keys it cannot contain.
    fn dcs_control_block(&self) -> &[u8] {
        let head = &self.dcs_head[..usize::from(self.dcs_head_len)];
        match head.iter().position(|&b| b == b';') {
            Some(sep) => &head[..sep],
            None => head,
        }
    }

    /// Consume one byte of a DCS-class string: header first, then payload.
    fn push_dcs(&mut self, b: u8) {
        // Every byte between the introducer and the terminator, for both
        // protocols alike — a sixel's parameters and a kitty control block
        // are a handful of bytes against an image's thousands, and counting
        // them uniformly is easier to reason about than two rules.
        self.dcs_data_len += 1;
        if self.dcs_introducer != b'_' && self.dcs_final == 0 {
            match b {
                // Parameter and private bytes stay in the header.
                0x30..=0x3f => {}
                // Intermediates: `+` (XTGETTCAP) and `$` (DECRQSS).
                0x20..=0x2f => self.dcs_intermediate = b,
                // Final byte closes the header; the rest is payload.
                0x40..=0x7e => self.dcs_final = b,
                _ => {}
            }
        }
        if usize::from(self.dcs_head_len) < self.dcs_head.len() {
            self.dcs_head[usize::from(self.dcs_head_len)] = b;
            self.dcs_head_len += 1;
        }
        // The body is what a decoder needs and the head is not: 128 bytes
        // holds a control block, never an image. Bounded, and the flag says
        // which of the two this is, so a truncated payload is reported as
        // uncaptured rather than decoded into a plausible wrong picture.
        if self.dcs_body.len() < self.capture {
            self.dcs_body.push(b);
        } else {
            self.dcs_body_full = false;
        }
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
                    // Only here is a BEL a bell. Inside OSC it terminates
                    // the string and inside a DCS-class string it is data,
                    // and both of those are other states.
                    if b == BEL && self.utf8_remaining == 0 {
                        self.bells = self.bells.saturating_add(1);
                    }
                    // One per *character*: continuation bytes arrive while
                    // `utf8_remaining` is non-zero, so nothing is counted
                    // twice. Controls, the BEL just handled included, are not
                    // printable.
                    if self.utf8_remaining == 0 && b >= 0x20 && b != 0x7f {
                        self.frame_printable = self.frame_printable.saturating_add(1);
                    }
                    // Text written inside an `OSC 8` span is that link's
                    // label. Bytes rather than characters, so a multi-byte
                    // grapheme survives the split — continuation bytes are
                    // all >= 0x80 and pass the same test.
                    if self.link_open && b >= 0x20 && b != 0x7f {
                        if self.link_label.len() < LINK_LABEL_MAX {
                            self.link_label.push(b);
                        } else {
                            self.link_label_truncated = true;
                        }
                    }
                    self.track_utf8(b);
                    State::Ground
                }
            }
            State::Esc => match b {
                b'[' => {
                    self.reset_csi_scanner();
                    State::Csi
                }
                b']' => {
                    self.osc_buf.clear();
                    self.osc_truncated = false;
                    State::Osc
                }
                // DCS, SOS, PM, APC — string sequences terminated by ST.
                b'P' | b'X' | b'^' | b'_' => {
                    self.reset_dcs_scanner(b);
                    State::Dcs
                }
                0x20..=0x2f => State::EscIntermediate,
                ESC => State::Esc,
                CAN | SUB => State::Ground,
                // RIS (`ESC c`), the hard reset: the terminal returns to its
                // power-on state, so the cursor is the terminal's default
                // again and any open hyperlink span cannot survive.
                //
                // Reporting the last `DECSCUSR` after a reset would claim a
                // fact the terminal does not hold — and it would do it in the
                // one place that matters, since `printf '\033c'` is a way a
                // program restores the cursor on exit, which is exactly what
                // `cursor_shape` exists to let a test check.
                //
                // The window title is deliberately *not* cleared here. In
                // xterm it is a window property rather than terminal state
                // and RIS does not restore it; guessing either way would be
                // the same error in the other direction. Same for the
                // clipboard, the bell count and the link *log*, which are
                // records of what the application emitted rather than state
                // the terminal still holds.
                b'c' => {
                    self.cursor_style = None;
                    self.close_link();
                    State::Ground
                }
                // Final byte of a two-character sequence (ESC 7, ESC =, …).
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
                _ => {
                    self.push_osc(b);
                    State::Osc
                }
            },
            State::Dcs => match b {
                ESC => State::DcsEsc,
                CAN | SUB => State::Ground,
                _ => {
                    self.push_dcs(b);
                    State::Dcs // BEL is data inside DCS-class strings
                }
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
    use crate::graphics::GraphicsFormat;

    fn fed(bytes: &[u8]) -> SeqTracker {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        t.feed(bytes);
        t
    }

    /// Every event a `BEL` byte can be, and only one of them is a bell.
    #[test]
    fn only_a_bel_in_ground_state_is_a_bell() {
        assert_eq!(fed(b"a\x07b").bells(), 1);
        assert_eq!(fed(b"\x07\x07\x07").bells(), 3, "a count, not a flag");
        assert_eq!(fed(b"plain").bells(), 0);

        // Terminating an OSC string: punctuation, not a bell. And the title
        // must still arrive, which is the thing that would break if the BEL
        // were intercepted before the state machine saw it.
        let t = fed(b"\x1b]0;my app\x07");
        assert_eq!(t.bells(), 0);
        assert_eq!(&*t.title(), "my app");

        // Payload inside a DCS-class string.
        assert_eq!(fed(b"\x1bPq\x07\x07\x1b\\").bells(), 0);
        // A bell after a sequence closes still counts.
        assert_eq!(fed(b"\x1b]0;t\x07\x07").bells(), 1);
    }

    /// The capture bound is a bound on the *payload*, not on each escape.
    ///
    /// kitty sends one escape per 4096 bytes, so a bound applied per escape
    /// would let a thousand-chunk image retain a thousand times the budget —
    /// which is the whole of what the knob is for.
    #[test]
    fn the_capture_bound_holds_across_a_chunked_transmission() {
        let mut tracker = SeqTracker::new(40);
        // Ten chunks of eight data bytes: every escape fits the bound
        // comfortably, and the ten together do not.
        let mut wire: Vec<u8> = b"\x1b_Ga=T,f=32,s=4,v=4,m=1;AAAAAAAA\x1b\\".to_vec();
        for _ in 0..8 {
            wire.extend_from_slice(b"\x1b_Gm=1;BBBBBBBB\x1b\\");
        }
        wire.extend_from_slice(b"\x1b_Gm=0;CCCCCCCC\x1b\\");

        let mut seen = None;
        for &byte in &wire {
            if let SeqEvent::Graphics(payload) = tracker.step(byte) {
                seen = Some(*payload);
            }
        }
        let payload = seen.expect("a payload");
        assert_eq!(payload.chunks(), 10);
        assert_eq!(
            payload.data(),
            None,
            "80 bytes must not be kept under a 40-byte bound"
        );
        assert_eq!(tracker.graphics().kitty, 1, "and it is still one image");
    }

    /// Every payload a feed produced, in order.
    fn payloads(bytes: &[u8]) -> Vec<GraphicsPayload> {
        let mut tracker = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        let mut out = Vec::new();
        for &byte in bytes {
            if let SeqEvent::Graphics(payload) = tracker.step(byte) {
                out.push(*payload);
            }
        }
        out
    }

    /// Kitty and sixel payloads, and the two DCS *questions* that share
    /// sixel's `q` final byte and must not be counted as pictures.
    #[test]
    fn graphics_payloads_are_counted_by_protocol() {
        let kitty = fed(b"\x1b_Gf=24,s=1,v=1,a=T;AAAABBBB\x1b\\");
        assert_eq!(kitty.graphics().kitty, 1);
        assert_eq!(kitty.graphics().sixel, 0);
        // Everything between the introducer and the terminator.
        assert_eq!(kitty.graphics().bytes, 26);
        assert_eq!(fed(b"\x1bPq~~\x1b\\").graphics().bytes, 3, "q~~");

        let sixel = fed(b"\x1bPq#0;2;0;0;0#0~~-~~\x1b\\");
        assert_eq!(sixel.graphics().sixel, 1);
        assert_eq!(sixel.graphics().kitty, 0);

        // Sixel with parameters before the `q`.
        assert_eq!(fed(b"\x1bP0;1;0q~~\x1b\\").graphics().sixel, 1);

        // Neither of these is a picture: both reach `q` through an
        // intermediate byte, which is the whole distinction.
        let termcap = fed(b"\x1bP+q544e\x1b\\").graphics();
        assert_eq!((termcap.kitty, termcap.sixel), (0, 0), "XTGETTCAP");
        let decrqss = fed(b"\x1bP$qm\x1b\\").graphics();
        assert_eq!((decrqss.kitty, decrqss.sixel), (0, 0), "DECRQSS");

        // Two of each accumulate.
        let both = fed(b"\x1b_Ga=T;AA\x1b\\\x1bPq~\x1b\\\x1b_Ga=T;BB\x1b\\");
        assert_eq!((both.graphics().kitty, both.graphics().sixel), (2, 1));
    }

    /// The protocol caps a payload at 4096 bytes and continues with `m=1`,
    /// so every image of consequence arrives in several escapes. Counting
    /// each of them reported one 4.9 KB chart as two pictures — and the
    /// second "picture" had no control block, so nothing could be said
    /// about it either.
    #[test]
    fn a_chunked_kitty_transmission_is_one_image() {
        let wire = b"\x1b_Ga=T,f=32,s=2,v=2,i=1,m=1;AAAA\x1b\\                     \x1b_Gm=1;BBBB\x1b\\\x1b_Gm=0;CCCC\x1b\\";
        let counts = fed(wire).graphics();
        assert_eq!(counts.kitty, 1, "one image, three escapes");

        let payloads = payloads(wire);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].chunks(), 3);
        assert_eq!(payloads[0].data(), Some(&b"AAAABBBBCCCC"[..]));
        // The facts come off the first escape; the continuations carry none.
        assert_eq!(payloads[0].size(), Some((2, 2)));
        assert_eq!(payloads[0].id(), Some(1));
        // And the cost is the whole transmission, not the last escape.
        assert_eq!(counts.bytes, payloads[0].bytes());
    }

    /// A delete takes an image *off* the screen. Counting it as one
    /// transmitted made an application that tears down what it drew
    /// indistinguishable from one that drew twice as much.
    #[test]
    fn a_delete_is_counted_apart_from_the_images() {
        let counts = fed(b"\x1b_Ga=T,f=32,s=1,v=1;AA\x1b\\\x1b_Ga=d,d=I,i=1,q=2\x1b\\").graphics();
        assert_eq!(counts.kitty, 1, "one image");
        assert_eq!(counts.deletes, 1, "and one teardown");
        // The wire cost of both is still counted: a delete is traffic.
        assert!(counts.bytes > 22);
    }

    /// A payload past the capture bound keeps every count and drops the
    /// data, rather than handing back a prefix that would decode into a
    /// plausible-looking wrong picture.
    #[test]
    fn a_payload_past_the_capture_bound_is_counted_and_not_kept() {
        let mut tracker = SeqTracker::new(8);
        let mut seen = None;
        for &byte in b"\x1b_Ga=T,f=32,s=4,v=4;AAAABBBBCCCCDDDD\x1b\\" {
            if let SeqEvent::Graphics(payload) = tracker.step(byte) {
                seen = Some(*payload);
            }
        }
        let payload = seen.expect("a payload");
        assert_eq!(payload.data(), None, "not kept");
        assert_eq!(payload.size(), Some((4, 4)), "but still described");
        assert_eq!(tracker.graphics().kitty, 1, "and still counted");
    }

    /// The control block is metadata; the data is what a decoder wants.
    #[test]
    fn a_payload_carries_the_data_and_not_the_framing() {
        let kitty = payloads(b"\x1b_Ga=T,f=24,s=1,v=1;QUJD\x1b\\");
        assert_eq!(kitty[0].data(), Some(&b"QUJD"[..]));
        assert_eq!(kitty[0].format(), GraphicsFormat::Rgb);

        // Sixel's header ends at the `q`; everything after it is the image.
        let sixel = payloads(b"\x1bP0;1;0q\"1;1;2;6#0;2;100;0;0~~\x1b\\");
        assert_eq!(sixel[0].data(), Some(&b"\"1;1;2;6#0;2;100;0;0~~"[..]));
        assert_eq!(sixel[0].size(), Some((2, 6)));
    }

    /// The kitty graphics *query* was the one probe of the startup set that
    /// got neither an answer nor a diagnosis: `string_final` looked only at
    /// `+q`/`$q`, which an APC never matches, so `note_unanswered` never saw
    /// it and the timeout said nothing.
    #[test]
    fn the_kitty_graphics_query_is_classified() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        let mut events = Vec::new();
        for &b in b"\x1b_Gi=1,a=q;\x1b\\" {
            events.push(t.step(b));
        }
        assert!(
            events.iter().any(|e| matches!(
                e,
                SeqEvent::Query(Query::KittyGraphics { id: Some(1), shape })
                    if shape.contains("_G")
            )),
            "the query must be classified, with its id, for a reply or a \
             timeout note: {events:?}"
        );
        // A query carries no picture, so it is not counted as one.
        assert_eq!(t.graphics().kitty, 0);
    }

    /// A transmission is an instruction, not a question. Classifying one as
    /// a query would put "the application queried the terminal" into the
    /// next timeout of every application that draws.
    #[test]
    fn a_kitty_transmission_is_not_treated_as_a_query() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        let mut events = Vec::new();
        for &b in b"\x1b_Gf=24,a=T;QUJD\x1b\\" {
            events.push(t.step(b));
        }
        assert!(
            !events.iter().any(|e| matches!(e, SeqEvent::Query(_))),
            "a transmit is not a question: {events:?}"
        );
        assert_eq!(t.graphics().kitty, 1);
    }

    /// The links a fed tracker holds, as `(uri, id, label, closed)`.
    fn links(bytes: &[u8]) -> Vec<(String, Option<String>, Option<String>, bool)> {
        fed(bytes)
            .links()
            .iter()
            .map(|l| {
                (
                    l.uri().to_string(),
                    l.id().map(str::to_string),
                    l.label().map(str::to_string),
                    l.closed(),
                )
            })
            .collect()
    }

    /// The reproduction from the issue: the label renders as ordinary text
    /// and the URL has to be somewhere a test can reach.
    #[test]
    fn an_osc8_span_records_its_uri_and_the_text_it_wrapped() {
        let seen = links(b"see \x1b]8;;https://example.invalid/a\x1b\\docs\x1b]8;;\x1b\\ here");
        assert_eq!(
            seen,
            vec![(
                "https://example.invalid/a".to_string(),
                None,
                Some("docs".to_string()),
                true
            )]
        );
    }

    #[test]
    fn a_bel_terminated_span_and_a_multibyte_label_both_survive() {
        // BEL is the other legal OSC terminator, and the one `printf` in a
        // shell reaches for.
        let seen = links("\x1b]8;;http://x/\x07café\x1b]8;;\x07".as_bytes());
        assert_eq!(seen[0].2, Some("café".to_string()));
        assert!(seen[0].3);
    }

    #[test]
    fn the_id_parameter_is_kept_so_multi_span_links_can_be_grouped() {
        let seen = links(
            b"\x1b]8;id=42:x=y;http://x/\x1b\\one\x1b]8;;\x1b\\               \x1b]8;id=42;http://x/\x1b\\two\x1b]8;;\x1b\\",
        );
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].1.as_deref(), Some("42"));
        assert_eq!(seen[1].1.as_deref(), Some("42"));
        assert_eq!(seen[0].2.as_deref(), Some("one"));
        assert_eq!(seen[1].2.as_deref(), Some("two"));
    }

    /// A URI with a query string contains `;` of its own. Splitting on the
    /// last separator, or on all of them, truncates the target silently —
    /// which is precisely the wrong-URL failure this feature exists to catch.
    #[test]
    fn only_the_first_separator_ends_the_parameters() {
        let seen = links(b"\x1b]8;;http://x/?a=1;b=2\x1b\\t\x1b]8;;\x1b\\");
        assert_eq!(seen[0].0, "http://x/?a=1;b=2");
    }

    /// An unterminated span is a real defect — in a real terminal every
    /// character after it joins the link — so it is reported rather than
    /// dropped, and its label is bounded rather than allowed to swallow the
    /// stream.
    #[test]
    fn an_unterminated_span_is_reported_open_and_cannot_bleed() {
        let mut bytes = b"\x1b]8;;http://x/\x1b\\".to_vec();
        bytes.extend(std::iter::repeat_n(b'z', LINK_LABEL_MAX + 100));
        let seen = links(&bytes);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "http://x/");
        assert!(!seen[0].3, "the application never closed it");
        // Still open, so there is no final label — and the bound means the
        // captured bytes are a prefix, which is not an answer either.
        assert_eq!(seen[0].2, None);
    }

    #[test]
    fn a_label_past_the_bound_is_unknown_rather_than_a_prefix() {
        let mut bytes = b"\x1b]8;;http://x/\x1b\\".to_vec();
        bytes.extend(std::iter::repeat_n(b'z', LINK_LABEL_MAX + 1));
        bytes.extend_from_slice(b"\x1b]8;;\x1b\\");
        let seen = links(&bytes);
        assert!(seen[0].3, "closed");
        assert_eq!(
            seen[0].2, None,
            "a prefix of the wrong length is a wrong answer"
        );
    }

    /// A real terminal replaces the current target rather than nesting, so a
    /// second open closes the first — otherwise the second label would be
    /// attributed to the first URI.
    #[test]
    fn a_new_span_supersedes_one_left_open() {
        let seen = links(b"\x1b]8;;http://a/\x1b\\one\x1b]8;;http://b/\x1b\\two\x1b]8;;\x1b\\");
        assert_eq!(seen.len(), 2);
        assert_eq!(
            (seen[0].0.as_str(), seen[0].2.as_deref()),
            ("http://a/", Some("one"))
        );
        assert_eq!(
            (seen[1].0.as_str(), seen[1].2.as_deref()),
            ("http://b/", Some("two"))
        );
    }

    /// A URI past the OSC capture bound is a *prefix*, and opening a span on
    /// a prefix records a link pointing somewhere the application never
    /// named — the wrong-URL failure this feature exists to catch, only
    /// manufactured by termlens rather than by the program under test.
    /// Refused outright, exactly as a truncated `OSC 52` payload is.
    #[test]
    fn an_osc8_past_the_capture_bound_is_refused_rather_than_truncated() {
        let mut bytes = b"\x1b]8;;http://x/".to_vec();
        bytes.extend(std::iter::repeat_n(b'a', OSC_CAPTURE_MAX));
        bytes.extend_from_slice(b"\x1b\\label\x1b]8;;\x1b\\");
        assert!(
            links(&bytes).is_empty(),
            "a truncated URI must not become a link"
        );
    }

    #[test]
    fn a_malformed_osc8_neither_opens_nor_closes() {
        // No second `;` at all: not a link, and not a close either —
        // silently closing would attribute what follows to nothing.
        let seen = links(b"\x1b]8;http://x/\x1b\\t");
        assert!(seen.is_empty());
        // An open, then a malformed one, then text: the text still belongs
        // to the span that is actually open.
        let seen = links(b"\x1b]8;;http://x/\x1b\\a\x1b]8;junk\x1b\\b\x1b]8;;\x1b\\");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].2.as_deref(), Some("ab"));
    }

    #[test]
    fn control_characters_are_not_part_of_a_label() {
        // A newline moves the cursor; it does not spell anything.
        let seen = links(b"\x1b]8;;http://x/\x1b\\a\r\nb\x1b]8;;\x1b\\");
        assert_eq!(seen[0].2.as_deref(), Some("ab"));
    }

    #[test]
    fn the_link_log_is_bounded_and_evicts_the_oldest() {
        let mut bytes = Vec::new();
        for n in 0..LINK_HISTORY + 10 {
            bytes
                .extend_from_slice(format!("\x1b]8;;http://x/{n}\x1b\\l\x1b]8;;\x1b\\").as_bytes());
        }
        let seen = links(&bytes);
        assert_eq!(seen.len(), LINK_HISTORY);
        // Oldest evicted, newest kept.
        assert_eq!(seen[0].0, "http://x/10");
        assert_eq!(
            seen[LINK_HISTORY - 1].0,
            format!("http://x/{}", LINK_HISTORY + 9)
        );
    }

    #[test]
    fn an_application_that_emits_no_links_reports_none() {
        assert!(links(b"plain text\x1b]0;title\x07").is_empty());
    }

    #[test]
    fn decscusr_records_the_parameter_the_application_asked_for() {
        // Never asked is its own state, and not the same as any value.
        assert_eq!(fed(b"hello").cursor_style(), None);
        for ps in 0u8..=6 {
            let bytes = format!("\x1b[{ps} q");
            assert_eq!(
                fed(bytes.as_bytes()).cursor_style(),
                Some(ps),
                "DECSCUSR {ps} was not recorded"
            );
        }
        // An omitted parameter is 0 (a blinking block), per xterm.
        assert_eq!(fed(b"\x1b[ q").cursor_style(), Some(0));
        // The last one wins, which is what makes a restore assertable.
        assert_eq!(fed(b"\x1b[5 q\x1b[2 q").cursor_style(), Some(2));
    }

    /// RIS is a way a program restores the cursor on exit, so a stale
    /// `DECSCUSR` after one would fail the very assertion `cursor_shape`
    /// exists to support — and claim a shape the terminal no longer holds.
    #[test]
    fn a_hard_reset_returns_the_cursor_to_the_terminals_default() {
        assert_eq!(fed(b"\x1b[5 q").cursor_style(), Some(5));
        assert_eq!(fed(b"\x1b[5 q\x1bc").cursor_style(), None);
        // …and a style set *after* the reset is still recorded.
        assert_eq!(fed(b"\x1b[5 q\x1bc\x1b[2 q").cursor_style(), Some(2));
    }

    /// An open span cannot survive a hard reset, so it is closed with what
    /// it had. The *log* is a record of what the application emitted and is
    /// deliberately kept, like the bell count and the clipboard.
    #[test]
    fn a_hard_reset_closes_an_open_span_and_keeps_the_log() {
        let seen = links(b"\x1b]8;;http://x/\x1b\\lab\x1bcafter");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "http://x/");
        assert_eq!(
            seen[0].2.as_deref(),
            Some("lab"),
            "the text before the reset"
        );
        assert!(seen[0].3, "closed by the reset");
        // Text after the reset belongs to no span.
        let seen = links(b"\x1b]8;;http://x/\x1b\\a\x1bcbbb");
        assert_eq!(seen[0].2.as_deref(), Some("a"));
    }

    /// The two-character escapes that are *not* RIS must keep passing
    /// through untouched — `ESC 7` (save cursor) is next to it in the table.
    #[test]
    fn other_two_character_escapes_do_not_reset_anything() {
        for seq in [
            &b"\x1b[5 q\x1b7"[..],
            b"\x1b[5 q\x1b8",
            b"\x1b[5 q\x1bD",
            b"\x1b[5 q\x1bM",
        ] {
            assert_eq!(
                fed(seq).cursor_style(),
                Some(5),
                "{seq:?} reset the cursor style"
            );
        }
    }

    #[test]
    fn an_undefined_or_misshapen_decscusr_leaves_the_last_known_style() {
        // 7+ is undefined; xterm ignores it, and inventing a shape here
        // would report one the application never asked for.
        assert_eq!(fed(b"\x1b[5 q\x1b[7 q").cursor_style(), Some(5));
        assert_eq!(fed(b"\x1b[7 q").cursor_style(), None);
        // A private prefix or a second parameter makes it a different
        // sequence, not a DECSCUSR with extra decoration.
        assert_eq!(fed(b"\x1b[?5 q").cursor_style(), None);
        assert_eq!(fed(b"\x1b[5;2 q").cursor_style(), None);
    }

    /// `SP` is the intermediate of DECSCUSR *and* of SL/SR (`CSI Ps SP @`
    /// / `A`), which scroll the screen sideways. Accepting the intermediate
    /// must not turn those into a cursor style, nor let them fall through
    /// into the query table below, which assumes no intermediate.
    #[test]
    fn the_other_space_intermediate_sequences_are_not_cursor_styles() {
        for seq in [&b"\x1b[2 @"[..], b"\x1b[2 A", b"\x1b[1 t"] {
            let mut t = fed(seq);
            assert_eq!(t.cursor_style(), None, "{seq:?} set a cursor style");
            assert_eq!(t.step(b'x'), SeqEvent::None);
        }
    }

    #[test]
    fn plain_text_is_ground() {
        assert!(!fed(b"hello world\r\n").mid_sequence());
    }

    #[test]
    fn split_csi_is_mid_sequence_until_final_byte() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
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
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        let events: Vec<SeqEvent> = b"\x1b[?2026h".iter().map(|&b| t.step(b)).collect();
        assert_eq!(*events.last().unwrap(), SeqEvent::SyncBegin);
        assert!(t.in_sync_update());
        let events: Vec<SeqEvent> = b"\x1b[?2026l".iter().map(|&b| t.step(b)).collect();
        assert_eq!(*events.last().unwrap(), SeqEvent::SyncEnd);
        assert!(!t.in_sync_update());
    }

    #[test]
    fn sync_2026_is_recognized_anywhere_in_a_multi_mode_list() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        t.feed(b"\x1b[?2026;25h");
        assert!(t.in_sync_update());
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        t.feed(b"\x1b[?25;2026h");
        assert!(t.in_sync_update());
    }

    #[test]
    fn lookalike_sequences_do_not_toggle_sync() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
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
    fn base64_decodes_or_refuses() {
        assert_eq!(
            decode_base64(b"dGhlIHRpdGxl").as_deref(),
            Some(&b"the title"[..])
        );
        // Every padding shape.
        assert_eq!(decode_base64(b"YQ==").as_deref(), Some(&b"a"[..]));
        assert_eq!(decode_base64(b"YWI=").as_deref(), Some(&b"ab"[..]));
        assert_eq!(decode_base64(b"YWJj").as_deref(), Some(&b"abc"[..]));
        // Unpadded is common in the wild and unambiguous.
        assert_eq!(decode_base64(b"YQ").as_deref(), Some(&b"a"[..]));
        assert_eq!(decode_base64(b"YWI").as_deref(), Some(&b"ab"[..]));
        // A real write of nothing.
        assert_eq!(decode_base64(b"").as_deref(), Some(&b""[..]));

        // Refusals: out of alphabet, impossible length, misplaced padding.
        assert_eq!(decode_base64(b"not base64!"), None);
        assert_eq!(decode_base64(b"YWJjZ"), None);
        assert_eq!(decode_base64(b"Y===="), None);
        // One leftover character encodes nothing.
        assert_eq!(decode_base64(b"Y"), None);
    }

    #[test]
    fn osc52_writes_are_captured_with_their_target() {
        let t = fed(b"\x1b]52;c;V2lyZSB1cCB0aGUgUFRZIHJlYWRlcg==\x07");
        let clip = t.clipboard().expect("a write was observed");
        assert_eq!(clip.targets(), "c");
        assert_eq!(clip.text(), Some("Wire up the PTY reader"));

        // Primary selection, ST-terminated instead of BEL.
        let t = fed(b"\x1b]52;p;c2VsZWN0ZWQgd29yZHM=\x1b\\");
        let clip = t.clipboard().expect("a write was observed");
        assert_eq!(clip.targets(), "p");
        assert_eq!(clip.text(), Some("selected words"));

        // No target named: the terminal would pick its default, and we
        // report what the application actually sent rather than guessing.
        let t = fed(b"\x1b]52;;Y29waWVk\x07");
        assert_eq!(t.clipboard().expect("write").targets(), "");

        // The most recent write wins.
        let t = fed(b"\x1b]52;c;YQ==\x07\x1b]52;c;YWI=\x07");
        assert_eq!(t.clipboard().expect("write").text(), Some("ab"));
    }

    #[test]
    fn an_undecodable_payload_is_not_an_empty_clipboard() {
        // The distinction the whole feature rests on: a test asserting
        // `text() == Some("")` must not pass on a payload we could not read.
        let empty = fed(b"\x1b]52;c;\x07");
        assert_eq!(empty.clipboard().expect("write").text(), Some(""));

        let broken = fed(b"\x1b]52;c;!!!not base64!!!\x07");
        assert_eq!(broken.clipboard().expect("write").text(), None);
        assert_eq!(broken.clipboard().expect("write").targets(), "c");

        // Valid base64 that is not text.
        let not_utf8 = fed(b"\x1b]52;c;//8=\x07");
        assert_eq!(not_utf8.clipboard().expect("write").text(), None);
    }

    #[test]
    fn a_payload_past_the_capture_bound_is_reported_as_unreadable() {
        // A prefix of base64 can still decode — to the wrong thing. Any
        // truncation must therefore read as "could not decode".
        let mut stream = b"\x1b]52;c;".to_vec();
        stream.extend(std::iter::repeat_n(b'A', OSC_CAPTURE_MAX + 64));
        stream.push(0x07);
        let t = fed(&stream);
        assert_eq!(t.clipboard().expect("write").text(), None);
    }

    #[test]
    fn a_clipboard_read_is_a_query_not_a_write() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        let events: Vec<SeqEvent> = b"\x1b]52;c;?\x07".iter().map(|&b| t.step(b)).collect();
        assert!(matches!(
            events.last(),
            Some(SeqEvent::Query(Query::Unanswerable(_)))
        ));
        assert!(t.clipboard().is_none(), "a read must not invent a write");
    }

    #[test]
    fn an_end_that_closes_no_begin_is_not_a_frame() {
        // Applications reset terminal modes defensively at startup and on
        // crash, and such a reset string contains `?2026l`. It must not end
        // a frame that never began.
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        let events: Vec<SeqEvent> = b"\x1b[?2026l".iter().map(|&b| t.step(b)).collect();
        assert!(!events.contains(&SeqEvent::SyncEnd));
        assert!(!t.in_sync_update());

        // Taken verbatim from a real crash handler.
        let reset = b"\x1b[?2026l\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?2004l\x1b[?1049l";
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        let events: Vec<SeqEvent> = reset.iter().map(|&b| t.step(b)).collect();
        assert!(!events.contains(&SeqEvent::SyncEnd));

        // And the End of a real frame still ends it, once only.
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        let events: Vec<SeqEvent> = b"\x1b[?2026h\x1b[?2026l\x1b[?2026l"
            .iter()
            .map(|&b| t.step(b))
            .collect();
        assert_eq!(
            events.iter().filter(|e| **e == SeqEvent::SyncEnd).count(),
            1
        );
    }

    #[test]
    fn sync_survives_an_aborted_csi_inside_the_update() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        t.feed(b"\x1b[?2026h\x1b[31\x18"); // CAN aborts the SGR, not the frame
        assert!(t.in_sync_update());
        t.feed(b"\x1b[?2026l");
        assert!(!t.in_sync_update());
    }

    fn queries_of(bytes: &[u8]) -> Vec<Query> {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
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

    /// The pixel reports used to be lumped in with the whole `CSI t` family
    /// as unanswerable. They are arithmetic, so they are now their own
    /// questions — while the rest of the family stays declined.
    #[test]
    fn pixel_geometry_queries_are_classified_apart_from_the_rest() {
        assert_eq!(queries_of(b"\x1b[14t"), vec![Query::WindowSizePixels]);
        assert_eq!(queries_of(b"\x1b[16t"), vec![Query::CellSizePixels]);
        assert_eq!(queries_of(b"\x1b[18t"), vec![Query::TextAreaSize]);
        for shape in [&b"\x1b[11t"[..], b"\x1b[13t", b"\x1b[19t", b"\x1b[20t"] {
            assert!(
                matches!(queries_of(shape).as_slice(), [Query::Unanswerable(_)]),
                "still declined: {shape:?}"
            );
        }
    }

    /// The names must come out intact, since the reply has to echo each one
    /// back — and they are longer than the 24-byte diagnostic buffer can
    /// hold, which is why they come from the header capture instead.
    #[test]
    fn xtgettcap_carries_the_names_it_was_asked_for() {
        // "TN" and "colors", hex-encoded, as xterm-style clients send them.
        let q = queries_of(b"\x1bP+q544e;636f6c6f7273\x1b\\");
        assert_eq!(
            q,
            vec![Query::RequestTermcap {
                names: "544e;636f6c6f7273".into(),
                shape: "^[P+q544e;636f6c6f7273^[\\".into(),
            }]
        );
    }

    #[test]
    fn a_kitty_probe_without_an_id_still_classifies() {
        assert_eq!(
            queries_of(b"\x1b_Ga=q;\x1b\\"),
            vec![Query::KittyGraphics {
                id: None,
                shape: "^[_Ga=q;^[\\".into()
            }]
        );
    }

    #[test]
    fn recognizes_unanswerable_questions_with_their_shape() {
        let q = queries_of(b"\x1b[?u");
        assert_eq!(q, vec![Query::Unanswerable("^[[?u".into())]);
        // 14t and 16t are classified in their own right now; 13t is not.
        let q = queries_of(b"\x1b[13t");
        assert_eq!(q, vec![Query::Unanswerable("^[[13t".into())]);
        // XTGETTCAP is answerable now; DECRQSS still is not.
        let q = queries_of(b"\x1bP$qm\x1b\\"); // DECRQSS
        assert_eq!(q, vec![Query::Unanswerable("^[P$qm^[\\".into())]);
        let q = queries_of(b"\x1b[=c"); // DA3
        assert_eq!(q, vec![Query::Unanswerable("^[[=c".into())]);
        let q = queries_of(b"\x1b]12;?\x07"); // cursor color
        assert_eq!(q, vec![Query::Unanswerable("^[]12;?^G".into())]);
        // Any CSI …n is a DSR-family status request by definition.
        let q = queries_of(b"\x1b[6;1n");
        assert_eq!(q, vec![Query::Unanswerable("^[[6;1n".into())]);
    }

    #[test]
    fn recognizes_decrqm_mode_requests() {
        assert_eq!(queries_of(b"\x1b[?2026$p"), vec![Query::RequestMode(2026)]);
        assert_eq!(queries_of(b"\x1b[?2004$p"), vec![Query::RequestMode(2004)]);
        assert_eq!(queries_of(b"\x1b[?1$p"), vec![Query::RequestMode(1)]);
        // ANSI-mode DECRQM (no `?`) is recognized but not answerable.
        assert_eq!(
            queries_of(b"\x1b[4$p"),
            vec![Query::Unanswerable("^[[4$p".into())]
        );
    }

    #[test]
    fn recognizes_the_remaining_unanswerable_families() {
        // DECRQSS: "what is the current setting of ...?"
        assert_eq!(
            queries_of(b"\x1bP$qm\x1b\\"),
            vec![Query::Unanswerable("^[P$qm^[\\".into())]
        );
        // Palette query and clipboard read.
        assert_eq!(
            queries_of(b"\x1b]4;1;?\x07"),
            vec![Query::Unanswerable("^[]4;1;?^G".into())]
        );
        assert_eq!(
            queries_of(b"\x1b]52;c;?\x07"),
            vec![Query::Unanswerable("^[]52;c;?^G".into())]
        );
    }

    #[test]
    fn setting_a_palette_colour_is_not_a_query() {
        // `OSC 4;1;rgb:...` sets rather than asks — only the `?` form is
        // a question.
        assert!(queries_of(b"\x1b]4;1;rgb:ff/00/00\x07").is_empty());
        assert!(queries_of(b"\x1b]52;c;aGVsbG8=\x07").is_empty());
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
    fn osc_0_and_2_set_the_title_via_bel_or_st() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        assert_eq!(&*t.title(), "");
        t.feed(b"\x1b]2;hello world\x07");
        assert_eq!(&*t.title(), "hello world");
        t.feed("\x1b]0;second ✓\x1b\\".as_bytes());
        assert_eq!(&*t.title(), "second ✓");
    }

    #[test]
    fn titles_longer_than_the_diagnostic_capture_are_kept_whole() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        let title = "t".repeat(80); // seq_buf truncates at 24; titles must not
        t.feed(format!("\x1b]2;{title}\x07").as_bytes());
        assert_eq!(&*t.title(), title.as_str());
    }

    #[test]
    fn title_survives_chunked_delivery() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        t.feed(b"\x1b]2;split");
        t.feed(b" title\x07");
        assert_eq!(&*t.title(), "split title");
    }

    #[test]
    fn title_keeps_embedded_semicolons() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        t.feed(b"\x1b]0;a;b;c\x07");
        assert_eq!(&*t.title(), "a;b;c");
    }

    #[test]
    fn icon_only_and_aborted_titles_do_not_change_the_title() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        t.feed(b"\x1b]2;kept\x07");
        t.feed(b"\x1b]1;icon only\x07"); // OSC 1: icon name, not the title
        assert_eq!(&*t.title(), "kept");
        t.feed(b"\x1b]2;aborted\x18"); // CAN aborts the string
        assert_eq!(&*t.title(), "kept");
        t.feed(b"\x1b]2;also aborted\x1b[31m"); // ESC starts a new sequence
        assert_eq!(&*t.title(), "kept");
        t.feed(b"\x1b]2;\x07"); // explicitly set empty: cleared
        assert_eq!(&*t.title(), "");
    }

    #[test]
    fn split_utf8_is_mid_sequence() {
        let bytes = "汉".as_bytes(); // 3 bytes
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE);
        t.feed(&bytes[..1]);
        assert!(t.mid_sequence());
        t.feed(&bytes[1..2]);
        assert!(t.mid_sequence());
        t.feed(&bytes[2..]);
        assert!(!t.mid_sequence());
    }
}
