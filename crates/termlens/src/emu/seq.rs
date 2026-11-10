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
//! It also tracks screen state the vt100 backend does not expose: the
//! **window title** (`OSC 0`/`OSC 2`), kept whole in its own buffer — the
//! diagnostic capture below truncates at 24 bytes, real titles don't fit —
//! focus reporting (mode 1004), the cursor shape (`DECSCUSR`), and the
//! mouse tracking modes twice over: the **set** the application asked for,
//! which vt100 collapses into one value, and the **one protocol** reports
//! are made in, which vt100 keeps after a reset xterm ends it on.
//!
//! And it holds the **character-set state** vt100 ignores: which glyph set
//! `ESC ( ) * + Ps` designated into G0–G3, which of G0/G1 `SI`/`SO` has
//! lock-shifted, and whether `ESC N` (SS2) / `ESC O` (SS3) has invoked G2/G3
//! for the next character only. That is what lets the emulator hand the grid
//! the glyph a byte *draws* — `ESC ( 0 l q q q k` is `┌───┐`, not five
//! letters — which is how ncurses draws every border (`smacs`/`rmacs` on an
//! xterm terminfo are exactly `ESC ( 0` / `ESC ( B`). A mixed line of text
//! and box-drawing uses the single shift instead, so `ESC * 0 ESC N l` is
//! `┌` and the character after it is back to the locked set. Two sets are
//! translated: DEC Special Graphics, and the UK set (`ESC ( A`), whose one
//! difference from ASCII is `£` at `#`; every other designation — the
//! alternate ROMs, the other national sets — is acknowledged and reads as
//! ASCII. `DECSC`/`DECRC` save and restore this state alongside the cursor,
//! and `RIS` and `DECSTR` return it to power-on. Locking shifts remain G0/G1
//! (`SO`/`SI`); `LS2`/`LS3` are not modelled. The tracker decides, the
//! emulator rewrites: the bytes the
//! parsers see are then a translated stream rather than a sub-slice of the
//! read, which `emu/vt100.rs` stages.
//!
//! The same split holds the **tab stops** (see [`TabStops`]). vt100 has a
//! hardcoded eight and no way to be told otherwise, so `HTS`, `TBC`, `CHT`
//! and `CBT` reached its dispatch table and vanished — and an application
//! that lays a table out by setting its own stops, which is what the
//! capabilities are for, drew every column in the wrong place. The set
//! lives here; the *cursor column* every one of those operations needs
//! lives in the grid, so the tracker emits a [`TabOp`] and the emulator
//! resolves it against the position it holds, rewriting a motion as `CHA`
//! (`CSI n G`) — a sequence the backend does dispatch, the same way DEC
//! Special Graphics became a glyph substitution. Plain `HT` is rewritten
//! too, or vt100's fixed eight and these stops would disagree the moment an
//! application set one.

use std::sync::Arc;

use super::MouseEncoding;
use crate::graphics::{GraphicsBuilder, GraphicsCounts, GraphicsPayload, Placement, SixelMeter};
use crate::screen::{Clipboard, Link, MouseMode, MouseModes};

/// OSC strings are captured whole (titles must not truncate), but bounded:
/// a buggy or hostile stream must not grow memory without limit. No real
/// title comes anywhere near this.
pub(super) const OSC_CAPTURE_MAX: usize = 64 * 1024;

/// How much of a kitty control block is kept: 256 KiB, the length past
/// which kitty drops an escape that is still arriving
/// (`MAX_ESCAPE_CODE_LENGTH` in its `vt-parser.c`, a quarter of its 1 MiB
/// read buffer; it reads a longer one only when the whole of it is already
/// in that buffer). A block that names every key of the protocol once,
/// each at its longest value, is under 400 bytes: the bound is there for
/// the stream that never ends one.
const KITTY_CONTROL_MAX: usize = 256 * 1024;

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

/// Columns between the tab stops a terminal powers on with. Eight is the
/// value every terminfo, every shell and vt100's own hardcoded `col_tab` is
/// written against.
const TAB_INTERVAL: u16 = 8;

/// Whether the power-on layout has a stop at `col`.
///
/// Column 0 is one of them, as it is in xterm's `TabReset` and alacritty's
/// `TabStops::new`. Forward motion scans strictly right of the cursor and so
/// can never land there; it matters only to `CBT`, which would clamp to 0
/// anyway. Keeping it makes the set the plain "every eighth column" the
/// resize rule extends, with no column that has to be special-cased.
fn default_stop(col: u16) -> bool {
    col % TAB_INTERVAL == 0
}

/// Where the tab stops are: one flag per column, `cols` wide.
///
/// The whole set lives in the tracker because vt100 holds no tab state at
/// all — its `HT` is a fixed eight — so there is nothing here to keep in
/// step with the backend, only a column to hand back to it.
#[derive(Debug)]
struct TabStops {
    stops: Vec<bool>,
}

impl TabStops {
    fn new(cols: u16) -> Self {
        Self {
            stops: (0..cols).map(default_stop).collect(),
        }
    }

    /// The rightmost column, which every motion clamps to.
    ///
    /// Zero for an empty set, which makes both motions no-ops rather than
    /// panics; the builder floors a terminal at two columns, so an empty set
    /// only ever arises in a unit test.
    fn last_column(&self) -> u16 {
        u16::try_from(self.stops.len())
            .unwrap_or(u16::MAX)
            .saturating_sub(1)
    }

    /// Grow or shrink to `cols`.
    ///
    /// **Decision:** columns the grid did not have before get the power-on
    /// every-eighth pattern, and stops inside the old width are left exactly
    /// as they were. A resize is not a reset — an application that set its
    /// own stops and then had its window widened would otherwise find them
    /// gone — and there is no better answer for territory that never
    /// existed than the layout the terminal would have powered on with.
    /// Narrowing drops the columns that no longer exist; widening again does
    /// not bring their stops back, since the set no longer holds them. This
    /// is what alacritty does, and it is the simple end of the trade.
    fn set_cols(&mut self, cols: u16) {
        let old = u16::try_from(self.stops.len()).unwrap_or(u16::MAX);
        self.stops.resize(usize::from(cols), false);
        for col in old..cols {
            self.stops[usize::from(col)] = default_stop(col);
        }
    }

    /// Back to the power-on layout, for `RIS` and `DECSTR`.
    fn reset(&mut self) {
        let cols = u16::try_from(self.stops.len()).unwrap_or(u16::MAX);
        for col in 0..cols {
            self.stops[usize::from(col)] = default_stop(col);
        }
    }

    /// `HTS`: a stop at `col`. Out-of-range columns are dropped rather than
    /// clamped — clamping would set a stop the application did not ask for.
    fn set(&mut self, col: u16) {
        if let Some(stop) = self.stops.get_mut(usize::from(col)) {
            *stop = true;
        }
    }

    /// `TBC 0`: clear the stop at `col`.
    fn clear(&mut self, col: u16) {
        if let Some(stop) = self.stops.get_mut(usize::from(col)) {
            *stop = false;
        }
    }

    /// `TBC 3`: clear every stop. Forward motion then runs to the last
    /// column and back-tab to column 0, which is what a terminal with no
    /// stops does.
    fn clear_all(&mut self) {
        self.stops.fill(false);
    }

    /// The most steps a motion can usefully take: one per column.
    fn steps_bound(&self) -> u16 {
        u16::try_from(self.stops.len()).unwrap_or(u16::MAX)
    }

    /// The column `count` stops right of `col`, or the last column if the
    /// stops run out first.
    fn forward(&self, col: u16, count: u16) -> u16 {
        let last = self.last_column();
        // vt100 reports a column past the end while a wrap is pending, and
        // clamps it in `col_tab`; clamping here keeps the rewrite agreeing
        // with the move it replaces.
        let mut at = col.min(last);
        // Every step moves at least one column, so more steps than there are
        // columns cannot move further — and `CSI 65535 I` is a parameter an
        // application is free to send.
        for _ in 0..count.min(self.steps_bound()) {
            match ((at.saturating_add(1))..=last).find(|&c| self.stops[usize::from(c)]) {
                Some(next) => at = next,
                None => return last,
            }
        }
        at
    }

    /// The column `count` stops left of `col`, or column 0 if the stops run
    /// out first.
    ///
    /// Each step goes to the nearest stop *strictly* left of where it
    /// started, which is what xterm and alacritty both do: a cursor sitting
    /// one past a stop it just tabbed to and then wrote over moves to that
    /// stop, not to the one before it.
    fn back(&self, col: u16, count: u16) -> u16 {
        let mut at = col.min(self.last_column());
        for _ in 0..count.min(self.steps_bound()) {
            match (0..at).rev().find(|&c| self.stops[usize::from(c)]) {
                Some(prev) => at = prev,
                None => return 0,
            }
        }
        at
    }
}

/// A tab-stop operation the tracker recognized but cannot carry out alone:
/// every one of them is relative to the cursor column, which lives in the
/// grid. The emulator resolves it against the position it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TabOp {
    /// `HTS` (`ESC H`): set a stop at the cursor column.
    Set,
    /// `TBC` / `TBC 0` (`CSI g`): clear the stop at the cursor column.
    ClearAtCursor,
    /// `TBC 3` (`CSI 3 g`): clear every stop.
    ClearAll,
    /// `HT` (`\t`) or `CHT` (`CSI n I`): forward `n` stops.
    Forward(u16),
    /// `CBT` (`CSI n Z`): back `n` stops. Also what an application echoing
    /// `Shift-Tab` emits on its output side.
    Back(u16),
}

/// Which glyph set a G0–G3 designation currently names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Charset {
    /// ASCII — and every designation this crate has no table for: the
    /// alternate ROMs and the national replacement sets other than the UK
    /// one, which differ from it in a handful of positions.
    Ascii,
    /// The DEC United Kingdom set (`ESC ( A`): ASCII with `£` at `#`.
    Uk,
    /// DEC Special Graphics (`ESC ( 0`): the line-drawing set.
    DecSpecialGraphics,
}

/// What `DECSC` saves of the character-set state — see
/// [`ScreenFrame::saved_charsets`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SavedCharsets {
    g0: Charset,
    g1: Charset,
    g2: Charset,
    g3: Charset,
    shifted_out: bool,
}

/// What the tracker follows of one screen — the main one or the alternate —
/// beside vt100, which keeps the same state per grid and exposes none of it.
///
/// The margin and origin mode are kept for the cursor position report,
/// which counts rows from the top margin under origin mode (#689). They
/// follow vt100's rules rather than xterm's wherever the two differ, since
/// a report is read back as a `CUP` that vt100 carries out: a region per
/// grid, `1049` clearing the alternate grid's, a resize clearing both.
/// The charset slot is xterm's `screen->sc[screen->whichBuf]`, one per
/// screen, so a restore on the main screen never reads what the alternate
/// screen saved (#685).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ScreenFrame {
    /// The top margin `DECSTBM` set, 0-based; 0 with no region set.
    top: u16,
    /// Origin mode (`DECOM`, `CSI ? 6 h`).
    origin: bool,
    /// The origin mode `DECSC` saved and `DECRC` restores. vt100 saves it
    /// with the cursor; the other saves, which vt100 does not implement,
    /// leave it alone.
    saved_origin: bool,
    /// The charset half of the state a cursor save keeps: G0–G3 and which
    /// of G0/G1 is locked in. A restore with nothing saved returns to the
    /// power-on defaults, as xterm does. `None` after RIS and `DECSTR`, or
    /// a restore after a reset would resurrect a designation from before
    /// it.
    saved_charsets: Option<SavedCharsets>,
}

/// The glyph a byte draws in DEC Special Graphics, where it differs from
/// ASCII: the 32 bytes `0x5f..=0x7e`. Everything below `_` draws as itself.
/// The mapping is xterm's, which is also what every terminfo `acsc` string
/// is written against.
fn dec_special_graphics(b: u8) -> Option<&'static str> {
    Some(match b {
        b'_' => " ",        // blank
        b'`' => "\u{25c6}", // ◆ diamond
        b'a' => "\u{2592}", // ▒ checkerboard
        b'b' => "\u{2409}", // ␉ HT
        b'c' => "\u{240c}", // ␌ FF
        b'd' => "\u{240d}", // ␍ CR
        b'e' => "\u{240a}", // ␊ LF
        b'f' => "\u{b0}",   // ° degree
        b'g' => "\u{b1}",   // ± plus-minus
        b'h' => "\u{2424}", // ␤ NL
        b'i' => "\u{240b}", // ␋ VT
        b'j' => "\u{2518}", // ┘
        b'k' => "\u{2510}", // ┐
        b'l' => "\u{250c}", // ┌
        b'm' => "\u{2514}", // └
        b'n' => "\u{253c}", // ┼
        b'o' => "\u{23ba}", // ⎺ scan line 1
        b'p' => "\u{23bb}", // ⎻ scan line 3
        b'q' => "\u{2500}", // ─ scan line 5
        b'r' => "\u{23bc}", // ⎼ scan line 7
        b's' => "\u{23bd}", // ⎽ scan line 9
        b't' => "\u{251c}", // ├
        b'u' => "\u{2524}", // ┤
        b'v' => "\u{2534}", // ┴
        b'w' => "\u{252c}", // ┬
        b'x' => "\u{2502}", // │
        b'y' => "\u{2264}", // ≤
        b'z' => "\u{2265}", // ≥
        b'{' => "\u{3c0}",  // π
        b'|' => "\u{2260}", // ≠
        b'}' => "\u{a3}",   // £
        b'~' => "\u{b7}",   // · bullet
        _ => return None,
    })
}

/// SS2/SS3 invoke G2/G3 for exactly one character, then the locking
/// shift resumes. Stored as which *slot* to read, not a copy of the set:
/// a designation between the shift and the character still applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SingleShift {
    G2,
    G3,
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
    /// `DECSTR` (`CSI ! p`) completed: the tracker has already returned
    /// its own state to the defaults; the emulator must do the same for
    /// the modes it holds.
    SoftReset,
    /// `RIS` (`ESC c`) completed: the tracker has already returned its
    /// own state to power-on, and the emulator must save the rows that
    /// scrolled before this byte, because the backend's history is about
    /// to be rebuilt empty (#391).
    HardReset,
    /// `ED 3` (`CSI 3 J`) completed: erase the saved lines. vt100 empties
    /// the active grid's history for it, so this is RIS's case without the
    /// reset: the emulator saves the rows that scrolled before this byte,
    /// and the history it reads next starts over.
    ScrollbackErased,
    /// `CSI ? 47`, `? 1047` or `? 1049`, set or reset, is about to switch
    /// screens. Every grid accessor reads the screen on display, so on the
    /// way in (`h`) the rows that scrolled off the main screen before this
    /// byte have to be copied before it is fed, or they are out of reach
    /// until the main screen is back — and gone if a reset comes first
    /// (#622). On the way out (`l`, `leaving`) the main screen has to be
    /// read back as soon as it is on display, before anything after this
    /// byte scrolls it (#682).
    AlternateScreen { leaving: bool },
    /// The application asked the terminal a question.
    Query(Query),
    /// An inline graphics payload completed. Carried out of the tracker
    /// rather than stored in it because the placement — where the cursor
    /// stood — is a fact about the grid, which only the emulator holds; so
    /// is moving the cursor past the image, which the [`Placement`] sizes.
    Graphics(Box<GraphicsPayload>, Placement),
    /// Both at once: a kitty query that kitty took as the last chunk of the
    /// upload in progress, which completes the upload's image and is then
    /// answered (#672).
    GraphicsQuery(Box<GraphicsPayload>, Placement, Query),
    /// A tab-stop operation completed, and needs the cursor column to
    /// finish — carried out for the same reason a graphics placement is.
    Tabs(TabOp),
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
        /// The `I=` value, if it gave one: kitty refuses a command naming
        /// both an image id and an image number (#672).
        number: Option<u32>,
        /// The `p=` value, if it gave one, which that refusal echoes.
        placement: Option<u32>,
        /// The `q=` value, 0 when absent: 1 suppresses an `OK`, 2 every
        /// reply (#639).
        quiet: u32,
        /// What kitty's loader made of the probe's data (#672).
        load: KittyLoad,
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
    /// DSR/DA/XTWINOPS reports, a kitty transmission or placement kitty
    /// would answer, …). Carries a printable rendering for diagnostics.
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

/// What kitty's loader made of a query's data, which decides whether kitty
/// answers it, and with what (#672).
///
/// A query with an id and data in the escape goes to the loader like any
/// transmission (`grman_handle_command` hands `a=q` to `handle_add_command`
/// in kitty's `graphics.c`), so in the middle of an upload it is that
/// upload's next chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KittyLoad {
    /// Loaded — tested on its own, or as the last chunk of the upload it
    /// joined. Taken to succeed, as a placement is: whether kitty could
    /// decode the data is not knowable here. kitty answers `OK`.
    Loaded,
    /// Joined a load that waits for more chunks (`m=1`). kitty has nothing
    /// to say: no data loaded yet, and no error.
    Pending,
    /// Sent to a load whose image kitty had already removed: kitty answers
    /// `EILSEQ` ("More payload loading refers to non-existent image").
    Gone,
}

/// What a kitty escape arriving in the middle of a chunked upload does to
/// it — kitty's `grman_handle_command` (`graphics.c`), case by case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MidUpload {
    /// The upload's next chunk. A direct transmission with no action, or
    /// with `t` or `T`, continues the load in progress in kitty's
    /// `handle_add_command`, and one with `f` in
    /// `handle_animation_frame_load_command`; both then read only `m=` (and
    /// `q=`) off the escape — "Subsequent chunks must have only the m and
    /// optionally q keys" (the protocol's "Remote client" section).
    Continues,
    /// Abandons it. "If an image is being loaded in chunks and the upload
    /// is not complete when any delete command is received, the partial
    /// upload must be aborted" (the protocol's "Deleting images"); and a
    /// transmission whose `t=` names a file or shared memory starts a new
    /// image in `handle_add_command`, dropping the load.
    Aborts,
    /// Leaves it alone: `a=p`, `a=a`, `a=c` and any other action have
    /// their own `case` in `grman_handle_command`, none of which touches
    /// the load in progress (#636).
    Beside,
}

impl MidUpload {
    fn of(keys: &[u8]) -> Self {
        let action = keys
            .split(|&b| b == b',')
            .find_map(|pair| pair.strip_prefix(b"a="));
        match action {
            None | Some(b"t" | b"T" | b"f") => {
                let direct = keys
                    .split(|&b| b == b',')
                    .find_map(|pair| pair.strip_prefix(b"t="))
                    .is_none_or(|medium| medium == b"d");
                if direct {
                    MidUpload::Continues
                } else {
                    MidUpload::Aborts
                }
            }
            Some(b"d") => MidUpload::Aborts,
            Some(_) => MidUpload::Beside,
        }
    }
}

/// The reply kitty owes a transmission or a placement, which termlens does
/// not send (#680).
///
/// kitty answers one that names an image — `i=…;OK`, or an error — through
/// `finish_command_response` in its `graphics.c`: only when the command
/// carries `i=` or `I=`, an `OK` only once the image's data has loaded (the
/// last chunk of an upload), `q=1` suppressing the `OK` and `q=2` every
/// reply. That `OK` says the image loaded, which termlens cannot know: it
/// opens no file the program under test points at and decodes nothing
/// without the `decode` feature. So the reply is not invented. The command
/// is named in the next timeout instead, as an unanswered query is, since an
/// application that waits for the reply waits for ever — unless it asked
/// for quiet, when kitty would have sent nothing either. A command naming
/// both `i=` and `I=` is refused with an error, which only `q=2` silences.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KittyAsk {
    /// The `i=` and `I=` of the command that started the load, nonzero.
    id: bool,
    number: bool,
    /// Its `q=`, 0 when absent.
    quiet: u32,
    /// Its printable rendering, for the timeout note.
    shape: String,
}

impl KittyAsk {
    /// What a transmission (`a=t`, `a=T`, no action) or a placement (`a=p`)
    /// asks for; `None` for any other action. Frames (`a=f`) and composes
    /// (`a=c`), which kitty answers too, are not covered.
    fn of(keys: &[u8], shape: String) -> Option<Self> {
        let action = keys
            .split(|&b| b == b',')
            .find_map(|pair| pair.strip_prefix(b"a="));
        if !matches!(action, None | Some(b"t" | b"T" | b"p")) {
            return None;
        }
        let nonzero = |name: &[u8]| kitty_key(keys, name).is_some_and(|value| value != 0);
        Some(Self {
            id: nonzero(b"i="),
            number: nonzero(b"I="),
            quiet: kitty_key(keys, b"q=").unwrap_or(0),
            shape,
        })
    }

    /// kitty's refusal of a command naming both an image id and an image
    /// number, sent at once — on the escape itself, before any load.
    fn refused(&self) -> bool {
        self.id && self.number && self.quiet < 2
    }

    /// The `OK` kitty sends once the data has loaded. `quiet` is the last
    /// escape's `q=`: a nonzero one overrides the first's, as kitty's `if
    /// (g->quiet) lg->quiet = g->quiet` does for a continuation chunk.
    fn answered(&self, quiet: u32) -> bool {
        let quiet = if quiet != 0 { quiet } else { self.quiet };
        !(self.id && self.number) && (self.id || self.number) && quiet == 0
    }
}

/// The escape completing `payload`, and the reply kitty owes it if any —
/// named in the next timeout rather than sent (#680).
fn kitty_completed(
    completed: Option<(GraphicsPayload, Placement)>,
    unanswered: Option<String>,
) -> SeqEvent {
    match (completed, unanswered) {
        (Some((payload, placement)), Some(shape)) => {
            SeqEvent::GraphicsQuery(Box::new(payload), placement, Query::Unanswerable(shape))
        }
        (Some((payload, placement)), None) => SeqEvent::Graphics(Box::new(payload), placement),
        (None, Some(shape)) => SeqEvent::Query(Query::Unanswerable(shape)),
        (None, None) => SeqEvent::None,
    }
}

/// Whether a kitty control block sets `key` to exactly `value`.
fn key_is(control: &[u8], key: &[u8], value: &[u8]) -> bool {
    control.split(|&b| b == b',').any(|pair| {
        pair.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(b"="))
            == Some(value)
    })
}

/// Whether `CSI Ps ; Ps t` with `count` parameters, the first two `first`
/// and `second`, is one of xterm's window reports: a question, whose
/// reply the responder gives (`14`, `16` and `18`, alone) or names in a
/// timeout. Every report ctlseqs documents: the window state (11), its
/// position (13) and pixel size (14), each also with `; 2`, the screen's
/// size in pixels (15), the cell size (16), the text area's and the
/// screen's size in characters (18, 19) and the icon label and title (20,
/// 21). `15` and the `; 2` forms were missing, so a program blocked on one
/// was never named (#692).
///
/// `unhandled.rs` reads this too, so what is named here is never also
/// listed in `Screen::unsupported`.
pub(super) fn is_window_report(count: usize, first: u32, second: u32) -> bool {
    match count {
        1 => matches!(first, 11 | 13 | 14 | 15 | 16 | 18 | 19 | 20 | 21),
        2 => matches!(first, 13 | 14) && second == 2,
        _ => false,
    }
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

/// The bit a DEC private mode number occupies in the tracked set of mouse
/// tracking modes, matching [`MouseModes`]'s layout; zero for any other
/// parameter, so accumulating over a whole list is one `|=` per value.
fn mouse_bit(mode: u32) -> u8 {
    match mode {
        9 => 1,
        1000 => 2,
        1002 => 4,
        1003 => 8,
        _ => 0,
    }
}

/// The tracking mode a DEC private mode number turns on, if it is one of
/// the four.
fn mouse_mode_of(mode: u32) -> Option<MouseMode> {
    match mode {
        9 => Some(MouseMode::Press),
        1000 => Some(MouseMode::PressRelease),
        1002 => Some(MouseMode::ButtonMotion),
        1003 => Some(MouseMode::AnyMotion),
        _ => None,
    }
}

/// The coordinate encoding a DEC private mode selects, if it selects one:
/// UTF-8 (1005), SGR (1006) or urxvt (1015).
fn mouse_encoding_of(mode: u32) -> Option<MouseEncoding> {
    match mode {
        1005 => Some(MouseEncoding::Utf8),
        1006 => Some(MouseEncoding::Sgr),
        1015 => Some(MouseEncoding::Urxvt),
        _ => None,
    }
}

/// An encoding's bit in a parameter list's set of them; zero for the
/// legacy form, which no mode selects.
fn encoding_bit(encoding: MouseEncoding) -> u8 {
    match encoding {
        MouseEncoding::Legacy => 0,
        MouseEncoding::Utf8 => 1,
        MouseEncoding::Sgr => 2,
        MouseEncoding::Urxvt => 4,
    }
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
    /// The current CSI wrote a sub-parameter (`:`). Only ED reads one —
    /// see [`scan_csi_byte`](Self::scan_csi_byte) — so for every other
    /// final byte this makes the sequence unrecognized, as it always was.
    csi_colon: bool,
    /// Digits now belong to a sub-parameter of the current parameter, not
    /// to the parameter itself, until the next `;`.
    csi_in_subparam: bool,
    csi_first: bool,
    csi_param: u32,
    csi_has_digits: bool,
    csi_first_param: u32,
    /// The second parameter, for `DECSTBM`'s bottom margin.
    csi_second_param: u32,
    csi_param_count: u8,
    /// The modes of the current CSI that move a [`ScreenFrame`] — `6`,
    /// `47`, `1048` and `1049` — in the order named. Unlike the flags
    /// above, order matters here: `CSI ? 6 ; 1049 h` sets origin mode on
    /// the main screen and saves it there, `CSI ? 1049 ; 6 h` sets it on
    /// the alternate screen, and vt100 applies a list in order. Four is
    /// more than any application names in one sequence; a fifth is
    /// dropped.
    csi_frame_modes: [u16; 4],
    csi_frame_mode_count: u8,
    csi_saw_2026: bool,
    /// Set when the current CSI's parameter list contains 1004 (focus
    /// reporting), the same trick `csi_saw_2026` uses — a mode can arrive
    /// anywhere in a multi-mode list, so scanning for it beats assuming it
    /// is the only parameter.
    csi_saw_1004: bool,
    /// Whether a completed parameter of the current CSI was `4`: insert
    /// mode, when the final byte turns out to be a plain `h` or `l`.
    csi_saw_irm: bool,
    /// Whether the current CSI's parameter list names an alternate-screen
    /// mode (`47`, `1047`, `1049`), for [`SeqEvent::AlternateScreen`].
    csi_saw_alternate: bool,
    /// The mouse tracking modes (`9`, `1000`, `1002`, `1003`) named in the
    /// current CSI's parameter list, as [`mouse_bit`]s — the same scan as
    /// `csi_saw_1004`, for a group rather than one mode — and the last of
    /// them named, which is the one a set leaves in force.
    csi_saw_mouse: u8,
    csi_last_mouse: MouseMode,
    /// The mouse encodings (1005, 1006, 1015) named in the current CSI's
    /// parameter list, as bits, and the last of them: a set selects the
    /// last one named, since each replaces the one before, and a reset
    /// clears the one in force if it is named anywhere in the list.
    csi_saw_encodings: u8,
    csi_last_encoding: Option<MouseEncoding>,
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
    /// kitty's loader still names an image kitty has removed, so the next
    /// chunk it is handed is refused rather than starting anything (#672).
    ///
    /// A query with `m=1` leaves it so: kitty loads the query's data, then
    /// trims every image whose data is not loaded yet — `remove_images(self,
    /// add_trim_predicate, 0)` after every `a=q` in `grman_handle_command` —
    /// which removes the very image being loaded, while the loader still
    /// points at it. Never set while `building` is in progress.
    orphaned_load: bool,
    /// The reply the command that started `building` asks kitty for, owed
    /// once its last chunk arrives (#680). Read only while `building` is in
    /// progress, and set by every escape that starts it.
    upload_ask: Option<KittyAsk>,
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
    /// The mouse tracking modes the application has enabled and not yet
    /// disabled, as [`mouse_bit`]s. vt100 collapses the four into the one
    /// protocol it would report in — correct for the input path, since a
    /// terminal reports in one protocol — which cannot say which members
    /// of the group were asked for; crossterm asks for three at once. The
    /// set is kept here so `DECRQM` can answer each mode on its own
    /// evidence and a snapshot can report what was requested (#151).
    mouse_tracking: u8,
    /// The one protocol mouse events are reported in, by xterm's rule: the
    /// tracking mode set last, and none once any of the four is reset,
    /// whichever was in force — `really_set_mousemode` in xterm's
    /// `charproc.c` sets `MOUSE_OFF` on a reset of 9, 1000, 1002 or 1003
    /// alike. Tracked here and not read from vt100, which clears its mode
    /// only when the reset names the one it holds: after `?1002h ?1000l`
    /// it went on reporting, and `click` typed a report at an application
    /// a terminal would have sent nothing (#693).
    mouse_protocol: MouseMode,
    /// How mouse coordinates are encoded: the last of 1005, 1006 and 1015
    /// set and not since reset, as in xterm, where each replaces the one
    /// before and a reset clears only the one in force. Tracked here and
    /// not read from vt100, which follows the same rule for 1005 and 1006
    /// but does not implement 1015 — so after `?1006h ?1015h ?1015l` it
    /// would still say SGR where xterm has gone back to the legacy form
    /// (#640).
    mouse_encoding: MouseEncoding,
    /// The raw `DECSCUSR` parameter the application last asked for, or
    /// `None` while it has never asked. Kept as the parameter rather than
    /// as a decoded shape so the one place that knows what `5` means is
    /// the accessor on `Screen`, and "never asked" stays distinguishable
    /// from every value it could have asked for.
    cursor_style: Option<u8>,
    /// The intermediate byte of the current `ESC <intermediate> <final>`
    /// sequence — `(` `)` `*` `+` for a G0–G3 designation — so the final
    /// byte knows which set it designates. Zero once a second intermediate
    /// makes it something else.
    esc_intermediate: u8,
    /// What `ESC ( ) * + Ps` last designated into G0–G3.
    g0: Charset,
    g1: Charset,
    g2: Charset,
    g3: Charset,
    /// True after `SO` (`0x0e`) invoked G1; `SI` (`0x0f`) returns to G0.
    /// Single shifts override this for one character without changing it.
    shifted_out: bool,
    /// Pending SS2/SS3, if any. Cleared by the next character, or by RIS;
    /// an intervening control or designation must not consume it, or a
    /// mixed line that shifts then redraws would lose the graphic.
    single_shift: Option<SingleShift>,
    /// The main screen's frame and the alternate screen's, in that order.
    /// vt100 saves and restores the cursor and attributes itself; what it
    /// does not know about, or does not expose, lives here.
    screens: [ScreenFrame; 2],
    /// Whether the alternate screen is up, by vt100's rule: `47` and
    /// `1049` switch, and `1047`, which vt100 does not implement, does not.
    /// Following xterm there would put the charset slot on one screen and
    /// the cursor vt100 saves on the other.
    alternate: bool,
    /// The grid's height, which decides whether a `DECSTBM` is a region or
    /// a reset, as it does in vt100.
    rows: u16,
    /// The grid's width, kept so a resize to the same size is told apart:
    /// vt100 clears the margins only when the size changes.
    cols: u16,
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
    /// The opening bytes of a DCS-class payload: enough to tell a kitty
    /// escape by its `G`, and for an XTGETTCAP name list, whose hex names
    /// run about six bytes each. A longer list truncates, and the names
    /// that survive are still answered — a partial answer beats none, and
    /// the ones we drop get no reply, which is what an application already
    /// has to handle.
    dcs_head: [u8; 128],
    dcs_head_len: u8,
    /// A kitty escape's control block: the `G` and the `key=value` pairs
    /// after it, up to the first `;`. Kept apart from the 128-byte head,
    /// which cut a long block short: a value that straddled byte 127 was
    /// read wrong and every key past it was lost, `m=1` among them (#700).
    /// Bounded by [`KITTY_CONTROL_MAX`], and by nothing the capture bound
    /// says: the keys describe the image, whatever is kept of its data.
    kitty_control: Vec<u8>,
    /// True while the bytes arriving belong to that control block: from the
    /// introducer of an APC to its first `;`, or to a first byte other than
    /// `G`, which makes it no kitty escape at all.
    kitty_control_open: bool,
    /// Every byte of the current DCS-class string, up to the capture bound —
    /// the image data itself, which the 128-byte head deliberately is not.
    /// Kept only while the bound allows; `dcs_body_full` says whether it is
    /// the whole payload or the start of one.
    dcs_body: Vec<u8>,
    dcs_body_full: bool,
    /// A sixel's raster attributes and painted height, measured over every
    /// byte of its stream rather than over the bounded `dcs_body`: they
    /// describe the image, and stay exact past the capture bound (#635).
    sixel_meter: SixelMeter,
    /// Where the tab stops are. Sized to the grid, so `set_size` follows
    /// every resize.
    tabs: TabStops,
    /// `IRM` (`CSI 4 h`) is set — see [`insert_mode`](Self::insert_mode).
    insert_mode: bool,
}

impl SeqTracker {
    /// `cols` sizes the tab-stop set, and `rows` decides which margins are
    /// a region.
    pub(crate) fn new(capture: usize, rows: u16, cols: u16) -> Self {
        Self {
            state: State::Ground,
            utf8_remaining: 0,
            sync_update: false,
            csi_prefix: 0,
            csi_intermediate: 0,
            csi_invalid: false,
            csi_colon: false,
            csi_in_subparam: false,
            csi_first: true,
            csi_param: 0,
            csi_has_digits: false,
            csi_first_param: 0,
            csi_second_param: 0,
            csi_param_count: 0,
            csi_frame_modes: [0; 4],
            csi_frame_mode_count: 0,
            csi_saw_2026: false,
            csi_saw_1004: false,
            csi_saw_irm: false,
            csi_saw_alternate: false,
            csi_saw_mouse: 0,
            csi_last_mouse: MouseMode::None,
            csi_saw_encodings: 0,
            csi_last_encoding: None,
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
            orphaned_load: false,
            upload_ask: None,
            capture,
            frame_printable: 0,
            focus_events: false,
            mouse_tracking: 0,
            mouse_protocol: MouseMode::None,
            mouse_encoding: MouseEncoding::Legacy,
            cursor_style: None,
            esc_intermediate: 0,
            g0: Charset::Ascii,
            g1: Charset::Ascii,
            g2: Charset::Ascii,
            g3: Charset::Ascii,
            shifted_out: false,
            single_shift: None,
            screens: [ScreenFrame::default(); 2],
            alternate: false,
            rows: rows.max(1),
            cols: cols.max(1),
            dcs_introducer: 0,
            dcs_final: 0,
            dcs_intermediate: 0,
            dcs_data_len: 0,
            dcs_head: [0; 128],
            dcs_head_len: 0,
            kitty_control: Vec::new(),
            kitty_control_open: false,
            dcs_body: Vec::new(),
            dcs_body_full: true,
            sixel_meter: SixelMeter::default(),
            tabs: TabStops::new(cols),
            insert_mode: false,
        }
    }

    /// Follow a resize of the grid. The tab-stop set grows or shrinks with
    /// it — see [`TabStops::set_cols`] for what happens to the columns on
    /// either side of the change — and a change of size clears both
    /// screens' margins, as vt100's does (xterm's too). Origin mode stays.
    pub(crate) fn set_size(&mut self, rows: u16, cols: u16) {
        // vt100 makes a zero dimension one, so both are compared that way.
        let size = (rows.max(1), cols.max(1));
        if size != (self.rows, self.cols) {
            for screen in &mut self.screens {
                screen.top = 0;
            }
        }
        (self.rows, self.cols) = size;
        self.tabs.set_cols(cols);
    }

    /// The frame of the screen on display.
    fn frame(&self) -> &ScreenFrame {
        &self.screens[usize::from(self.alternate)]
    }

    fn frame_mut(&mut self) -> &mut ScreenFrame {
        &mut self.screens[usize::from(self.alternate)]
    }

    /// The row a cursor position report counts from: the top margin under
    /// origin mode, where `CUP` counts from it too, and row 0 otherwise.
    /// xterm subtracts `top_marg` when `ORIGIN` is set (#689).
    pub(crate) fn report_origin_row(&self) -> u16 {
        let frame = self.frame();
        if frame.origin {
            frame.top
        } else {
            0
        }
    }

    /// Whether origin mode (`DECOM`) is set on the screen on display.
    pub(crate) fn origin_mode(&self) -> bool {
        self.frame().origin
    }

    /// Save the character sets into the slot of the screen on display.
    fn save_charsets(&mut self) {
        let saved = SavedCharsets {
            g0: self.g0,
            g1: self.g1,
            g2: self.g2,
            g3: self.g3,
            shifted_out: self.shifted_out,
        };
        self.frame_mut().saved_charsets = Some(saved);
    }

    /// Restore the character sets from the slot of the screen on display;
    /// with nothing saved there, xterm restores the defaults rather than
    /// leaving the current designation, and so do we.
    fn restore_charsets(&mut self) {
        match self.frame().saved_charsets {
            Some(saved) => {
                self.g0 = saved.g0;
                self.g1 = saved.g1;
                self.g2 = saved.g2;
                self.g3 = saved.g3;
                self.shifted_out = saved.shifted_out;
            }
            None => self.reset_charsets(),
        }
    }

    /// `DECSC`: the charsets, and the origin mode vt100 saves with the
    /// cursor.
    fn save_cursor(&mut self) {
        self.save_charsets();
        let frame = self.frame_mut();
        frame.saved_origin = frame.origin;
    }

    /// `DECRC`, the other half of [`save_cursor`](Self::save_cursor).
    fn restore_cursor(&mut self) {
        self.restore_charsets();
        let frame = self.frame_mut();
        frame.origin = frame.saved_origin;
    }

    /// `DECSTBM` (`CSI Pt ; Pb r`), read as vt100 reads it: an omitted or
    /// zero margin is the screen's edge, a bottom past the screen is its
    /// last row, and a region that does not have its top above its bottom
    /// is no region at all.
    fn set_margins(&mut self) {
        let given = |param: u32, default: u16| match u16::try_from(param).unwrap_or(u16::MAX) {
            0 => default,
            value => value,
        };
        let top = if self.csi_param_count == 0 {
            1
        } else {
            given(self.csi_first_param, 1)
        };
        let bottom = if self.csi_param_count < 2 {
            self.rows
        } else {
            given(self.csi_second_param, self.rows)
        };
        let (top, bottom) = (top - 1, (bottom - 1).min(self.rows - 1));
        self.frame_mut().top = if top < bottom { top } else { 0 };
    }

    /// Set or reset one of the modes that move a [`ScreenFrame`], in vt100's
    /// way.
    fn set_frame_mode(&mut self, mode: u16, set: bool) {
        match (mode, set) {
            (6, _) => self.frame_mut().origin = set,
            (47, _) => self.alternate = set,
            // xterm's other cursor save. vt100 does not implement it, so
            // only the charset half is honoured.
            (1048, true) => self.save_charsets(),
            (1048, false) => self.restore_charsets(),
            // vt100 saves the cursor, clears the alternate grid — margins,
            // origin mode and saved cursor with it — and switches; and on
            // the way back switches, then restores. The alternate screen's
            // charset slot is xterm's and survives, as it does there.
            (1049, true) => {
                self.save_cursor();
                let alternate = &mut self.screens[1];
                *alternate = ScreenFrame {
                    saved_charsets: alternate.saved_charsets,
                    ..ScreenFrame::default()
                };
                self.alternate = true;
            }
            (1049, false) => {
                self.alternate = false;
                self.restore_cursor();
            }
            _ => {}
        }
    }

    /// Carry out a [`TabOp`] at cursor column `col`, returning the column the
    /// cursor must move to when the operation is a motion and `None` when it
    /// only edits the set.
    pub(crate) fn tab_op(&mut self, op: TabOp, col: u16) -> Option<u16> {
        match op {
            TabOp::Set => {
                self.tabs.set(col);
                None
            }
            TabOp::ClearAtCursor => {
                self.tabs.clear(col);
                None
            }
            TabOp::ClearAll => {
                self.tabs.clear_all();
                None
            }
            TabOp::Forward(count) => Some(self.tabs.forward(col, count)),
            TabOp::Back(count) => Some(self.tabs.back(col, count)),
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
            .map(String::from_utf8_lossy);
        // Borrowed where the bytes are valid UTF-8, which is the normal case:
        // `Link::open` copies into an `Arc<str>` either way, so owning here
        // first would allocate twice for one span.
        let uri = String::from_utf8_lossy(uri);
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

    /// Abort a kitty upload still waiting for its last chunk, keeping the
    /// wire cost it already ran up.
    ///
    /// Only a kitty escape continues an upload. A delete aborts one, in the
    /// protocol's own words, and neither a sixel nor a hard reset can be
    /// part of one. All three used to leave the upload in place (#587): the
    /// sixel was built on top of it and came out carrying the kitty `s=`,
    /// `c=` and `i=`, the delete was swallowed as its last chunk, and after
    /// `RIS` the next transmission completed it.
    ///
    /// Whatever kitty's loader was left naming goes with it: a delete frees
    /// the load (`handle_delete_command` calls `free_load_data`), and
    /// termlens takes a sixel and a reset to start the loader over.
    fn abandon_upload(&mut self) {
        self.counts.bytes += self.building.abandon();
        self.orphaned_load = false;
        // Nothing of it completes here, so no reply to it is named (#680).
        self.upload_ask = None;
    }

    /// True while the application has focus reporting (mode 1004) enabled.
    pub(crate) fn focus_events(&self) -> bool {
        self.focus_events
    }

    /// The mouse tracking modes the application has enabled and not yet
    /// disabled — the requested set, not the one protocol reports are made
    /// in, which is [`mouse_protocol`](Self::mouse_protocol).
    pub(crate) fn mouse_tracking(&self) -> MouseModes {
        MouseModes::from_bits(self.mouse_tracking)
    }

    /// The protocol mouse events are reported in — see the `mouse_protocol`
    /// field for the rule.
    pub(crate) fn mouse_protocol(&self) -> MouseMode {
        self.mouse_protocol
    }

    /// The mouse coordinate encoding in force — see `mouse_encoding`.
    pub(crate) fn mouse_encoding(&self) -> MouseEncoding {
        self.mouse_encoding
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

    /// Whether `b` is the body of an OSC string past [`OSC_CAPTURE_MAX`],
    /// which this tracker drops and the parsers must not be given either.
    ///
    /// vte keeps an OSC string whole until its terminator, in each of the
    /// emulator's two parsers, so a child that never terminated one grew
    /// the test process by twice the body (#694). Withholding the bytes this
    /// tracker drops leaves the parsers the same capped string the title is
    /// read from, while the terminators — BEL, the ESC of an ST, CAN and
    /// SUB — still reach them, so the string ends in all three at the same
    /// byte. A C0 control in the body is withheld too, which changes nothing:
    /// vte ignores it there.
    ///
    /// Consulted **before** the byte is stepped, as
    /// [`charset_glyph`](Self::charset_glyph) is: the answer depends on the
    /// state the byte arrives in.
    pub(crate) fn withholds(&self, b: u8) -> bool {
        self.state == State::Osc
            && self.osc_buf.len() >= OSC_CAPTURE_MAX
            && !matches!(b, 0x07 | 0x18 | 0x1a | 0x1b)
    }

    /// The glyph `b` draws in the invoked character set, when that set
    /// redefines `b` — the 32 bytes DEC Special Graphics redraws, or `#` in
    /// the UK set — or `None` when the byte draws as itself, or is not a
    /// character at all.
    ///
    /// Consulted **before** the byte is stepped: whether a byte is a
    /// character depends on the state the tracker is in before it, and the
    /// designation's own final byte must not be translated. A pending
    /// single shift is read here and consumed in `transition` when the
    /// character is processed, so a second look (the open-link label) still
    /// sees the same set.
    pub(crate) fn charset_glyph(&self, b: u8) -> Option<&'static str> {
        if self.state != State::Ground {
            return None;
        }
        match self.invoked_charset() {
            Charset::Ascii => None,
            Charset::Uk => (b == b'#').then_some("\u{a3}"),
            Charset::DecSpecialGraphics => dec_special_graphics(b),
        }
    }

    fn invoked_charset(&self) -> Charset {
        match self.single_shift {
            Some(SingleShift::G2) => self.g2,
            Some(SingleShift::G3) => self.g3,
            None => {
                if self.shifted_out {
                    self.g1
                } else {
                    self.g0
                }
            }
        }
    }

    /// Every set back to ASCII with G0 invoked and no single shift pending:
    /// the power-on charset state, which RIS returns to and which `DECRC`
    /// with nothing saved restores.
    fn reset_charsets(&mut self) {
        self.g0 = Charset::Ascii;
        self.g1 = Charset::Ascii;
        self.g2 = Charset::Ascii;
        self.g3 = Charset::Ascii;
        self.shifted_out = false;
        self.single_shift = None;
    }

    /// The tracker's half of `DECSTR`, which is xterm's soft reset
    /// (`ReallyReset` with `full` false): the character sets back to
    /// power-on, the cursor shape back to the terminal's default, insert
    /// mode off, and on the screen on display the margins and origin mode
    /// cleared and the cursor save emptied — xterm saves the reset state
    /// there and homes it, which a restore cannot tell from an empty slot.
    ///
    /// Kept, as xterm keeps them (it resets each only when `full`, for
    /// `RIS`): mouse tracking and its encoding, focus reporting and the tab
    /// stops (#687). The window title, the clipboard, the bell count and the
    /// link log stay too, for the same reason they survive `RIS`: they are
    /// records of what the application emitted, not modes the terminal
    /// holds. An open link span stays open — a soft reset clears no screen,
    /// so the text after it is still the span's text. A kitty upload still
    /// in progress survives as well, though `RIS` abandons one: kitty's soft
    /// reset leaves its images alone, and the next chunk there still
    /// completes the upload.
    fn soft_reset(&mut self) {
        self.reset_charsets();
        self.cursor_style = None;
        self.insert_mode = false;
        let frame = self.frame_mut();
        *frame = ScreenFrame::default();
    }

    /// Whether `IRM` (`CSI 4 h`) is set: a printable byte pushes the rest
    /// of the row right instead of overwriting the cell under the cursor.
    /// vt100 does not model the mode, so the emulator reserves the room
    /// with an `ICH` it does dispatch (#261).
    pub(crate) fn insert_mode(&self) -> bool {
        self.insert_mode
    }

    /// Apply the final byte of an `ESC ( ) * + Ps` designation.
    ///
    /// `0` is DEC Special Graphics and `A` the United Kingdom set, whose one
    /// difference from ASCII is `£` at `#`. Everything else — `B` (ASCII),
    /// the alternate-ROM sets `1` and `2`, the other national sets — reads
    /// as ASCII: a designation acknowledged and left untranslated, which is
    /// close enough for every one of them that guessing at the odd position
    /// would be a worse answer than the plain one.
    fn designate(&mut self, final_byte: u8) {
        let set = match final_byte {
            b'0' => Charset::DecSpecialGraphics,
            b'A' => Charset::Uk,
            _ => Charset::Ascii,
        };
        match self.esc_intermediate {
            b'(' => self.g0 = set,
            b')' => self.g1 = set,
            b'*' => self.g2 = set,
            b'+' => self.g3 = set,
            _ => {}
        }
    }

    fn reset_dcs_scanner(&mut self, introducer: u8) {
        self.dcs_introducer = introducer;
        self.dcs_final = 0;
        self.dcs_intermediate = 0;
        self.dcs_data_len = 0;
        self.dcs_head_len = 0;
        self.kitty_control.clear();
        self.kitty_control_open = introducer == b'_';
        self.dcs_body.clear();
        self.dcs_body_full = true;
        self.sixel_meter = SixelMeter::default();
    }

    fn reset_csi_scanner(&mut self) {
        self.csi_prefix = 0;
        self.csi_intermediate = 0;
        self.csi_invalid = false;
        self.csi_colon = false;
        self.csi_in_subparam = false;
        self.csi_first = true;
        self.csi_param = 0;
        self.csi_has_digits = false;
        self.csi_first_param = 0;
        self.csi_second_param = 0;
        self.csi_param_count = 0;
        self.csi_frame_mode_count = 0;
        self.csi_saw_2026 = false;
        self.csi_saw_1004 = false;
        self.csi_saw_irm = false;
        self.csi_saw_alternate = false;
        self.csi_saw_mouse = 0;
        self.csi_last_mouse = MouseMode::None;
        self.csi_saw_encodings = 0;
        self.csi_last_encoding = None;
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
        if self.csi_param == 4 {
            self.csi_saw_irm = true;
        }
        if matches!(self.csi_param, 47 | 1047 | 1049) {
            self.csi_saw_alternate = true;
        }
        self.csi_saw_mouse |= mouse_bit(self.csi_param);
        if let Some(mode) = mouse_mode_of(self.csi_param) {
            self.csi_last_mouse = mode;
        }
        if let Some(encoding) = mouse_encoding_of(self.csi_param) {
            self.csi_saw_encodings |= encoding_bit(encoding);
            self.csi_last_encoding = Some(encoding);
        }
        if let Ok(mode @ (6 | 47 | 1048 | 1049)) = u16::try_from(self.csi_param) {
            let at = usize::from(self.csi_frame_mode_count);
            if at < self.csi_frame_modes.len() {
                self.csi_frame_modes[at] = mode;
                self.csi_frame_mode_count += 1;
            }
        }
        match self.csi_param_count {
            0 => self.csi_first_param = self.csi_param,
            1 => self.csi_second_param = self.csi_param,
            _ => {}
        }
        self.csi_param_count = self.csi_param_count.saturating_add(1);
        self.csi_param = 0;
        self.csi_has_digits = false;
        self.csi_in_subparam = false;
    }

    /// Track one CSI parameter/intermediate byte.
    fn scan_csi_byte(&mut self, b: u8) {
        match b {
            b'?' | b'>' | b'=' if self.csi_first => self.csi_prefix = b,
            // A sub-parameter's digits are not the parameter's.
            b'0'..=b'9' if self.csi_in_subparam => {}
            b'0'..=b'9' => {
                self.csi_param = self
                    .csi_param
                    .saturating_mul(10)
                    .saturating_add(u32::from(b - b'0'));
                self.csi_has_digits = true;
            }
            b';' => self.end_csi_param(),
            // A sub-parameter (`3:1`). vt100 reads the first value of the
            // first parameter for ED and ignores the rest, so `CSI 3:1 J`
            // erases the saved lines there — and a tracker that refused the
            // shape let the erase through unseen, which cost the rows
            // termlens had already kept (#621). Kept to the parameter it
            // belongs to; `csi_final` decides which sequences may carry one.
            b':' => {
                self.csi_colon = true;
                self.csi_in_subparam = true;
            }
            // `$` is the intermediate of the DECRQM request (`CSI ? n $ p`),
            // `SP` of DECSCUSR (`CSI Ps SP q`) and `!` of DECSTR
            // (`CSI ! p`); recording them keeps those sequences
            // classifiable instead of discarding them as unrecognized.
            b'$' | b' ' | b'!' => self.csi_intermediate = b,
            // Sub-parameters or other intermediates: none of the sequences
            // we recognize use them.
            _ => self.csi_invalid = true,
        }
        self.csi_first = false;
    }

    /// The event (if any) implied by a CSI final byte.
    fn csi_final(&mut self, b: u8) -> SeqEvent {
        // ED and DECSTBM read the first value of a parameter with
        // sub-parameters in vt100, so they act on one; nothing else does.
        if self.csi_invalid || (self.csi_colon && !matches!(b, b'J' | b'r')) {
            return SeqEvent::None;
        }
        if self.csi_has_digits {
            self.end_csi_param();
        }
        let params_empty = self.csi_param_count == 0;
        let (count, first) = (self.csi_param_count, self.csi_first_param);
        let single = move |v: u32| count == 1 && first == v;

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

        // DECSTR (`CSI ! p`), the soft reset: what a well-behaved TUI sends
        // on startup and teardown for a known-good terminal without the
        // screen clear RIS brings. xterm's is the reference (#687): what it
        // resets of the state this crate holds is reset here, and the
        // emulator is told to do the same for the state it holds (#233).
        if self.csi_intermediate == b'!' {
            if b == b'p' && self.csi_prefix == 0 && params_empty {
                self.soft_reset();
                return SeqEvent::SoftReset;
            }
            return SeqEvent::None;
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

        // The screen frames — see `ScreenFrame`: the margins, origin mode,
        // the screen switches vt100 carries out and the cursor saves. Ahead
        // of 2026, whose Begin returns from a list that names one of these
        // too.
        match (self.csi_prefix, b) {
            (0, b'r') => self.set_margins(),
            // SCOSC / SCORC. With a parameter `CSI s` is DECSLRM in xterm
            // and `CSI u` belongs to kitty's keyboard protocol.
            (0, b's') if params_empty => self.save_charsets(),
            (0, b'u') if params_empty => self.restore_charsets(),
            (b'?', b'h' | b'l') => {
                for i in 0..usize::from(self.csi_frame_mode_count) {
                    self.set_frame_mode(self.csi_frame_modes[i], b == b'h');
                }
            }
            _ => {}
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

        // The mouse tracking modes, kept as the set the application asked
        // for — see `mouse_tracking`. Every mode named in one list is set
        // or cleared together, which is how they arrive. And the one
        // protocol reports are made in — see `mouse_protocol`: a list is
        // applied in order, so a set leaves the last mode it names in
        // force, and a reset naming any of them turns reporting off.
        if self.csi_prefix == b'?' && self.csi_saw_mouse != 0 {
            match b {
                b'h' => {
                    self.mouse_tracking |= self.csi_saw_mouse;
                    self.mouse_protocol = self.csi_last_mouse;
                }
                b'l' => {
                    self.mouse_tracking &= !self.csi_saw_mouse;
                    self.mouse_protocol = MouseMode::None;
                }
                _ => {}
            }
        }

        // The mouse encodings, by xterm's rule — see `mouse_encoding`.
        if self.csi_prefix == b'?' {
            match b {
                b'h' => {
                    if let Some(last) = self.csi_last_encoding {
                        self.mouse_encoding = last;
                    }
                }
                b'l' if self.csi_saw_encodings & encoding_bit(self.mouse_encoding) != 0 => {
                    self.mouse_encoding = MouseEncoding::Legacy;
                }
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

        // The alternate screen, announced rather than tracked: vt100 holds
        // the state, and the emulator only needs to know where the switch
        // falls in the stream. After 2026, whose Begin returns first from a
        // list naming both: a frame boundary missed is worse than rows
        // copied only once the main screen is back, as they were before.
        // An End that closes a frame stops the feed at this byte anyway,
        // which is all a way out needs.
        if self.csi_prefix == b'?' && self.csi_saw_alternate && matches!(b, b'h' | b'l') {
            return SeqEvent::AlternateScreen { leaving: b == b'l' };
        }

        // Insert mode (`IRM`, ANSI mode 4): `smir`/`rmir` in the terminfo
        // entry every child is handed, and the mode ncurses uses for
        // `insch`. vt100 dispatches no ANSI mode at all, so the flag lives
        // here and the emulator acts on it (#261). Any mode list naming 4
        // counts, as it does for the private modes above.
        if self.csi_prefix == 0 && self.csi_saw_irm {
            match b {
                b'h' => self.insert_mode = true,
                b'l' => self.insert_mode = false,
                _ => {}
            }
        }

        // Tab stops: `TBC` (`CSI g`), `CHT` (`CSI n I`) and `CBT`
        // (`CSI n Z`). All three are relative to the cursor column, so they
        // leave as requests rather than as changes.
        if self.csi_prefix == 0 {
            // An omitted parameter is 0, and a 0 count means 1 — vt100's own
            // `canonicalize_params_1`, so a rewritten move and the move it
            // replaces read their parameter the same way.
            let ps = if params_empty {
                0
            } else {
                self.csi_first_param
            };
            let count = u16::try_from(ps).unwrap_or(u16::MAX).max(1);
            match b {
                // Only `0` (this column) and `3` (all) are modelled. The
                // rest of the family clears *line* tab stops, which this
                // crate has no notion of; ignoring them beats inventing one.
                b'g' if self.csi_param_count <= 1 => {
                    return match ps {
                        0 => SeqEvent::Tabs(TabOp::ClearAtCursor),
                        3 => SeqEvent::Tabs(TabOp::ClearAll),
                        _ => SeqEvent::None,
                    };
                }
                b'I' if self.csi_param_count <= 1 => {
                    return SeqEvent::Tabs(TabOp::Forward(count));
                }
                b'Z' if self.csi_param_count <= 1 => {
                    return SeqEvent::Tabs(TabOp::Back(count));
                }
                // ED 3, erase the saved lines. vt100 reads the first
                // parameter alone, and so does this.
                b'J' if ps == 3 => return SeqEvent::ScrollbackErased,
                _ => {}
            }
        }
        // DECSED 3 (`CSI ? 3 J`): vt100 treats it as ED 3, as xterm does,
        // so it erases the same history and must be seen the same way
        // (#621).
        if self.csi_prefix == b'?' && b == b'J' && !params_empty && self.csi_first_param == 3 {
            return SeqEvent::ScrollbackErased;
        }
        if self.csi_colon {
            return SeqEvent::None;
        }

        // Queries. Classification only — answering policy lives upstream.
        let query = match (self.csi_prefix, b) {
            (0, b'n') if single(6) => Some(Query::CursorPosition { private: false }),
            (b'?', b'n') if single(6) => Some(Query::CursorPosition { private: true }),
            (0, b'n') if single(5) => Some(Query::OperatingStatus),
            (0, b'c') if params_empty || single(0) => Some(Query::PrimaryDa),
            (b'>', b'c') if params_empty || single(0) => Some(Query::SecondaryDa),
            (b'>', b'q') if params_empty || single(0) => {
                Some(Query::Unanswerable(self.seq_printable()))
            }
            (0, b't') if single(18) => Some(Query::TextAreaSize),
            // Pixel geometry. Arithmetic, not rendering: answering claims
            // nothing the emulator cannot do, and DA1 goes on declining
            // graphics either way.
            (0, b't') if single(14) => Some(Query::WindowSizePixels),
            (0, b't') if single(16) => Some(Query::CellSizePixels),
            // Questions we can recognize but not answer: the DSR family,
            // and DA3. Not `CSI > Ps n`, which is XTMODKEYS's "disable", a
            // setting with no reply (#692).
            (0 | b'?' | b'=', b'n') | (b'=', b'c') => {
                Some(Query::Unanswerable(self.seq_printable()))
            }
            // XTSMGRAPHICS (`CSI ? Pi ; Pa ; Pv S`): sixel programs ask it
            // for the colour registers and the geometry right after DA1
            // claims sixel. Every form gets a reply from xterm, a set
            // included, so every form is a question; none is answered here,
            // since the register count would be a claim about a renderer
            // termlens does not have (#641).
            (b'?', b'S') => Some(Query::Unanswerable(self.seq_printable())),
            (b'?', b'u') if params_empty => {
                // kitty keyboard probe. Its protocol pairs this with DA1;
                // our DA1 answer unblocks the probe like any non-kitty
                // terminal, but the probe itself is still unanswered.
                Some(Query::Unanswerable(self.seq_printable()))
            }
            // The other window reports; `14`, `16` and `18` alone are
            // classified above.
            (0, b't')
                if is_window_report(
                    usize::from(self.csi_param_count),
                    self.csi_first_param,
                    self.csi_second_param,
                ) =>
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
        // An APC opening with `G` is the kitty graphics protocol — a
        // continuation chunk too, which is `G` with `m=` and nothing else.
        // Any other APC is not graphics and leaves an upload where it was,
        // as kitty does; it used to be taken as the upload's next chunk.
        // The first byte captured, not slot 0: a reset zeroes only the
        // length, so an empty APC found the last escape's `G` there (#735).
        if self.dcs_introducer == b'_'
            && self.dcs_head[..usize::from(self.dcs_head_len)].first() == Some(&b'G')
        {
            let control = self.kitty_control.clone();
            // The control block is `G` followed by the comma-separated keys.
            let keys = control.strip_prefix(b"G").unwrap_or(&control);
            let is_query = keys.split(|&b| b == b',').any(|pair| pair == b"a=q");
            if !is_query {
                let at = self.dcs_payload_start();
                if self.orphaned_load {
                    match MidUpload::of(keys) {
                        // kitty's `handle_add_command` takes it as the next
                        // chunk of the load, finds the image that load was
                        // for gone, and refuses it (`EILSEQ`), clearing the
                        // load: the escape is spent, and no image comes of
                        // it (#672). Its bytes crossed the wire all the same.
                        MidUpload::Continues if !key_is(keys, b"a", b"f") => {
                            self.orphaned_load = false;
                            self.counts.bytes += self.dcs_data_len;
                            return SeqEvent::None;
                        }
                        // A delete frees the load, and a transmission from
                        // outside the stream starts an image of its own.
                        MidUpload::Aborts => self.orphaned_load = false,
                        // A placement, an animation, a compose — and a
                        // frame, which kitty's frame loader refuses without
                        // clearing the load — leave it as it is.
                        _ => {}
                    }
                }
                if self.building.in_progress() {
                    match MidUpload::of(keys) {
                        MidUpload::Continues => {}
                        // A delete is never an upload's last chunk: it
                        // aborts the upload in progress and is then a
                        // delete like any other. A transmission from
                        // outside the stream starts an image of its own.
                        MidUpload::Aborts => self.abandon_upload(),
                        // A placement, an animation or a compose: a
                        // payload of its own, and the upload goes on
                        // (#636). It was taken as the upload's next chunk.
                        MidUpload::Beside => {
                            let mut beside = GraphicsBuilder::default();
                            beside.kitty(keys);
                            beside.chunk(
                                self.dcs_data_len,
                                &self.dcs_body[at..],
                                self.dcs_body_full,
                                self.capture,
                            );
                            let completed = beside.finish_placed();
                            if let Some((payload, _)) = &completed {
                                self.counts.record(payload);
                            }
                            let unanswered = KittyAsk::of(keys, self.seq_printable())
                                .filter(|ask| ask.refused() || ask.answered(0))
                                .map(|ask| ask.shape);
                            return kitty_completed(completed, unanswered);
                        }
                    }
                }
                // The reply kitty owes this escape at once, and the one it
                // owes the load this escape starts once the load completes
                // (#680): see `KittyAsk`. A continuation asks nothing of its
                // own; it only finishes what the first chunk asked.
                let mut unanswered = None;
                if !self.building.in_progress() {
                    let ask = KittyAsk::of(keys, self.seq_printable());
                    if let Some(ask) = ask.as_ref().filter(|ask| ask.refused()) {
                        unanswered = Some(ask.shape.clone());
                    }
                    self.upload_ask = ask.filter(|ask| !ask.refused());
                }
                // One image, however many escapes it took. `m=1` says
                // another follows; the transmission is not an image until
                // one arrives without it, and counting each escape instead
                // reported a 4.9 KB chart as two pictures.
                self.building.kitty(keys);
                self.building.chunk(
                    self.dcs_data_len,
                    &self.dcs_body[at..],
                    self.dcs_body_full,
                    self.capture,
                );
                if key_is(keys, b"m", b"1") {
                    return kitty_completed(None, unanswered);
                }
                let quiet = kitty_key(keys, b"q=").unwrap_or(0);
                if let Some(ask) = self.upload_ask.take().filter(|ask| ask.answered(quiet)) {
                    unanswered = Some(ask.shape);
                }
                let completed = self.building.finish_placed();
                if let Some((payload, _)) = &completed {
                    self.counts.record(payload);
                }
                return kitty_completed(completed, unanswered);
            }
            return self.kitty_query(keys);
        }
        // Sixel: `DCS <params> q <data> ST`, with no intermediate byte. The
        // intermediate is the whole distinction from the two DCS questions
        // below, which reach the same `q` final through `+` and `$`.
        if self.dcs_introducer == b'P' && self.dcs_final == b'q' && self.dcs_intermediate == 0 {
            // No part of a kitty upload, so it abandons one still in
            // progress rather than being built on top of it.
            self.abandon_upload();
            self.building.sixel(&self.sixel_meter);
            let at = self.dcs_payload_start();
            self.building.chunk(
                self.dcs_data_len,
                &self.dcs_body[at..],
                self.dcs_body_full,
                self.capture,
            );
            return match self.building.finish_placed() {
                Some((payload, placement)) => {
                    self.counts.record(&payload);
                    SeqEvent::Graphics(Box::new(payload), placement)
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

    /// A kitty query (`a=q`), taken through kitty's loader in the order
    /// kitty's `grman_handle_command` (`graphics.c`) takes it (#672).
    ///
    /// Only an explicit `a=q` is a question. A transmission is an
    /// instruction, and classifying one as a query would put "the
    /// application queried the terminal" into the next timeout of every
    /// application that draws — a false diagnosis, and a loud one. Only one
    /// that names an image and does not ask for quiet is named, since kitty
    /// owes it a reply termlens does not send (#680): see `KittyAsk`.
    ///
    /// A command naming both an image id and an image number is refused
    /// before anything else is looked at, and a query without an id right
    /// after; neither reaches the loader. Nor does a feature query (`e=`):
    /// kitty 0.49 rejects the key outright, and the development branch that
    /// adds it says "A feature query transmits no image data, so it must not
    /// interfere with any chunked load that is in progress". Every other
    /// query hands its
    /// data to `handle_add_command` as a transmission would, so:
    ///
    /// - from a file or shared memory (`t=` other than `d`), it starts a
    ///   load of its own, and the upload in progress is dropped;
    /// - with the data in the escape and an upload in progress, it *is* the
    ///   upload's next chunk: its data is appended, and without `m=1` it is
    ///   the last one, so the upload's image is complete — and placed, for
    ///   `a=T` — before the query is answered;
    /// - with `m=1`, the load then waits for more, and the trim that follows
    ///   every query (`remove_images(self, add_trim_predicate, 0)`) removes
    ///   the image whose data is not loaded yet: the upload's, or the
    ///   query's own. Nothing is answered, and the loader is left naming an
    ///   image that is gone, so the next chunk is refused;
    /// - handed to a loader in that state, it is refused (`EILSEQ`).
    fn kitty_query(&mut self, keys: &[u8]) -> SeqEvent {
        let id = kitty_key(keys, b"i=");
        let number = kitty_key(keys, b"I=");
        let nonzero = |value: Option<u32>| value.is_some_and(|value| value != 0);
        let feature_query = keys
            .split(|&b| b == b',')
            .any(|pair| pair.starts_with(b"e="));
        let mut load = KittyLoad::Loaded;
        let mut completed = None;
        if nonzero(id) && !nonzero(number) && !feature_query {
            let direct = keys
                .split(|&b| b == b',')
                .find_map(|pair| pair.strip_prefix(b"t="))
                .is_none_or(|medium| medium == b"d");
            let more = key_is(keys, b"m", b"1");
            if !direct {
                self.abandon_upload();
            } else if self.orphaned_load {
                self.orphaned_load = false;
                load = KittyLoad::Gone;
            } else {
                if self.building.in_progress() {
                    let at = self.dcs_payload_start();
                    self.building.chunk(
                        self.dcs_data_len,
                        &self.dcs_body[at..],
                        self.dcs_body_full,
                        self.capture,
                    );
                    if !more {
                        completed = self.building.finish_placed();
                    }
                }
                if more {
                    self.abandon_upload();
                    self.orphaned_load = true;
                    load = KittyLoad::Pending;
                }
            }
        }
        let query = Query::KittyGraphics {
            id,
            number,
            placement: kitty_key(keys, b"p="),
            quiet: kitty_key(keys, b"q=").unwrap_or(0),
            load,
            shape: self.seq_printable(),
        };
        match completed {
            Some((payload, placement)) => {
                self.counts.record(&payload);
                SeqEvent::GraphicsQuery(Box::new(payload), placement, query)
            }
            None => SeqEvent::Query(query),
        }
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

    /// Take one byte of an APC into the kitty control block, if it is part
    /// of one.
    ///
    /// A kitty payload is `G <key=value>,… ; <base64>`; only the control
    /// block before the `;` is worth reading, and stopping there keeps a
    /// base64 blob from being scanned for keys it cannot contain.
    fn push_kitty_control(&mut self, b: u8) {
        if !self.kitty_control_open {
            return;
        }
        if b == b';' || (self.kitty_control.is_empty() && b != b'G') {
            self.kitty_control_open = false;
        } else if self.kitty_control.len() < KITTY_CONTROL_MAX {
            self.kitty_control.push(b);
        } else {
            // The bound: nothing past it is read. A pair it cuts in two is
            // dropped whole, so that a value is never read as its own first
            // digits, which is what the 128-byte head did to one.
            if b != b',' {
                let whole = self.kitty_control.iter().rposition(|&b| b == b',');
                self.kitty_control.truncate(whole.unwrap_or(1));
            }
            self.kitty_control_open = false;
        }
    }

    /// Consume one byte of a DCS-class string: header first, then payload.
    fn push_dcs(&mut self, b: u8) {
        // Every byte between the introducer and the terminator, for both
        // protocols alike — a sixel's parameters and a kitty control block
        // are a handful of bytes against an image's thousands, and counting
        // them uniformly is easier to reason about than two rules.
        self.dcs_data_len += 1;
        // A sixel's data: everything after the `q` that closes a header with
        // no intermediate — which is the test the terminator applies too.
        if self.dcs_introducer == b'P' && self.dcs_final == b'q' && self.dcs_intermediate == 0 {
            self.sixel_meter.feed(b);
        }
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
        self.push_kitty_control(b);
        // The body is what a decoder needs and the head is not: 128 bytes
        // holds a name list, never an image. Bounded, and the flag says
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
        } else if !self.passes_through(b) {
            self.push_seq(b);
        }
        self.transition(b)
    }

    /// Whether `b` is outside the sequence it arrived in: a C0 control
    /// the parser executes in passing, or a byte it ignores there. Neither
    /// is part of the sequence's shape, so neither is captured into it.
    fn passes_through(&self, b: u8) -> bool {
        matches!(self.state, State::Esc | State::EscIntermediate | State::Csi)
            && matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff)
    }

    fn transition(&mut self, b: u8) -> SeqEvent {
        const ESC: u8 = 0x1b;
        const CAN: u8 = 0x18;
        const SUB: u8 = 0x1a;
        const BEL: u8 = 0x07;
        const HT: u8 = 0x09;
        const SO: u8 = 0x0e;
        const SI: u8 = 0x0f;

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
                    // Locking shifts: SO invokes G1, SI returns to G0. vt100
                    // sees both bytes too and ignores them, so nothing else
                    // has to be told. A pending single shift overrides
                    // whichever is locked, for one character, and is not
                    // consumed here — SI/SO are C0, not GL.
                    match b {
                        SO => self.shifted_out = true,
                        SI => self.shifted_out = false,
                        // Plain `HT` has to come through here as well as
                        // `CHT`, or vt100's fixed eight would keep answering
                        // it and the two would disagree the moment an
                        // application set a stop of its own — a table drawn
                        // half from each.
                        HT => event = SeqEvent::Tabs(TabOp::Forward(1)),
                        _ => {}
                    }
                    // Anything that is not a C0 control or DEL. Named once
                    // because the two readers below want the same test for
                    // different reasons, and they must not drift apart: a
                    // byte that counts as drawn is a byte a link wraps.
                    let printable = b >= 0x20 && b != 0x7f;
                    // One per *character*: continuation bytes arrive while
                    // `utf8_remaining` is non-zero, so nothing is counted
                    // twice. Controls, the BEL just handled included, are not
                    // printable.
                    if printable && self.utf8_remaining == 0 {
                        self.frame_printable = self.frame_printable.saturating_add(1);
                    }
                    // Text written inside an `OSC 8` span is that link's
                    // label. Bytes rather than characters here, so a
                    // multi-byte grapheme survives the split — its
                    // continuation bytes are all >= 0x80 and printable too.
                    // A byte the invoked character set redefines is recorded
                    // as the glyph it draws: the label is what a reader sees
                    // and clicks, and a reader sees `─`, not `q`.
                    if printable && self.link_open {
                        let drawn: &[u8] = match self.charset_glyph(b) {
                            Some(glyph) => glyph.as_bytes(),
                            None => std::slice::from_ref(&b),
                        };
                        for &drawn in drawn {
                            if self.link_label.len() < LINK_LABEL_MAX {
                                self.link_label.push(drawn);
                            } else {
                                self.link_label_truncated = true;
                            }
                        }
                    }
                    // A single shift lasts for one *character*, not one byte:
                    // a multi-byte UTF-8 character consumes it too, so a
                    // continuation byte must not, or the shift survives `汉`
                    // and translates the byte after it. C0, DEL and newline
                    // still do not consume it: SS2 then SI then `l` draws
                    // from G2, which is the mixed-line case this exists for.
                    // RIS clears it separately.
                    let utf8_continuation = (0x80..=0xbf).contains(&b);
                    if b >= 0x20 && b != 0x7f && !utf8_continuation {
                        self.single_shift = None;
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
                0x20..=0x2f => {
                    self.esc_intermediate = b;
                    State::EscIntermediate
                }
                ESC => State::Esc,
                CAN | SUB => State::Ground,
                // A C0 control inside an escape is executed and the escape
                // goes on, and DEL and the bytes from 0x80 are ignored —
                // vte's `advance_esc`, which is what feeds the grid. Dropping
                // to ground here put the two out of step (#627): in
                // `ESC CR [6n` the parser answers a CSI the tracker never
                // saw, and a byte the invoked set redraws was staged as
                // UTF-8 the parser then swallowed inside the escape.
                0x00..=0x1f | 0x7f..=0xff => {
                    self.execute_in_sequence(b);
                    State::Esc
                }
                // RIS (`ESC c`), the hard reset: the terminal returns to its
                // power-on state, so the cursor is the terminal's default
                // again, all four character sets are ASCII with G0 invoked,
                // no single shift is pending, and neither an open hyperlink
                // span nor a kitty upload still waiting for its last chunk
                // can survive — kitty's own reset drops the image being
                // loaded, so the chunks still to come fail there.
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
                    self.reset_charsets();
                    // vt100 builds a new screen: the main one, with no
                    // margins, no origin mode and nothing saved.
                    self.screens = [ScreenFrame::default(); 2];
                    self.alternate = false;
                    // Focus reporting and an open synchronized update are
                    // modes the terminal holds, so a reset ends both. Left
                    // set, `focus_in` typed `^[[I` at a program that never
                    // asked again, and an update begun before the reset
                    // never ended, so `wait_stable` could not succeed
                    // (#623). The half-drawn frame is dropped, not
                    // completed: nothing after the reset belongs to it.
                    self.focus_events = false;
                    self.sync_update = false;
                    self.frame_printable = 0;
                    self.mouse_tracking = 0;
                    self.mouse_protocol = MouseMode::None;
                    self.mouse_encoding = MouseEncoding::Legacy;
                    self.insert_mode = false;
                    self.tabs.reset();
                    self.close_link();
                    self.abandon_upload();
                    // The grid rebuild is vt100's; the event tells the
                    // emulator to capture what scrolled before it, since
                    // that history is what the rebuild discards (#391).
                    event = SeqEvent::HardReset;
                    State::Ground
                }
                // DECSC / DECRC: the charset half of save-cursor and
                // restore-cursor. The idiom is save, jump, draw a border,
                // restore — and a restore that forgot the designation
                // rendered the border after it as `lqk`. vt100 does the
                // cursor and attributes; the sets are ours (#232), kept per
                // screen (#685), and so is the origin mode vt100 saves.
                b'7' => {
                    self.save_cursor();
                    State::Ground
                }
                b'8' => {
                    self.restore_cursor();
                    State::Ground
                }
                // HTS (`ESC H`): a tab stop at the cursor column, which the
                // emulator supplies. `hts` is in the terminfo entry every
                // child is handed, so an application laying out a table is
                // entitled to expect it.
                b'H' => {
                    event = SeqEvent::Tabs(TabOp::Set);
                    State::Ground
                }
                // SS2 / SS3: invoke G2 / G3 for the next character only.
                // These are two-character escapes in the *output* stream;
                // `ESC O A` as a DECCKM cursor key is what we *send*, a
                // different direction, and is not parsed here.
                b'N' => {
                    self.single_shift = Some(SingleShift::G2);
                    State::Ground
                }
                b'O' => {
                    self.single_shift = Some(SingleShift::G3);
                    State::Ground
                }
                // Final byte of a two-character sequence (ESC 7, ESC =, …).
                _ => State::Ground,
            },
            State::EscIntermediate => match b {
                // A second intermediate makes this something other than a
                // G0–G3 designation (`ESC % G` selects UTF-8, for one).
                0x20..=0x2f => {
                    self.esc_intermediate = 0;
                    State::EscIntermediate
                }
                ESC => State::Esc,
                CAN | SUB => State::Ground,
                // The final byte: a designation, if the intermediate was a
                // G-set introducer, and the end of the sequence either way.
                0x30..=0x7e => {
                    self.designate(b);
                    State::Ground
                }
                // C0 executes and the rest is ignored, as in `Esc` (#627).
                _ => {
                    self.execute_in_sequence(b);
                    State::EscIntermediate
                }
            },
            State::Csi => match b {
                0x40..=0x7e => {
                    event = self.csi_final(b);
                    State::Ground
                }
                ESC => State::Esc,
                CAN | SUB => State::Ground,
                // Executed or ignored in passing, as in `Esc`: vte's CSI
                // states do the same, so `CSI 6 CR n` is still a question
                // (#627).
                0x00..=0x1f | 0x7f..=0xff => {
                    self.execute_in_sequence(b);
                    State::Csi
                }
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

    /// A byte that arrived inside an escape and is not part of it: a C0
    /// control is carried out as in ground state, and anything else (DEL,
    /// 0x80 and up) is ignored. CAN, SUB and ESC never reach here; they end
    /// or restart the sequence.
    ///
    /// Only the tracker's own share of the control is done. `HT` is not
    /// turned into a tab-stop request: the column it moves to would be
    /// written as a `CHA` while the parser is still inside the escape,
    /// which would abort that escape there and not here. vt100 moves it by
    /// its own fixed stops instead — a divergence confined to a tab inside
    /// an escape, against a desynchronised parser for everything after it.
    fn execute_in_sequence(&mut self, b: u8) {
        match b {
            0x07 => self.bells = self.bells.saturating_add(1),
            0x0e => self.shifted_out = true,
            0x0f => self.shifted_out = false,
            _ => {}
        }
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
    use crate::graphics::{GraphicsAction, GraphicsFormat, GraphicsProtocol};

    /// The size these tests construct a tracker at: the default terminal's.
    /// Only the tab-stop set and the margins are sized, and only the tests
    /// of those care what the size is.
    const TEST_ROWS: u16 = 24;
    const TEST_COLS: u16 = 80;

    fn fed(bytes: &[u8]) -> SeqTracker {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
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
        let mut tracker = SeqTracker::new(40, TEST_ROWS, TEST_COLS);
        // Ten chunks of eight data bytes: every escape fits the bound
        // comfortably, and the ten together do not.
        let mut wire: Vec<u8> = b"\x1b_Ga=T,f=32,s=4,v=4,m=1;AAAAAAAA\x1b\\".to_vec();
        for _ in 0..8 {
            wire.extend_from_slice(b"\x1b_Gm=1;BBBBBBBB\x1b\\");
        }
        wire.extend_from_slice(b"\x1b_Gm=0;CCCCCCCC\x1b\\");

        let mut seen = None;
        for &byte in &wire {
            if let SeqEvent::Graphics(payload, _) = tracker.step(byte) {
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
        let mut tracker = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        let mut out = Vec::new();
        for &byte in bytes {
            if let SeqEvent::Graphics(payload, _) | SeqEvent::GraphicsQuery(payload, _, _) =
                tracker.step(byte)
            {
                out.push(*payload);
            }
        }
        out
    }

    /// Every payload and every query a feed produced, in order, and the
    /// tracker it left.
    fn kitty_events(bytes: &[u8]) -> (Vec<GraphicsPayload>, Vec<Query>, SeqTracker) {
        let mut tracker = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        let (mut seen, mut queries) = (Vec::new(), Vec::new());
        for &byte in bytes {
            match tracker.step(byte) {
                SeqEvent::Graphics(payload, _) => seen.push(*payload),
                SeqEvent::GraphicsQuery(payload, _, query) => {
                    seen.push(*payload);
                    queries.push(query);
                }
                SeqEvent::Query(query) => queries.push(query),
                _ => {}
            }
        }
        (seen, queries, tracker)
    }

    /// The `(i=, load)` of each kitty query.
    fn loads(queries: &[Query]) -> Vec<(Option<u32>, KittyLoad)> {
        queries
            .iter()
            .map(|query| match query {
                Query::KittyGraphics { id, load, .. } => (*id, *load),
                other => panic!("not a kitty query: {other:?}"),
            })
            .collect()
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

    /// The first chunk of a kitty upload whose last chunk never comes, with
    /// every fact a control block can state, so one leaking into whatever
    /// follows is visible. #587's first sequence.
    const KITTY_UNFINISHED: &[u8] = b"\x1b_Ga=T,f=32,s=1,v=1,i=42,c=3,r=2,o=z,t=f,m=1;AAAA\x1b\\";
    /// A 4x6 sixel, red throughout.
    const RED_SIXEL: &[u8] = b"\x1bPq\"1;1;4;6#0;2;100;0;0~~~~\x1b\\";
    /// The first chunk of #587's second sequence.
    const UPLOAD_OF_5: &[u8] = b"\x1b_Ga=T,f=32,s=2,v=1,i=5,m=1;AAAA\x1b\\";

    /// Wire bytes the tracker counts for whole escapes: everything between
    /// the two-byte introducer and the two-byte `ST`.
    fn wire_cost(escapes: &[&[u8]]) -> u64 {
        escapes.iter().map(|escape| escape.len() as u64 - 4).sum()
    }

    /// A sixel is no part of a kitty upload, so one arriving mid-upload
    /// abandons it (#587). It was built on top of it instead: one payload,
    /// described as a sixel carrying the kitty `s=`, `c=`/`r=`, `t=`, `i=`
    /// and `o=`, its data the kitty chunk and the sixel run together.
    #[test]
    fn a_sixel_abandons_an_unfinished_kitty_upload() {
        let wire = [KITTY_UNFINISHED, RED_SIXEL].concat();
        let seen = payloads(&wire);
        assert_eq!(seen.len(), 1, "{seen:?}");
        let sixel = &seen[0];
        assert_eq!(sixel.protocol(), GraphicsProtocol::Sixel);
        assert_eq!(sixel.size(), Some((4, 6)), "its own raster: {sixel:?}");
        assert_eq!(sixel.cells(), None, "no kitty fact: {sixel:?}");
        assert_eq!(sixel.transmission(), None, "{sixel:?}");
        assert_eq!(sixel.id(), None, "{sixel:?}");
        assert!(!sixel.compressed(), "{sixel:?}");
        assert_eq!(sixel.chunks(), 1);
        assert_eq!(sixel.data(), Some(&b"\"1;1;4;6#0;2;100;0;0~~~~"[..]));

        let counts = fed(&wire).graphics();
        assert_eq!((counts.kitty, counts.sixel), (0, 1), "{counts:?}");
        // Abandoned is not unsent: the upload's chunk crossed the wire, and
        // `bytes` counts traffic rather than pictures.
        assert_eq!(counts.bytes, wire_cost(&[KITTY_UNFINISHED, RED_SIXEL]));
    }

    /// The picture the bug was about: built on the kitty chunk, the `AAAA`
    /// in front pushed the red four columns right of the declared 4x6 and
    /// the frame decoded fully transparent.
    #[cfg(feature = "decode")]
    #[test]
    fn a_sixel_after_an_abandoned_upload_decodes_to_the_red_it_painted() {
        let seen = payloads(&[KITTY_UNFINISHED, RED_SIXEL].concat());
        let bitmap = seen[0].decode().expect("decodes");
        assert_eq!((bitmap.width(), bitmap.height()), (4, 6));
        assert_eq!(bitmap.colours(), vec![([255, 0, 0, 255], 24)]);
    }

    /// "If an image is being loaded in chunks and the upload is not complete
    /// when any delete command is received, the partial upload must be
    /// aborted" — and the delete still deletes (#587). It was read as the
    /// upload's last chunk instead: its control block ignored, the upload
    /// counted as an image that decodes to nothing, and the teardown
    /// `deletes` exists to assert gone.
    #[test]
    fn a_delete_aborts_an_unfinished_upload_and_is_counted() {
        let delete: &[u8] = b"\x1b_Ga=d,d=i,i=5\x1b\\";
        let wire = [UPLOAD_OF_5, delete].concat();
        let counts = fed(&wire).graphics();
        assert_eq!(counts.kitty, 0, "an aborted upload is no image: {counts:?}");
        assert_eq!(counts.deletes, 1, "and the delete is counted: {counts:?}");
        assert_eq!(counts.bytes, wire_cost(&[UPLOAD_OF_5, delete]));

        let seen = payloads(&wire);
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].action(), GraphicsAction::Delete);
        assert_eq!(seen[0].id(), Some(5));
        assert_eq!(seen[0].size(), None, "no fact of the upload's: {seen:?}");
        assert_eq!(seen[0].chunks(), 1);
    }

    /// A terminal at power-on has no upload half-received, and kitty's own
    /// `RIS` drops the image being loaded, so the chunks still to come fail.
    /// The next transmission is then described by its own control block
    /// rather than completing the abandoned one (#587). `DECSTR` resets
    /// modes and keeps content, and kitty's keeps the upload too, so the
    /// next chunk after it completes the upload as if nothing had come
    /// between.
    #[test]
    fn a_hard_reset_abandons_an_unfinished_upload_and_a_soft_reset_does_not() {
        let fresh: &[u8] = b"\x1b_Ga=T,f=24,s=1,v=1,i=9;QUJD\x1b\\";
        let seen = payloads(&[UPLOAD_OF_5, b"\x1bc", fresh].concat());
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].id(), Some(9), "its own control block: {seen:?}");
        assert_eq!(seen[0].format(), GraphicsFormat::Rgb);
        assert_eq!(seen[0].size(), Some((1, 1)));
        assert_eq!(seen[0].chunks(), 1);
        assert_eq!(seen[0].data(), Some(&b"QUJD"[..]));

        let seen = payloads(&[UPLOAD_OF_5, b"\x1b[!p", b"\x1b_Gm=0;BBBB\x1b\\"].concat());
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].id(), Some(5), "the upload survived: {seen:?}");
        assert_eq!(seen[0].chunks(), 2);
        assert_eq!(seen[0].data(), Some(&b"AAAABBBB"[..]));
    }

    /// An upload cut by a sixel or a hard reset is left undefined by the
    /// protocol: "the client **must** finish sending all chunks for a single
    /// image before sending any other graphics related escape codes" (its
    /// "Remote client" section). termlens drops the upload on either, so the
    /// chunk that would have been its last is a transmission of its own,
    /// with none of the upload's facts, and nothing is owed a reply for it
    /// (#680). kitty 0.49 does otherwise, and neither is a promise: it
    /// ignores the sixel and completes the upload, and after a reset it
    /// refuses the chunk (`EILSEQ`). Pinned so a change is a decision.
    #[test]
    fn the_last_chunk_after_a_sixel_or_reset_is_no_part_of_the_upload() {
        let last: &[u8] = b"\x1b_Gm=0;BBBB\x1b\\";
        for between in [RED_SIXEL, b"\x1bc"] {
            let shown = String::from_utf8_lossy(between);
            let (seen, queries, tracker) = kitty_events(&[UPLOAD_OF_5, between, last].concat());
            let kitty: Vec<_> = seen
                .iter()
                .filter(|payload| payload.protocol() == GraphicsProtocol::Kitty)
                .collect();
            assert_eq!(kitty.len(), 1, "{shown}: {seen:?}");
            assert_eq!(kitty[0].id(), None, "{shown}: not the upload's: {seen:?}");
            assert_eq!(kitty[0].size(), None, "{shown}: {seen:?}");
            assert_eq!(kitty[0].chunks(), 1, "{shown}: {seen:?}");
            assert_eq!(kitty[0].data(), Some(&b"BBBB"[..]), "{shown}");
            assert_eq!(queries, [], "{shown}: the upload's reply is not owed");
            assert_eq!(tracker.graphics().kitty, 1, "{shown}");
        }
    }

    /// kitty answers a transmission or a placement that names an image
    /// (`finish_command_response` in its `graphics.c`) unless `q=` asks for
    /// quiet. termlens sends no reply, since it cannot know whether the
    /// image loaded, and names the command instead (#680): the payload
    /// comes with an unanswerable query carrying the shape of the escape
    /// that asked — for an upload, its first chunk, named at its last.
    #[test]
    fn a_kitty_command_kitty_would_answer_is_named_unanswered() {
        fn named(wire: &[u8]) -> Vec<String> {
            let (seen, queries, _) = kitty_events(wire);
            assert!(!seen.is_empty(), "still captured: {wire:?}");
            queries
                .into_iter()
                .map(|query| match query {
                    Query::Unanswerable(shape) => shape,
                    other => panic!("{other:?}"),
                })
                .collect()
        }
        let named_as = |wire: &[u8], shape: &str| {
            assert_eq!(named(wire), [shape], "{}", String::from_utf8_lossy(wire));
        };
        let unnamed = |wire: &[u8]| {
            assert_eq!(
                named(wire),
                [] as [String; 0],
                "{}",
                String::from_utf8_lossy(wire)
            );
        };

        // Transmissions, by id and by number, with and without an action.
        named_as(
            b"\x1b_Gi=7,a=T,f=24,s=1,v=1;QUJD\x1b\\",
            "^[_Gi=7,a=T,f=24,s=1,v=1;",
        );
        named_as(
            b"\x1b_Gi=7,f=24,s=1,v=1;QUJD\x1b\\",
            "^[_Gi=7,f=24,s=1,v=1;QUJD",
        );
        named_as(
            b"\x1b_GI=3,a=t,f=24,s=1,v=1;QUJD\x1b\\",
            "^[_GI=3,a=t,f=24,s=1,v=1;",
        );
        // A file transmission: kitty would read the file and say.
        named_as(b"\x1b_Gi=7,t=f;L3RtcC94\x1b\\", "^[_Gi=7,t=f;L3RtcC94^[\\");
        // A placement.
        named_as(b"\x1b_Ga=p,i=7\x1b\\", "^[_Ga=p,i=7^[\\");
        // Quiet: `q=1` silences the `OK`, `q=2` everything.
        for quiet in [b"q=1", b"q=2"] {
            let quiet = String::from_utf8_lossy(quiet);
            unnamed(format!("\x1b_Gi=7,{quiet},a=T,f=24,s=1,v=1;QUJD\x1b\\").as_bytes());
            unnamed(format!("\x1b_Ga=p,i=7,{quiet}\x1b\\").as_bytes());
        }
        // Nothing to address a reply to, and actions kitty does not answer.
        unnamed(b"\x1b_Ga=T,f=24,s=1,v=1;QUJD\x1b\\");
        unnamed(b"\x1b_Gi=0,a=T,f=24,s=1,v=1;QUJD\x1b\\");
        unnamed(b"\x1b_Ga=d,d=I,i=7\x1b\\");
        // Both an id and a number: refused with an error, which `q=1` does
        // not silence.
        named_as(b"\x1b_Ga=p,i=7,I=3,q=1\x1b\\", "^[_Ga=p,i=7,I=3,q=1^[\\");
        unnamed(b"\x1b_Ga=p,i=7,I=3,q=2\x1b\\");

        // An upload is answered once, at its last chunk, and the first
        // chunk's quiet holds unless a later one sets its own.
        let first = UPLOAD_OF_5;
        let last: &[u8] = b"\x1b_Gm=0;BBBB\x1b\\";
        let (_, queries, _) = kitty_events(first);
        assert_eq!(queries, [], "nothing owed before the last chunk");
        named_as(&[first, last].concat(), "^[_Ga=T,f=32,s=2,v=1,i=5,");
        unnamed(&[first, b"\x1b_Gm=0,q=1;BBBB\x1b\\"].concat());
        unnamed(
            &[
                b"\x1b_Ga=T,f=32,s=2,v=1,i=5,q=2,m=1;AAAA\x1b\\".as_slice(),
                last,
            ]
            .concat(),
        );
        // A placement in the middle of an upload is answered for itself.
        let (_, queries, _) = kitty_events(&[first, b"\x1b_Ga=p,i=9\x1b\\"].concat());
        assert_eq!(queries, [Query::Unanswerable("^[_Ga=p,i=9^[\\".into())]);
        // And one dropped by a delete owes nothing.
        unnamed(&[first, b"\x1b_Ga=d,d=i,i=5\x1b\\", last].concat());
    }

    /// Only an APC opening with `G` is kitty's, continuation chunks
    /// included. Any other APC used to be taken as the upload's next chunk —
    /// and, carrying no `m=1`, as its last. kitty ignores one, and so the
    /// upload goes on to the chunk that really is its last.
    #[test]
    fn an_apc_that_is_not_kitty_leaves_an_upload_alone() {
        let wire = [
            UPLOAD_OF_5,
            b"\x1b_Xnot kitty;BBBB\x1b\\",
            b"\x1b_Gm=0;CCCC\x1b\\",
        ]
        .concat();
        let seen = payloads(&wire);
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].chunks(), 2);
        assert_eq!(seen[0].data(), Some(&b"AAAACCCC"[..]));
    }

    /// Whether an APC is kitty's is read off its own first byte. The gate
    /// read slot 0 of the head buffer whatever the head's length, and a
    /// reset only zeroes the length, so an empty APC (`ESC _ ESC \`) after
    /// a kitty escape found that escape's `G` there and was counted as a
    /// kitty transmission of 0 bytes (#735). An APC with a first byte of
    /// its own overwrote the slot, and is pinned here beside it.
    #[test]
    fn an_apc_is_kittys_only_by_its_own_first_byte() {
        for apc in [&b"\x1b_\x1b\\"[..], b"\x1b_X\x1b\\", b"\x1b_XG;AAAA\x1b\\"] {
            let shown = String::from_utf8_lossy(apc);
            let wire = [&b"\x1b_Ga=d\x1b\\"[..], apc].concat();
            let (seen, _, tracker) = kitty_events(&wire);
            assert_eq!(seen.len(), 1, "{shown}: {seen:?}");
            assert_eq!(tracker.graphics().kitty, 0, "{shown}");
            // Nor is one taken as the next chunk of an upload.
            let wire = [UPLOAD_OF_5, apc, b"\x1b_Gm=0;CCCC\x1b\\"].concat();
            let (seen, _, tracker) = kitty_events(&wire);
            assert_eq!(seen.len(), 1, "{shown}: {seen:?}");
            assert_eq!(seen[0].chunks(), 2, "{shown}");
            assert_eq!(seen[0].data(), Some(&b"AAAACCCC"[..]), "{shown}");
            assert_eq!(tracker.graphics().kitty, 1, "{shown}");
        }
    }

    /// A control block naming every key a transmission and its placement
    /// take, each at a long value, as a client may send it: 142 bytes after
    /// the `G`. Byte 127 falls inside `r=1000`.
    const LONG_CONTROL: &str = "a=T,f=24,t=d,o=z,i=4294967295,p=4294967295,s=10000,v=10000,\
        x=10000,y=10000,w=10000,h=10000,X=100,Y=100,z=-1073741824,c=1000,r=1000,C=1,q=2,m=1";

    /// kitty reads a control block off its escape buffer, however long it
    /// is (`parse_graphics_code`). termlens read it from the first 128
    /// bytes of the escape: a value that straddled byte 127 was cut short
    /// (`r=1000` read as `r=1`) and every key past it was dropped — and
    /// when one of those was the `m=1` of a first chunk, the upload ended
    /// at once and its second chunk was counted as an image of its own
    /// (#700).
    #[test]
    fn a_kitty_control_block_is_read_past_its_first_128_bytes() {
        let head = &format!("G{LONG_CONTROL}")[..128];
        assert!(head.ends_with(",r=1"), "byte 127 must fall in `r=`: {head}");
        let first = format!("\x1b_G{LONG_CONTROL};AAAA\x1b\\");
        let wire = [first.as_bytes(), b"\x1b_Gm=0;BBBB\x1b\\"].concat();
        // At every capture bound: the keys are not read off the retained
        // body, which a bound of zero does not keep.
        for capture in [crate::graphics::DEFAULT_CAPTURE, 0] {
            let mut tracker = SeqTracker::new(capture, TEST_ROWS, TEST_COLS);
            let mut seen = Vec::new();
            for &byte in &wire {
                if let SeqEvent::Graphics(payload, placement) = tracker.step(byte) {
                    seen.push((*payload, placement));
                }
            }
            assert_eq!(seen.len(), 1, "one image in two chunks: {seen:?}");
            let (image, placement) = &seen[0];
            assert_eq!(image.chunks(), 2, "capture {capture}");
            assert_eq!(image.id(), Some(u32::MAX));
            assert_eq!(image.cells(), Some((1000, 1000)), "`r=` whole");
            assert!(!placement.moves, "`C=1` sits past byte 127");
            assert_eq!(tracker.graphics().kitty, 1, "capture {capture}");
            assert_eq!(
                image.data(),
                (capture > 0).then_some(&b"AAAABBBB"[..]),
                "capture {capture}"
            );
        }
        // The reviewer's shape: keys kitty accepts repeated pad the block,
        // and every fact of the image sits past byte 127.
        let pad = "z=-1073741824,".repeat(9);
        let first = format!("\x1b_Ga=t,{pad}i=9,f=24,s=1,v=2,m=1;AAAA\x1b\\");
        let seen = payloads(&[first.as_bytes(), b"\x1b_Gm=0;AAAA\x1b\\"].concat());
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(
            (seen[0].id(), seen[0].format(), seen[0].size()),
            (Some(9), GraphicsFormat::Rgb, Some((1, 2)))
        );
    }

    /// The control block is kept up to the length at which kitty gives up on
    /// an escape that is still arriving, and no further. A key that ends on
    /// the near side of [`KITTY_CONTROL_MAX`] is read; one the bound cuts in
    /// two is dropped whole rather than read as its first digits, which is
    /// what the old head did to `r=1000`; and one past it is not read.
    #[test]
    fn a_kitty_control_block_is_cut_where_kitty_stops_reading() {
        // `Ga=t,`, then `pad` bytes of pairs kitty accepts, then `key`.
        let id_after = |pad: usize, key: &str| {
            let full = pad / 5 - 1;
            let last = "1".repeat(pad - 5 * full - 3);
            let wire = format!(
                "\x1b_Ga=t,{}z={last},{key};AAAA\x1b\\",
                "z=-1,".repeat(full)
            );
            let seen = payloads(wire.as_bytes());
            assert_eq!(seen.len(), 1, "{pad} bytes, then {key}");
            seen[0].id()
        };
        let room = KITTY_CONTROL_MAX - "Ga=t,".len();
        assert_eq!(id_after(room - 3, "i=9"), Some(9), "ends on the bound");
        assert_eq!(id_after(room - 3, "i=91"), None, "not read as `i=9`");
        assert_eq!(id_after(room - 2, "i=9"), None, "cut after its `=`");
        assert_eq!(id_after(room, "i=9"), None, "starts past the bound");
    }

    /// A placement in the middle of an upload is kitty's `case 'p'`, which
    /// never touches the load in progress (`grman_handle_command`); the
    /// action-less chunk after it then completes the load (#636). The
    /// placement was taken as the upload's last chunk instead, and the real
    /// last chunk as a transmission of its own: two images, neither of which
    /// decoded, and the `i=9` placement gone.
    #[test]
    fn a_placement_mid_upload_is_its_own_payload_and_the_upload_goes_on() {
        let wire: &[u8] = b"\x1b_Ga=T,f=24,s=2,v=1,i=1,m=1;/wAA\x1b\\\
            \x1b_Ga=p,i=9,c=2,r=1\x1b\\\
            \x1b_Gm=0;AAD/\x1b\\";
        let seen = payloads(wire);
        assert_eq!(seen.len(), 2, "{seen:?}");
        let (placed, image) = (&seen[0], &seen[1]);
        assert_eq!(placed.action(), GraphicsAction::Place, "{placed:?}");
        assert_eq!(placed.id(), Some(9));
        assert_eq!(placed.cells(), Some((2, 1)));
        assert_eq!(placed.chunks(), 1);
        assert_eq!(image.action(), GraphicsAction::TransmitAndPlace);
        assert_eq!(image.id(), Some(1));
        assert_eq!(image.chunks(), 2);
        assert_eq!(image.data(), Some(&b"/wAAAAD/"[..]));
        let counts = fed(wire).graphics();
        assert_eq!(counts.kitty, 1, "one image: {counts:?}");
        assert_eq!(counts.bytes, wire.len() as u64 - 12, "every escape's cost");
        // Animation and compose have their own cases too.
        for beside in [&b"\x1b_Ga=a,i=1,s=3\x1b\\"[..], b"\x1b_Ga=c,i=1,r=2\x1b\\"] {
            let seen = payloads(&[UPLOAD_OF_5, beside, b"\x1b_Gm=0;BBBB\x1b\\"].concat());
            assert_eq!(seen.len(), 2, "{seen:?}");
            assert_eq!(seen[0].action(), GraphicsAction::Other);
            assert_eq!(seen[1].id(), Some(5), "{seen:?}");
            assert_eq!(seen[1].data(), Some(&b"AAAABBBB"[..]));
        }
    }

    /// The transmit actions are kitty's chunk continuations
    /// (`handle_add_command` continues the load in progress for any direct
    /// transmission), so they are left as they were; one from outside the
    /// stream (`t=f`) starts an image of its own there, dropping the load.
    #[test]
    fn a_transmission_mid_upload_continues_it_unless_it_names_a_file() {
        let seen = payloads(&[UPLOAD_OF_5, b"\x1b_Ga=t,i=8,m=0;BBBB\x1b\\"].concat());
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].id(), Some(5), "the upload's own keys: {seen:?}");
        assert_eq!(seen[0].data(), Some(&b"AAAABBBB"[..]));

        let file: &[u8] = b"\x1b_Ga=t,t=f,i=8;L3RtcC94\x1b\\";
        let seen = payloads(&[UPLOAD_OF_5, file].concat());
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].id(), Some(8), "its own image: {seen:?}");
        assert_eq!(seen[0].chunks(), 1);
        assert_eq!(fed(&[UPLOAD_OF_5, file].concat()).graphics().kitty, 1);
    }

    /// A query with an id and its data in the escape is, mid-upload, that
    /// upload's next chunk: kitty's `grman_handle_command` hands `a=q` to
    /// `handle_add_command`, which continues the load in progress for any
    /// direct transmission (`init_img = false`). Without `m=1` it is the
    /// last one, so the upload's image is complete and the query is
    /// answered — kitty's 0.49 `graphics.c` (#672). termlens left the upload
    /// alone and answered the query on its own.
    #[test]
    fn a_kitty_query_mid_upload_is_the_uploads_last_chunk() {
        let query: &[u8] = b"\x1b_Ga=q,i=31;BBBB\x1b\\";
        let (seen, queries, tracker) = kitty_events(&[UPLOAD_OF_5, query].concat());
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].id(), Some(5), "the upload's image: {seen:?}");
        assert_eq!(seen[0].action(), GraphicsAction::TransmitAndPlace);
        assert_eq!(seen[0].chunks(), 2);
        assert_eq!(seen[0].data(), Some(&b"AAAABBBB"[..]));
        assert_eq!(loads(&queries), [(Some(31), KittyLoad::Loaded)]);
        assert_eq!(tracker.graphics().kitty, 1);
        assert_eq!(tracker.graphics().bytes, wire_cost(&[UPLOAD_OF_5, query]));
    }

    /// The queries that never reach kitty's loader leave an upload alone:
    /// one naming both an image id and an image number (refused before
    /// anything else, "Must not specify both image id and image number"),
    /// one without an id ("Query graphics command without image id"), and a
    /// feature query (`e=`), which kitty 0.49 rejects as a key it does not
    /// know and later kitty keeps clear of any load in progress.
    #[test]
    fn a_kitty_query_that_never_reaches_the_loader_leaves_an_upload_alone() {
        for query in [
            &b"\x1b_Ga=q,i=31,I=7;BBBB\x1b\\"[..],
            b"\x1b_Ga=q;BBBB\x1b\\",
            b"\x1b_Ga=q,i=0;BBBB\x1b\\",
            b"\x1b_Ga=q,i=31,e=T;BBBB\x1b\\",
        ] {
            let wire = [UPLOAD_OF_5, query, b"\x1b_Gm=0;CCCC\x1b\\"].concat();
            let (seen, queries, _) = kitty_events(&wire);
            let shown = String::from_utf8_lossy(query);
            assert_eq!(seen.len(), 1, "{shown}: {seen:?}");
            assert_eq!(seen[0].data(), Some(&b"AAAACCCC"[..]), "{shown}");
            assert_eq!(loads(&queries[..1])[0].1, KittyLoad::Loaded, "{shown}");
            // And the upload, `i=5` without `q=`, is owed kitty's `OK` at
            // its last chunk, which is named rather than sent (#680).
            assert_eq!(
                queries[1..],
                [Query::Unanswerable("^[_Ga=T,f=32,s=2,v=1,i=5,".into())],
                "{shown}"
            );
        }
    }

    /// With `m=1`, kitty loads the query's data and then, as after every
    /// query, trims each image whose data is not loaded yet
    /// (`remove_images(self, add_trim_predicate, 0)`) — the very image being
    /// loaded. Its loader still names that image, so the chunk after is
    /// refused (`EILSEQ`, "More payload loading refers to non-existent
    /// image") and clears it (#672). No image comes of any of it, and
    /// nothing answers the query.
    #[test]
    fn a_kitty_query_with_m_1_leaves_kittys_loader_on_a_removed_image() {
        let query: &[u8] = b"\x1b_Ga=q,i=31,m=1;BBBB\x1b\\";
        let last: &[u8] = b"\x1b_Gm=0;CCCC\x1b\\";
        let (seen, queries, tracker) = kitty_events(&[UPLOAD_OF_5, query, last].concat());
        assert!(seen.is_empty(), "{seen:?}");
        assert_eq!(loads(&queries), [(Some(31), KittyLoad::Pending)]);
        assert_eq!(tracker.graphics().kitty, 0);
        assert_eq!(
            tracker.graphics().bytes,
            wire_cost(&[UPLOAD_OF_5, query, last])
        );
        // A query split into chunks by itself goes the same way: its own
        // test image is the one removed.
        let (seen, queries, _) = kitty_events(&[query, last].concat());
        assert!(seen.is_empty(), "{seen:?}");
        assert_eq!(loads(&queries), [(Some(31), KittyLoad::Pending)]);
        // Once refused, the loader is clear: the next transmission is an
        // image like any other.
        let fresh: &[u8] = b"\x1b_Ga=T,f=24,s=1,v=1,i=9;QUJD\x1b\\";
        let seen = payloads(&[UPLOAD_OF_5, query, last, fresh].concat());
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].id(), Some(9));
    }

    /// Whatever kitty would take as the load's next chunk is refused while
    /// the loader names a removed image — a fresh transmission with the
    /// data in the escape too (`init_img` is false whenever a load is under
    /// way), and a query, which gets `EILSEQ`. A delete frees the load
    /// first (`free_load_data`), and so does a transmission from a file;
    /// a placement leaves it as it is (#672).
    #[test]
    fn kittys_loader_on_a_removed_image_refuses_the_next_chunk_whatever_it_is() {
        let orphan: &[u8] = b"\x1b_Ga=q,i=31,m=1;AAAA\x1b\\";
        let fresh: &[u8] = b"\x1b_Ga=T,f=24,s=1,v=1,i=9;QUJD\x1b\\";
        let (seen, _, tracker) = kitty_events(&[orphan, fresh].concat());
        assert!(seen.is_empty(), "refused: {seen:?}");
        assert_eq!(
            tracker.graphics().bytes,
            wire_cost(&[fresh]),
            "but it crossed the wire"
        );

        let (_, queries, _) = kitty_events(
            &[
                orphan,
                b"\x1b_Ga=q,i=32;BBBB\x1b\\",
                b"\x1b_Ga=q,i=33;CCCC\x1b\\",
            ]
            .concat(),
        );
        assert_eq!(
            loads(&queries),
            [
                (Some(31), KittyLoad::Pending),
                (Some(32), KittyLoad::Gone),
                (Some(33), KittyLoad::Loaded),
            ]
        );

        // Whether the fresh transmission is an image after each of these.
        for (between, cleared) in [
            (&b"\x1b_Ga=d,d=I,i=4\x1b\\"[..], true),
            (b"\x1b_Ga=t,t=f,i=8;L3RtcC94\x1b\\", true),
            (b"\x1b_Ga=p,i=4\x1b\\", false),
        ] {
            let seen = payloads(&[orphan, between, fresh].concat());
            assert_eq!(
                seen.iter().any(|payload| payload.id() == Some(9)),
                cleared,
                "{}: {seen:?}",
                String::from_utf8_lossy(between)
            );
        }
    }

    /// A query from a file or shared memory starts a test load of its own,
    /// which drops the upload in progress (`handle_add_command` resets the
    /// loader when `init_img` is true) (#672).
    #[test]
    fn a_kitty_query_from_a_file_mid_upload_drops_the_upload() {
        let wire = [
            UPLOAD_OF_5,
            b"\x1b_Ga=q,i=31,t=f;L3RtcC94\x1b\\",
            b"\x1b_Gm=0;CCCC\x1b\\",
        ]
        .concat();
        let (seen, queries, _) = kitty_events(&wire);
        assert!(
            seen.iter().all(|payload| payload.id() != Some(5)),
            "{seen:?}"
        );
        assert_eq!(loads(&queries), [(Some(31), KittyLoad::Loaded)]);
    }

    /// Chunks each padded on their own, as the protocol allows: kitty
    /// decodes every escape's base64 by itself, so `/w==` then `AAA=` is
    /// the bytes `ff` `00 00` — one RGB pixel (#637). The joined body
    /// failed as "bad base64".
    #[cfg(feature = "decode")]
    #[test]
    fn kitty_chunks_padded_one_by_one_decode() {
        let seen = payloads(b"\x1b_Ga=T,f=24,s=1,v=1,m=1;/w==\x1b\\\x1b_Gm=0;AAA=\x1b\\");
        assert_eq!(seen[0].data(), Some(&b"/w==AAA="[..]));
        let bitmap = seen[0].decode().expect("decodes");
        assert_eq!(bitmap.pixel(0, 0), Some([0xff, 0, 0, 0xff]));
    }

    /// A sixel past the capture bound keeps its declared size: the raster
    /// attributes are measured as the bytes arrive, not read back off the
    /// data the bound kept (#635). At bound 0 and at a bound that cuts the
    /// stream inside the raster record, `size()` was `None`.
    #[test]
    fn a_sixel_keeps_its_declared_size_at_every_capture_bound() {
        let wire: &[u8] = b"\x1bPq\"1;1;8;45#0;2;100;0;0~~~~~~~~-~~~~~~~~\x1b\\";
        for capture in [crate::graphics::DEFAULT_CAPTURE, 20, 6, 0] {
            let mut tracker = SeqTracker::new(capture, TEST_ROWS, TEST_COLS);
            let mut seen = None;
            for &byte in wire {
                if let SeqEvent::Graphics(payload, placement) = tracker.step(byte) {
                    seen = Some((*payload, placement));
                }
            }
            let (payload, placement) = seen.expect("a payload");
            assert_eq!(payload.size(), Some((8, 45)), "capture {capture}");
            assert_eq!(placement.height, 45, "capture {capture}");
            assert_eq!(
                payload.data().is_some(),
                capture == crate::graphics::DEFAULT_CAPTURE,
                "capture {capture}"
            );
        }
    }

    /// A payload past the capture bound keeps every count and drops the
    /// data, rather than handing back a prefix that would decode into a
    /// plausible-looking wrong picture.
    #[test]
    fn a_payload_past_the_capture_bound_is_counted_and_not_kept() {
        let mut tracker = SeqTracker::new(8, TEST_ROWS, TEST_COLS);
        let mut seen = None;
        for &byte in b"\x1b_Ga=T,f=32,s=4,v=4;AAAABBBBCCCCDDDD\x1b\\" {
            if let SeqEvent::Graphics(payload, _) = tracker.step(byte) {
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
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        let mut events = Vec::new();
        for &b in b"\x1b_Gi=1,a=q;\x1b\\" {
            events.push(t.step(b));
        }
        assert!(
            events.iter().any(|e| matches!(
                e,
                SeqEvent::Query(Query::KittyGraphics { id: Some(1), shape, .. })
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
    /// next timeout of every application that draws. (One that names an
    /// image is owed kitty's reply, and is named for that: #680.)
    #[test]
    fn a_kitty_transmission_is_not_treated_as_a_query() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
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

    /// The label is what a reader sees: a link wrapped around line drawing
    /// records the glyphs, not the bytes that stood for them.
    #[test]
    fn a_label_drawn_in_the_graphics_set_records_the_glyphs() {
        let seen = links(b"\x1b(0\x1b]8;;http://x/\x1b\\lqk\x1b]8;;\x1b\\\x1b(B");
        assert_eq!(seen[0].2.as_deref(), Some("\u{250c}\u{2500}\u{2510}"));
        // SS2 is looked up twice (the rewriter, then the open span) and
        // consumed once: a shift that stuck would keep translating.
        let seen = links(b"\x1b*0\x1b]8;;http://x/\x1b\\\x1bNl|\x1b]8;;\x1b\\");
        assert_eq!(seen[0].2.as_deref(), Some("\u{250c}|"));
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

    /// The rest of the rule the RIS arm states (#516): the title is a window
    /// property `RIS` does not restore in xterm, and the bell count and the
    /// clipboard are records of what the application emitted, so a hard
    /// reset keeps all three. The cursor shape set in the same stream is
    /// asserted gone, so the reset demonstrably ran.
    #[test]
    fn a_hard_reset_keeps_the_title_the_bell_count_and_the_clipboard() {
        // Title (its BEL terminates the OSC), a real bell, a clipboard write
        // of "ab", a cursor shape, then `ESC c`.
        let t = fed(b"\x1b]0;my app\x07\x07\x1b]52;c;YWI=\x07\x1b[5 q\x1bc");
        assert_eq!(t.cursor_style(), None, "the reset ran");
        assert_eq!(&*t.title(), "my app", "the title survives it");
        assert_eq!(t.bells(), 1, "the bell count survives it");
        let clip = t.clipboard().expect("the clipboard write survives it");
        assert_eq!(clip.text(), Some("ab"));
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

    /// The tracker parses bytes a *child process* chooses, so every buffer
    /// in it is a place a hostile or merely broken program could push on.
    /// Each one is bounded in code; this is the test that says so out loud.
    ///
    /// Two phases, because they catch different things. Random bytes explore
    /// the state machine and are the net for a panic — an index, a slice, an
    /// arithmetic overflow — on input nobody thought about. They do *not*
    /// reach any bound: a bound needs thousands of bytes pushed the same way
    /// on purpose, which random bytes essentially never do. So the second
    /// phase writes the shapes a hostile child would actually use.
    ///
    /// The peak assertions at the end are the point. Without them this test
    /// would go on passing if the corpus stopped stressing the buffers, and
    /// its bound checks would be decoration — which is what they were when
    /// first written here, and what the mutation pass caught.
    ///
    /// Deterministic: the seed is the whole corpus, so a failure reproduces.
    #[test]
    fn hostile_input_cannot_panic_or_grow_a_buffer_past_its_bound() {
        let capture = crate::graphics::DEFAULT_CAPTURE;
        let mut tracker = SeqTracker::new(capture, TEST_ROWS, TEST_COLS);
        // (links, label, osc, dcs_body, kitty_control) high-water marks.
        type Peak = (usize, usize, usize, usize, usize);
        let mut peak: Peak = (0, 0, 0, 0, 0);

        fn observe(t: &SeqTracker, capture: usize, peak: &mut Peak) {
            assert!(t.links.len() <= LINK_HISTORY, "link log overran");
            assert!(t.link_label.len() <= LINK_LABEL_MAX, "label overran");
            assert!(t.osc_buf.len() <= OSC_CAPTURE_MAX, "osc buffer overran");
            assert!(t.dcs_body.len() <= capture, "dcs body overran");
            assert!(usize::from(t.seq_len) <= t.seq_buf.len());
            assert!(usize::from(t.dcs_head_len) <= t.dcs_head.len());
            assert!(
                t.kitty_control.len() <= KITTY_CONTROL_MAX,
                "kitty control block overran"
            );
            peak.0 = peak.0.max(t.links.len());
            peak.1 = peak.1.max(t.link_label.len());
            peak.2 = peak.2.max(t.osc_buf.len());
            peak.3 = peak.3.max(t.dcs_body.len());
            peak.4 = peak.4.max(t.kitty_control.len());
        }

        // Phase 1 — the panic net. xorshift64: a few lines, no dependency,
        // reproducible. Weighted toward the introducers that actually drive
        // the state machine; uniform noise would sit in Ground almost always.
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        const PALETTE: &[u8] =
            b"\x1b[]P_^X;:0123456789 qhlmc\\\x07\x18\x1a~?$8abzId=\xff\x80\xc3\n\r\t";
        for _ in 0..400_000 {
            tracker.step(PALETTE[(rand() as usize) % PALETTE.len()]);
            observe(&tracker, capture, &mut peak);
        }

        // Phase 2 — the shapes a hostile child would use, each aimed at one
        // buffer and overrunning it by a clear margin.
        let feed = |bytes: &[u8], t: &mut SeqTracker, peak: &mut _| {
            for &b in bytes {
                t.step(b);
                observe(t, capture, peak);
            }
        };
        // (a) far more spans than the log holds.
        for n in 0..LINK_HISTORY * 3 {
            feed(
                format!("\x1b]8;;http://x/{n}\x1b\\L\x1b]8;;\x1b\\").as_bytes(),
                &mut tracker,
                &mut peak,
            );
        }
        // (b) one span whose label runs well past the bound.
        feed(b"\x1b]8;;http://x/\x1b\\", &mut tracker, &mut peak);
        feed(&vec![b'L'; LINK_LABEL_MAX * 2], &mut tracker, &mut peak);
        feed(b"\x1b]8;;\x1b\\", &mut tracker, &mut peak);
        // (c) one OSC longer than the capture bound.
        feed(b"\x1b]0;", &mut tracker, &mut peak);
        feed(&vec![b'T'; OSC_CAPTURE_MAX * 2], &mut tracker, &mut peak);
        feed(b"\x07", &mut tracker, &mut peak);
        // (d) one kitty control block that never reaches its `;`.
        feed(b"\x1b_G", &mut tracker, &mut peak);
        feed(
            &b"z=-1,".repeat(KITTY_CONTROL_MAX / 5 * 2),
            &mut tracker,
            &mut peak,
        );
        feed(b"\x1b\\", &mut tracker, &mut peak);

        // The corpus must actually have reached each bound, or every check
        // above was inert.
        assert_eq!(peak.0, LINK_HISTORY, "the link log was never filled");
        assert_eq!(peak.1, LINK_LABEL_MAX, "the label bound was never reached");
        assert_eq!(peak.2, OSC_CAPTURE_MAX, "the osc bound was never reached");
        assert_eq!(
            peak.4, KITTY_CONTROL_MAX,
            "the kitty control bound was never reached"
        );

        // And the accessors stay total on whatever state that left behind.
        let _ = tracker.title();
        let _ = tracker.clipboard();
        let _ = tracker.links();
        let _ = tracker.cursor_style();
        let _ = tracker.graphics();
    }

    /// The same, on the grammar rather than the alphabet: well-formed
    /// `OSC 8` spans interleaved with the sequences most likely to collide
    /// with them, driven long enough to exercise eviction many times over.
    #[test]
    fn a_long_well_formed_stream_stays_bounded_and_consistent() {
        let mut tracker = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        for n in 0..5_000u32 {
            let chunk = format!(
                "\x1b]8;id={n};http://example.invalid/{n}\x1b\\label{n}\x1b]8;;\x1b\\\
                 \x1b]0;title{n}\x07\x1b[{} q text\r\n",
                n % 7
            );
            tracker.feed(chunk.as_bytes());
            assert!(tracker.links.len() <= LINK_HISTORY);
        }
        let links = tracker.links();
        assert_eq!(links.len(), LINK_HISTORY, "the log fills and then holds");
        // Oldest evicted, newest kept, every retained span complete.
        assert!(links.iter().all(|l| l.closed() && l.label().is_some()));
        assert_eq!(links[LINK_HISTORY - 1].uri(), "http://example.invalid/4999");
        assert_eq!(links[LINK_HISTORY - 1].label(), Some("label4999"));
    }

    /// The glyphs a fed tracker would hand the grid for `text`, byte by
    /// byte: the translation where one applies, the byte itself otherwise.
    fn drawn(bytes: &[u8]) -> String {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        let mut out = String::new();
        for &b in bytes {
            match t.charset_glyph(b) {
                Some(glyph) => out.push_str(glyph),
                None if t.state == State::Ground && (0x20..0x7f).contains(&b) => {
                    out.push(b as char);
                }
                None => {}
            }
            t.step(b);
        }
        out
    }

    /// The reproduction from the issue, and the shape ncurses emits on an
    /// xterm terminfo: designate into G0, draw, designate ASCII back.
    #[test]
    fn dec_special_graphics_in_g0_translates_the_bytes_it_redefines() {
        assert_eq!(drawn(b"lqqqk"), "lqqqk", "ASCII until designated");
        assert_eq!(
            drawn(b"\x1b(0lqqqk\x1b(B"),
            "\u{250c}\u{2500}\u{2500}\u{2500}\u{2510}"
        );
        // Back to ASCII after the redesignation, and the letters are letters.
        assert_eq!(drawn(b"\x1b(0lq\x1b(Blq"), "\u{250c}\u{2500}lq");
        // Bytes below `_` are the same in both sets.
        assert_eq!(drawn(b"\x1b(0A1 +\x1b(B"), "A1 +");
        // The designation's own final byte is never a glyph.
        assert_eq!(drawn(b"\x1b(0\x1b(0"), "");
    }

    /// The vt100-terminfo shape: designate into G1 once, then SO/SI.
    #[test]
    fn shift_out_invokes_g1_and_shift_in_returns_to_g0() {
        assert_eq!(drawn(b"\x1b)0lqk"), "lqk", "G1 designated but not invoked");
        assert_eq!(
            drawn(b"\x1b)0\x0elqk\x0flqk"),
            "\u{250c}\u{2500}\u{2510}lqk"
        );
        // SO with an ASCII G1 changes nothing.
        assert_eq!(drawn(b"\x0elqk"), "lqk");
        // And G0 can be the graphics set while G1 is ASCII: SO turns it off.
        assert_eq!(drawn(b"\x1b(0q\x0eq\x0fq"), "\u{2500}q\u{2500}");
    }

    /// The issue's reproduction: designate G2/G3, single-shift one graphic,
    /// then the locked set resumes. `|` is itself a Special Graphics byte
    /// (`≠`), so a shift that stuck would translate it too.
    #[test]
    fn ss2_invokes_g2_for_one_character() {
        assert_eq!(drawn(b"\x1b*0\x1bNl\x1b(B|"), "\u{250c}|");
        assert_eq!(
            drawn(b"\x1b*0\x1bNll"),
            "\u{250c}l",
            "the character after the shift is the locked set again"
        );
        // Designated but not invoked: G2 does not replace the locking shift.
        assert_eq!(drawn(b"\x1b*0l"), "l");
        // SS2 with G2 still ASCII changes nothing, and does not stick.
        assert_eq!(drawn(b"\x1bNlq"), "lq");
    }

    #[test]
    fn ss3_invokes_g3_for_one_character() {
        assert_eq!(drawn(b"\x1b+0\x1bOl\x1b(B|"), "\u{250c}|");
        assert_eq!(drawn(b"\x1b+0\x1bOll"), "\u{250c}l");
        assert_eq!(drawn(b"\x1b+0l"), "l");
    }

    /// A single shift overrides the locked set without disturbing it: SO
    /// stays in G1 around the one G2 character.
    #[test]
    fn a_single_shift_overrides_the_locking_shift_for_one_character() {
        // G1 graphics, G2 ASCII: SO then SS2 then `lqk` is `l` then `─┐`.
        assert_eq!(drawn(b"\x1b)0\x1b*B\x0e\x1bNlqk"), "l\u{2500}\u{2510}");
        // And the other way: G0 graphics, SS2 from ASCII G2 turns one
        // byte back into a letter without leaving the graphics set.
        assert_eq!(drawn(b"\x1b(0\x1b*B\x1bNlq"), "l\u{2500}");
    }

    /// SS2/SS3 apply to the next *character*, not the next byte. An
    /// intervening SI or a G0 redesignation must not eat the shift.
    #[test]
    fn a_single_shift_survives_intervening_controls_and_designations() {
        assert_eq!(drawn(b"\x1b*0\x1bN\x0fl"), "\u{250c}");
        assert_eq!(drawn(b"\x1b*0\x1bN\x7fl"), "\u{250c}");
        assert_eq!(drawn(b"\x1b*0\x1bN\x1b(Bl"), "\u{250c}");
        // A designation into G2 between the shift and the character still
        // applies: the slot is read when the character is drawn.
        assert_eq!(drawn(b"\x1b*0\x1bN\x1b*Bl"), "l");
        assert_eq!(drawn(b"\x1bN\x1b*0l"), "\u{250c}");
    }

    /// A single shift is one *character*: CJK, emoji and `é` consume it,
    /// so the graphics byte after them is a letter. `drawn` skips UTF-8,
    /// so this asks the tracker after the character rather than concatenating.
    #[test]
    fn a_multibyte_character_consumes_a_single_shift() {
        for ch in ["汉", "🦀", "é"] {
            let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
            t.feed(b"\x1b*0\x1bN");
            assert_eq!(
                t.charset_glyph(b'l'),
                Some("\u{250c}"),
                "shift pending before {ch}"
            );
            t.feed(ch.as_bytes());
            assert_eq!(
                t.charset_glyph(b'l'),
                None,
                "shift must not survive {ch} and translate the next l"
            );
        }
    }

    #[test]
    fn a_hard_reset_returns_all_four_sets_to_ascii() {
        assert_eq!(drawn(b"\x1b(0\x1b)0\x0eq\x1bcq\x0fq"), "\u{2500}qq");
        // Pending SS2 does not survive RIS. Redesignating G2 afterwards
        // without a new shift would still translate `l` if the shift stuck,
        // which is worse than never shifting: the rest of the line goes.
        assert_eq!(drawn(b"\x1b*0\x1bN\x1bc\x1b*0l"), "l");
        // G2 itself is ASCII again, so a new SS2 without redesignating is
        // a letter too.
        assert_eq!(drawn(b"\x1b*0\x1bc\x1bNl"), "l");
        assert_eq!(drawn(b"\x1b+0\x1bO\x1bc\x1b+0l"), "l");
    }

    /// Only a byte in ground state is a character: the same bytes inside an
    /// OSC title, a CSI parameter or a DCS payload must pass untouched.
    #[test]
    fn bytes_inside_sequences_are_never_translated() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        for &b in b"\x1b(0" {
            t.step(b);
        }
        assert_eq!(t.charset_glyph(b'q'), Some("\u{2500}"));
        for &b in b"\x1b]0;lqqk" {
            assert_eq!(t.charset_glyph(b), None, "inside an OSC: {b:?}");
            t.step(b);
        }
        t.step(0x07);
        assert_eq!(&*t.title(), "lqqk", "the title keeps its letters");
        for &b in b"\x1b[3" {
            assert_eq!(t.charset_glyph(b), None, "inside a CSI: {b:?}");
            t.step(b);
        }
        t.step(b'm');
        assert_eq!(t.charset_glyph(b'x'), Some("\u{2502}"), "and ground again");
    }

    /// Other national sets and the alternate ROMs read as ASCII, and a second
    /// intermediate is not a designation at all.
    #[test]
    fn other_designations_read_as_ascii() {
        for seq in [
            &b"\x1b(A"[..],
            b"\x1b(B",
            b"\x1b(1",
            b"\x1b*A",
            b"\x1b+B",
            b"\x1b(2",
            b"\x1b(<",
        ] {
            let mut bytes = seq.to_vec();
            bytes.push(b'q');
            assert_eq!(drawn(&bytes), "q", "{seq:?}");
        }
        // `ESC ( 0` then `ESC % G` (select UTF-8): the second sequence has
        // two intermediates and designates nothing, so G0 stays graphics.
        assert_eq!(drawn(b"\x1b(0\x1b%Gq"), "\u{2500}");
        // Same for a national set in G2/G3, invoked by the single shift.
        assert_eq!(drawn(b"\x1b*A\x1bNq"), "q");
        assert_eq!(drawn(b"\x1b+A\x1bOq"), "q");
    }

    #[test]
    fn a_designation_split_across_feeds_still_applies() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        t.feed(b"\x1b(");
        assert!(t.mid_sequence());
        t.feed(b"0");
        assert_eq!(t.charset_glyph(b'l'), Some("\u{250c}"));

        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        t.feed(b"\x1b*");
        assert!(t.mid_sequence());
        t.feed(b"0\x1bN");
        assert!(!t.mid_sequence());
        assert_eq!(t.charset_glyph(b'l'), Some("\u{250c}"));
    }

    #[test]
    fn plain_text_is_ground() {
        assert!(!fed(b"hello world\r\n").mid_sequence());
    }

    #[test]
    fn split_csi_is_mid_sequence_until_final_byte() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
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
        assert!(fed(b"\x1b*").mid_sequence());
        assert!(!fed(b"\x1b*0").mid_sequence());
        // SS2/SS3 are two-character escapes; they must not stay mid-sequence
        // or wait_idle would hang after a lone graphic shift.
        assert!(!fed(b"\x1bN").mid_sequence());
        assert!(!fed(b"\x1bO").mid_sequence());
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
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        let events: Vec<SeqEvent> = b"\x1b[?2026h".iter().map(|&b| t.step(b)).collect();
        assert_eq!(*events.last().unwrap(), SeqEvent::SyncBegin);
        assert!(t.in_sync_update());
        let events: Vec<SeqEvent> = b"\x1b[?2026l".iter().map(|&b| t.step(b)).collect();
        assert_eq!(*events.last().unwrap(), SeqEvent::SyncEnd);
        assert!(!t.in_sync_update());
    }

    /// The hard reset is the one sequence that empties the backend's
    /// history, so the emulator must hear about it to save what scrolled
    /// before it (#391). `CSI c` is a device-attributes query, not the
    /// reset, and the same final byte must not fire the event.
    #[test]
    fn hard_reset_fires_an_event_and_csi_c_does_not() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        let events: Vec<SeqEvent> = b"\x1bc".iter().map(|&b| t.step(b)).collect();
        assert_eq!(*events.last().unwrap(), SeqEvent::HardReset);

        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        let events: Vec<SeqEvent> = b"\x1b[c".iter().map(|&b| t.step(b)).collect();
        assert!(!events.contains(&SeqEvent::HardReset));
    }

    /// The mouse encoding is the last of 1005, 1006 and 1015 set, and a
    /// reset clears only the one in force — xterm's rule, which vt100 keeps
    /// for the two it implements and which 1015 joins here (#640).
    #[test]
    fn the_mouse_encoding_is_the_last_one_set() {
        let encoding = |bytes: &[u8]| fed(bytes).mouse_encoding();
        assert_eq!(encoding(b""), MouseEncoding::Legacy);
        assert_eq!(encoding(b"\x1b[?1000h"), MouseEncoding::Legacy);
        assert_eq!(encoding(b"\x1b[?1015h"), MouseEncoding::Urxvt);
        assert_eq!(encoding(b"\x1b[?1005h"), MouseEncoding::Utf8);
        assert_eq!(encoding(b"\x1b[?1006h\x1b[?1015h"), MouseEncoding::Urxvt);
        assert_eq!(encoding(b"\x1b[?1015h\x1b[?1006h"), MouseEncoding::Sgr);
        // In one list, the last named wins, as vt100 applies them in order.
        assert_eq!(encoding(b"\x1b[?1000;1006;1015h"), MouseEncoding::Urxvt);
        assert_eq!(encoding(b"\x1b[?1015;1006h"), MouseEncoding::Sgr);
        // Resetting the one in force goes back to the legacy form, not to
        // the one it replaced; resetting another leaves it alone.
        assert_eq!(
            encoding(b"\x1b[?1006h\x1b[?1015h\x1b[?1015l"),
            MouseEncoding::Legacy
        );
        assert_eq!(
            encoding(b"\x1b[?1006h\x1b[?1015h\x1b[?1006l"),
            MouseEncoding::Urxvt
        );
        assert_eq!(
            encoding(b"\x1b[?1015h\x1b[?1006;1015l"),
            MouseEncoding::Legacy
        );
        // RIS returns it to power-on; DECSTR keeps it, as xterm does (#687).
        assert_eq!(encoding(b"\x1b[?1015h\x1bc"), MouseEncoding::Legacy);
        assert_eq!(encoding(b"\x1b[?1015h\x1b[!p"), MouseEncoding::Urxvt);
        // An ANSI mode of the same number is not a mouse encoding.
        assert_eq!(encoding(b"\x1b[1015h"), MouseEncoding::Legacy);
    }

    /// The protocol mouse events are reported in is the tracking mode set
    /// last, and a reset of any of the four turns reporting off, whichever
    /// is in force — xterm's `really_set_mousemode`, which vt100 follows
    /// only for a reset of the mode it holds (#693). Unlike the encoding
    /// above, where a reset clears only the one in force.
    #[test]
    fn the_mouse_protocol_is_the_last_mode_set_and_any_reset_ends_it() {
        let protocol = |bytes: &[u8]| fed(bytes).mouse_protocol();
        assert_eq!(protocol(b""), MouseMode::None);
        assert_eq!(protocol(b"\x1b[?9h"), MouseMode::Press);
        assert_eq!(protocol(b"\x1b[?1000h"), MouseMode::PressRelease);
        assert_eq!(protocol(b"\x1b[?1002h"), MouseMode::ButtonMotion);
        assert_eq!(protocol(b"\x1b[?1003h"), MouseMode::AnyMotion);
        // The last one set wins, whichever reports more.
        assert_eq!(protocol(b"\x1b[?1000h\x1b[?1003h"), MouseMode::AnyMotion);
        assert_eq!(protocol(b"\x1b[?1003h\x1b[?1000h"), MouseMode::PressRelease);
        // In one list too, which is applied in order.
        assert_eq!(protocol(b"\x1b[?1000;1002;1003h"), MouseMode::AnyMotion);
        assert_eq!(protocol(b"\x1b[?1003;1006;9h"), MouseMode::Press);
        // A reset of the mode in force, of another one, or of one never
        // set: reporting is off after each.
        assert_eq!(protocol(b"\x1b[?1002h\x1b[?1002l"), MouseMode::None);
        assert_eq!(protocol(b"\x1b[?1002h\x1b[?1000l"), MouseMode::None);
        assert_eq!(protocol(b"\x1b[?9h\x1b[?1000l"), MouseMode::None);
        assert_eq!(
            protocol(b"\x1b[?1000h\x1b[?1003h\x1b[?1000l"),
            MouseMode::None
        );
        assert_eq!(protocol(b"\x1b[?1003h\x1b[?25;1002l"), MouseMode::None);
        // A reset that names none of the four leaves it, and so does a set
        // of another mode.
        assert_eq!(
            protocol(b"\x1b[?1002h\x1b[?1006l\x1b[?25l\x1b[?1004h"),
            MouseMode::ButtonMotion
        );
        // Set again after a reset, it is back.
        assert_eq!(
            protocol(b"\x1b[?1002h\x1b[?1000l\x1b[?1002h"),
            MouseMode::ButtonMotion
        );
        // RIS turns reporting off; DECSTR keeps it, as xterm does (#687).
        assert_eq!(protocol(b"\x1b[?1002h\x1bc"), MouseMode::None);
        assert_eq!(protocol(b"\x1b[?1002h\x1b[!p"), MouseMode::ButtonMotion);
        // An ANSI mode of the same number is not a tracking mode, and
        // neither is any other final byte.
        assert_eq!(protocol(b"\x1b[1000h"), MouseMode::None);
        assert_eq!(protocol(b"\x1b[?1002h\x1b[1000l"), MouseMode::ButtonMotion);
        assert_eq!(
            protocol(b"\x1b[?1002h\x1b[?1000$p"),
            MouseMode::ButtonMotion
        );

        // The set the application asked for is a different fact: a reset
        // takes out the modes it names and no others.
        let tracker = fed(b"\x1b[?1002h\x1b[?1003h\x1b[?1000l");
        assert_eq!(tracker.mouse_protocol(), MouseMode::None);
        assert_eq!(
            tracker.mouse_tracking().iter().collect::<Vec<_>>(),
            [MouseMode::ButtonMotion, MouseMode::AnyMotion]
        );
    }

    /// RIS returns focus reporting and synchronized output to power-on: left
    /// on, `focus_in` typed at a program that had reset, and an update begun
    /// before the reset stayed open for good (#623). `DECSTR` keeps focus
    /// reporting, as xterm's soft reset does (#687).
    #[test]
    fn a_hard_reset_ends_focus_reporting_and_an_open_synchronized_update() {
        let mut t = fed(b"\x1b[?1004h\x1b[?2026hhalf");
        assert!(
            t.focus_events() && t.in_sync_update(),
            "set before the reset"
        );
        let events: Vec<SeqEvent> = b"\x1bc".iter().map(|&b| t.step(b)).collect();
        assert_eq!(events.last(), Some(&SeqEvent::HardReset));
        assert!(
            !events.contains(&SeqEvent::SyncEnd),
            "the half-drawn update is dropped, not completed as a frame"
        );
        assert!(!t.focus_events(), "focus reporting is off after RIS");
        assert!(!t.in_sync_update(), "no update is open after RIS");
        // A fresh update after the reset counts only what it draws.
        t.feed(b"\x1b[?2026hab");
        assert_eq!(t.take_frame_printable(), 2);
    }

    /// The three spellings vt100 reads as "erase the saved lines" are all
    /// seen as that, so the rows termlens kept survive each one (#621):
    /// DECSED 3 and the sub-parameter form erased them unseen.
    #[test]
    fn every_spelling_of_erase_saved_lines_is_seen() {
        let erased = |bytes: &[u8]| {
            let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
            bytes
                .iter()
                .filter(|&&b| t.step(b) == SeqEvent::ScrollbackErased)
                .count()
        };
        for erase in [
            &b"\x1b[3J"[..],
            b"\x1b[?3J",
            b"\x1b[3:1J",
            b"\x1b[?3:1J",
            b"\x1b[3;1J",
        ] {
            assert_eq!(erased(erase), 1, "{}", printable(erase));
        }
        // Only the first value of the first parameter is read, as vt100 reads
        // it: a sub-parameter of 3 is not an erase, and nor is another ED.
        for other in [
            &b"\x1b[2J"[..],
            b"\x1b[?2J",
            b"\x1b[2:3J",
            b"\x1b[J",
            b"\x1b[>3J",
            b"\x1b[1;3J",
        ] {
            assert_eq!(erased(other), 0, "{}", printable(other));
        }
        // A sub-parameter still leaves every other sequence unrecognized.
        assert!(queries_of(b"\x1b[6:1n").is_empty());
        assert!(!fed(b"\x1b[?2026:1h").in_sync_update());
    }

    /// A C0 control inside an escape is executed and the escape goes on;
    /// DEL and bytes from 0x80 are ignored there. That is vte's rule, and
    /// vte is what feeds the grid: a tracker that dropped to ground missed
    /// the CSI the parser then ran (#627).
    #[test]
    fn a_control_inside_an_escape_is_executed_and_the_escape_goes_on() {
        let cpr = [Query::CursorPosition { private: false }];
        for stream in [
            &b"\x1b\x07[6n"[..],
            b"\x1b\r[6n",
            b"\x1b\x7f[6n",
            b"\x1b\xc3[6n",
            b"\x1b[\x076n",
            b"\x1b[6\rn",
            b"\x1b[6\x7fn",
        ] {
            assert_eq!(queries_of(stream), cpr, "{}", printable(stream));
        }
        // The bell rang, once each, wherever it sat.
        assert_eq!(fed(b"\x1b\x07[6n").bells(), 1);
        assert_eq!(fed(b"\x1b[\x076n").bells(), 1);
        assert_eq!(fed(b"\x1b(\x07").bells(), 1);
        // The control is not part of the sequence: an unanswered query is
        // named as the application would recognise it.
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        let named: Vec<SeqEvent> = b"\x1b[\r?u".iter().map(|&b| t.step(b)).collect();
        assert_eq!(
            named.last(),
            Some(&SeqEvent::Query(Query::Unanswerable("^[[?u".into())))
        );
        // A designation goes on through a control too.
        assert_eq!(drawn(b"\x1b(\r0q"), "\u{2500}");
        // The issue's reproduction: the CR runs inside the escape and the
        // first `q` ends it, so the rest are drawn, in line art (`x` is
        // the vertical bar there).
        assert_eq!(
            drawn(b"\x1b(0\x1b\rqqqq\r\nxx"),
            "\u{2500}\u{2500}\u{2500}\u{2502}\u{2502}"
        );
        // A shift inside an escape still shifts.
        assert_eq!(drawn(b"\x1b)0\x1b\x0e[mq"), "\u{2500}");
    }

    /// Entering the alternate screen is announced at its final byte, so
    /// the emulator can copy the main screen's history before the switch
    /// (#622), and so is leaving it, so the emulator can read the main
    /// screen back before anything scrolls it (#682). The same numbers as
    /// ANSI modes are not.
    #[test]
    fn switching_to_and_from_the_alternate_screen_fires_an_event() {
        let last = |bytes: &[u8]| {
            let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
            bytes.iter().map(|&b| t.step(b)).last().unwrap()
        };
        for (switch, leaving) in [
            (&b"\x1b[?47h"[..], false),
            (b"\x1b[?1047h", false),
            (b"\x1b[?1049h", false),
            (b"\x1b[?25;1049h", false),
            (b"\x1b[?47l", true),
            (b"\x1b[?1047l", true),
            (b"\x1b[?1049l", true),
            (b"\x1b[?1049;25l", true),
        ] {
            assert_eq!(
                last(switch),
                SeqEvent::AlternateScreen { leaving },
                "{switch:?}"
            );
        }
        for other in [
            &b"\x1b[1049l"[..],
            b"\x1b[1049h",
            b"\x1b[?104h",
            b"\x1b[?10490h",
        ] {
            assert_eq!(last(other), SeqEvent::None, "{other:?}");
        }
    }

    #[test]
    fn sync_2026_is_recognized_anywhere_in_a_multi_mode_list() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        t.feed(b"\x1b[?2026;25h");
        assert!(t.in_sync_update());
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        t.feed(b"\x1b[?25;2026h");
        assert!(t.in_sync_update());
    }

    #[test]
    fn lookalike_sequences_do_not_toggle_sync() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
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
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
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
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        let events: Vec<SeqEvent> = b"\x1b[?2026l".iter().map(|&b| t.step(b)).collect();
        assert!(!events.contains(&SeqEvent::SyncEnd));
        assert!(!t.in_sync_update());

        // Taken verbatim from a real crash handler.
        let reset = b"\x1b[?2026l\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?2004l\x1b[?1049l";
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        let events: Vec<SeqEvent> = reset.iter().map(|&b| t.step(b)).collect();
        assert!(!events.contains(&SeqEvent::SyncEnd));

        // And the End of a real frame still ends it, once only.
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
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
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        t.feed(b"\x1b[?2026h\x1b[31\x18"); // CAN aborts the SGR, not the frame
        assert!(t.in_sync_update());
        t.feed(b"\x1b[?2026l");
        assert!(!t.in_sync_update());
    }

    fn queries_of(bytes: &[u8]) -> Vec<Query> {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        bytes
            .iter()
            .filter_map(|&b| match t.step(b) {
                SeqEvent::Query(q) | SeqEvent::GraphicsQuery(_, _, q) => Some(q),
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
                number: None,
                placement: None,
                quiet: 0,
                load: KittyLoad::Loaded,
                shape: "^[_Ga=q;^[\\".into()
            }]
        );
    }

    /// `q=` is carried with the id: whether kitty replies depends on both
    /// (#639).
    #[test]
    fn a_kitty_probe_carries_its_quiet_level() {
        for (wire, id, quiet) in [
            (&b"\x1b_Gi=31,a=q,q=1;\x1b\\"[..], Some(31), 1),
            (b"\x1b_Ga=q,q=2,i=31;\x1b\\", Some(31), 2),
            (b"\x1b_Ga=q,I=7;\x1b\\", None, 0),
        ] {
            match &queries_of(wire)[..] {
                [Query::KittyGraphics {
                    id: got_id,
                    quiet: got_quiet,
                    ..
                }] => assert_eq!((*got_id, *got_quiet), (id, quiet)),
                other => panic!("{other:?}"),
            }
        }
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
        let q = queries_of(b"\x1b[>q"); // XTVERSION
        assert_eq!(q, vec![Query::Unanswerable("^[[>q".into())]);
        let q = queries_of(b"\x1b[>0q");
        assert_eq!(q, vec![Query::Unanswerable("^[[>0q".into())]);
        let q = queries_of(b"\x1b]12;?\x07"); // cursor color
        assert_eq!(q, vec![Query::Unanswerable("^[]12;?^G".into())]);
        // Any CSI …n is a DSR-family status request by definition.
        let q = queries_of(b"\x1b[6;1n");
        assert_eq!(q, vec![Query::Unanswerable("^[[6;1n".into())]);
    }

    /// `CSI > Ps n` is XTMODKEYS's "disable", a setting with no reply,
    /// and naming it in a timeout pointed the reader at a question nobody
    /// asked (#692).
    #[test]
    fn xtmodkeys_disable_is_not_a_question() {
        for wire in [&b"\x1b[>4n"[..], b"\x1b[>n", b"\x1b[>0n", b"\x1b[>1n"] {
            assert_eq!(queries_of(wire), Vec::<Query>::new(), "{}", printable(wire));
        }
        // The other prefixes are still the DSR family's.
        assert_eq!(
            queries_of(b"\x1b[?996n"),
            vec![Query::Unanswerable("^[[?996n".into())]
        );
    }

    /// xterm's window reports, every one documented in ctlseqs: the screen
    /// size in pixels (`15 t`) and the `; 2` forms of the position and the
    /// pixel size (`13 ; 2 t`, `14 ; 2 t`) are questions termlens does not
    /// answer, and were never named (#692).
    #[test]
    fn every_xterm_window_report_is_a_question() {
        for wire in [&b"\x1b[15t"[..], b"\x1b[14;2t", b"\x1b[13;2t"] {
            assert_eq!(
                queries_of(wire),
                vec![Query::Unanswerable(printable(wire))],
                "{}",
                printable(wire)
            );
        }
        // Not reports: a resize, the title stack, and a second parameter
        // xterm gives no meaning to.
        for wire in [&b"\x1b[8;24;80t"[..], b"\x1b[22;0t", b"\x1b[11;2t"] {
            assert_eq!(queries_of(wire), Vec::<Query>::new(), "{}", printable(wire));
        }
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
        // XTSMGRAPHICS, in every form: xterm replies to a set too (#641).
        for xtsm in [
            &b"\x1b[?1;1;0S"[..],
            b"\x1b[?2;1;0S",
            b"\x1b[?2;4;0S",
            b"\x1b[?1;3;256S",
        ] {
            assert_eq!(
                queries_of(xtsm),
                vec![Query::Unanswerable(printable(xtsm))],
                "{}",
                printable(xtsm)
            );
        }
        // Scroll up (`CSI n S`) is not a question.
        assert!(queries_of(b"\x1b[2S").is_empty());
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
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        assert_eq!(&*t.title(), "");
        t.feed(b"\x1b]2;hello world\x07");
        assert_eq!(&*t.title(), "hello world");
        t.feed("\x1b]0;second ✓\x1b\\".as_bytes());
        assert_eq!(&*t.title(), "second ✓");
    }

    #[test]
    fn titles_longer_than_the_diagnostic_capture_are_kept_whole() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        let title = "t".repeat(80); // seq_buf truncates at 24; titles must not
        t.feed(format!("\x1b]2;{title}\x07").as_bytes());
        assert_eq!(&*t.title(), title.as_str());
    }

    #[test]
    fn title_survives_chunked_delivery() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        t.feed(b"\x1b]2;split");
        t.feed(b" title\x07");
        assert_eq!(&*t.title(), "split title");
    }

    #[test]
    fn title_keeps_embedded_semicolons() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        t.feed(b"\x1b]0;a;b;c\x07");
        assert_eq!(&*t.title(), "a;b;c");
    }

    #[test]
    fn icon_only_and_aborted_titles_do_not_change_the_title() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
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
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        t.feed(&bytes[..1]);
        assert!(t.mid_sequence());
        t.feed(&bytes[1..2]);
        assert!(t.mid_sequence());
        t.feed(&bytes[2..]);
        assert!(!t.mid_sequence());
    }

    /// Every [`TabOp`] a stream can produce, so the recognition is pinned
    /// apart from the rewrite that acts on it.
    fn tab_ops(bytes: &[u8]) -> Vec<TabOp> {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, TEST_COLS);
        let mut out = Vec::new();
        for &byte in bytes {
            if let SeqEvent::Tabs(op) = t.step(byte) {
                out.push(op);
            }
        }
        out
    }

    #[test]
    fn the_five_tab_sequences_are_recognized() {
        assert_eq!(tab_ops(b"\x1bH"), vec![TabOp::Set]);
        assert_eq!(tab_ops(b"\x1b[g"), vec![TabOp::ClearAtCursor]);
        assert_eq!(tab_ops(b"\x1b[0g"), vec![TabOp::ClearAtCursor]);
        assert_eq!(tab_ops(b"\x1b[3g"), vec![TabOp::ClearAll]);
        assert_eq!(tab_ops(b"\t"), vec![TabOp::Forward(1)]);
        // An omitted or zero count is one, as it is for every other CSI
        // motion; anything else is taken as written.
        assert_eq!(tab_ops(b"\x1b[I"), vec![TabOp::Forward(1)]);
        assert_eq!(tab_ops(b"\x1b[0I"), vec![TabOp::Forward(1)]);
        assert_eq!(tab_ops(b"\x1b[3I"), vec![TabOp::Forward(3)]);
        assert_eq!(tab_ops(b"\x1b[Z"), vec![TabOp::Back(1)]);
        assert_eq!(tab_ops(b"\x1b[2Z"), vec![TabOp::Back(2)]);
    }

    #[test]
    fn sequences_that_only_look_like_tab_operations_are_left_alone() {
        // `TBC 1`, `2`, `4` and `5` clear *line* tab stops, which this crate
        // has no notion of. Recognized as not ours rather than guessed at.
        assert!(tab_ops(b"\x1b[1g").is_empty());
        assert!(tab_ops(b"\x1b[2g").is_empty());
        // A private prefix is a different sequence entirely.
        assert!(tab_ops(b"\x1b[?3g").is_empty());
        assert!(tab_ops(b"\x1b[?1I").is_empty());
        // `CSI H` is CUP, and only the bare `ESC H` is HTS.
        assert!(tab_ops(b"\x1b[H").is_empty());
        assert!(tab_ops(b"\x1b[1;1H").is_empty());
        // A tab inside a string sequence is payload, not a motion — the
        // same rule that keeps a BEL there from being a bell.
        assert!(tab_ops(b"\x1b]0;a\tb\x07").is_empty());
        assert!(tab_ops(b"\x1bPq\t\x1b\\").is_empty());
    }

    #[test]
    fn a_stop_set_at_the_cursor_is_where_the_next_tab_lands() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, 24);
        // The default eight, until the application says otherwise.
        assert_eq!(t.tab_op(TabOp::Forward(1), 1), Some(8));
        assert_eq!(t.tab_op(TabOp::Set, 3), None);
        assert_eq!(t.tab_op(TabOp::Forward(1), 1), Some(3));
        // Setting one adds to the set rather than replacing it.
        assert_eq!(t.tab_op(TabOp::Forward(1), 3), Some(8));
        // And clearing it takes only that one away.
        assert_eq!(t.tab_op(TabOp::ClearAtCursor, 3), None);
        assert_eq!(t.tab_op(TabOp::Forward(1), 1), Some(8));
    }

    #[test]
    fn motions_move_by_whole_stops_and_saturate_at_the_edges() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, 24);
        assert_eq!(t.tab_op(TabOp::Forward(2), 0), Some(16));
        // Past the last stop there is nowhere left to go but the last
        // column, and asking for more stops than exist does not overshoot.
        assert_eq!(t.tab_op(TabOp::Forward(9), 0), Some(23));
        assert_eq!(t.tab_op(TabOp::Forward(u16::MAX), 0), Some(23));
        // Back-tab goes to the nearest stop strictly left of the cursor —
        // so a cursor one past the stop it just wrote over returns to it.
        assert_eq!(t.tab_op(TabOp::Back(1), 17), Some(16));
        assert_eq!(t.tab_op(TabOp::Back(1), 16), Some(8));
        assert_eq!(t.tab_op(TabOp::Back(2), 17), Some(8));
        assert_eq!(t.tab_op(TabOp::Back(u16::MAX), 17), Some(0));
    }

    #[test]
    fn clearing_every_stop_leaves_the_two_edges() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, 24);
        assert_eq!(t.tab_op(TabOp::ClearAll, 0), None);
        assert_eq!(t.tab_op(TabOp::Forward(1), 0), Some(23));
        assert_eq!(t.tab_op(TabOp::Back(1), 20), Some(0));
        // A stop set afterwards is the only one there is.
        assert_eq!(t.tab_op(TabOp::Set, 5), None);
        assert_eq!(t.tab_op(TabOp::Forward(1), 0), Some(5));
    }

    /// `RIS` restores the every-eighth set; `DECSTR` keeps the stops the
    /// application set, as xterm's soft reset does (#687).
    #[test]
    fn a_hard_reset_restores_the_default_every_eighth_set_and_a_soft_one_does_not() {
        for (reset, after) in [(&b"\x1bc"[..], 8), (b"\x1b[!p", 3)] {
            let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, 24);
            assert_eq!(t.tab_op(TabOp::ClearAll, 0), None);
            assert_eq!(t.tab_op(TabOp::Set, 3), None);
            assert_eq!(t.tab_op(TabOp::Forward(1), 0), Some(3));
            t.feed(reset);
            assert_eq!(
                t.tab_op(TabOp::Forward(1), 0),
                Some(after),
                "after {}",
                printable(reset)
            );
        }
    }

    #[test]
    fn a_resize_extends_the_set_without_disturbing_the_stops_it_had() {
        let mut t = SeqTracker::new(crate::graphics::DEFAULT_CAPTURE, TEST_ROWS, 24);
        assert_eq!(t.tab_op(TabOp::Set, 3), None);
        assert_eq!(t.tab_op(TabOp::ClearAtCursor, 16), None);
        t.set_size(TEST_ROWS, 40);
        // The documented rule: new columns get the every-eighth pattern,
        // and both edits inside the old width survive.
        assert_eq!(t.tab_op(TabOp::Forward(1), 0), Some(3));
        assert_eq!(t.tab_op(TabOp::Forward(1), 8), Some(24), "16 stays cleared");
        assert_eq!(t.tab_op(TabOp::Forward(1), 24), Some(32), "new territory");
        // Narrowing drops the columns that no longer exist, and motion
        // clamps to the width that does.
        t.set_size(TEST_ROWS, 10);
        assert_eq!(t.tab_op(TabOp::Forward(1), 8), Some(9));
    }
}
