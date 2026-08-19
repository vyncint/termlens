//! Emulator abstraction.
//!
//! The public types ([`Screen`](crate::Screen) et al.) are termlens's own;
//! the VT emulator sits behind this small internal trait so the backend can
//! be swapped (e.g. for `wezterm-term` or `alacritty_terminal`) without any
//! public API change. One backend ships today: the `vt100` crate, plus the
//! attribute shadow in `shadow.rs` that recovers the three SGR attributes
//! vt100 drops.

mod seq;
mod shadow;
mod vt100;

pub(crate) use self::seq::Query;
pub(crate) use self::vt100::Vt100Emulator;

use crate::screen::MouseMode;
use crate::Screen;

/// What one completed repaint cost, measured between the application's own
/// markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameSpan {
    pub(crate) duration: std::time::Duration,
    pub(crate) printable: u32,
}

/// Why the emulator stopped consuming mid-segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    /// A synchronized update ended (DEC 2026 ESU): the screen at this
    /// instant is a complete frame, and the span is what it cost.
    FrameComplete(FrameSpan),
    /// The application asked the terminal a question; the screen state
    /// (cursor, size) is exactly as of the query.
    Query(Query),
}

/// Outcome of feeding one segment of PTY bytes into the emulator.
#[derive(Debug, Clone)]
pub(crate) struct Processed {
    /// How many input bytes were consumed (> 0 for non-empty input).
    pub(crate) consumed: usize,
    /// Set when consumption stopped before the end of the input.
    pub(crate) stop: Option<Stop>,
}

/// Input-affecting terminal modes the application has set — the emulator
/// knows them, and the input path uses them so sent bytes match what the
/// application configured the "terminal" to send.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InputModes {
    pub(crate) mouse: MouseMode,
    pub(crate) mouse_encoding: MouseEncoding,
    pub(crate) bracketed_paste: bool,
    pub(crate) application_cursor: bool,
    /// Focus reporting (mode 1004). Lives here rather than behind a new
    /// trait method: it is an input-affecting mode like the others, and the
    /// `Emulator` surface is deliberately the narrowest the terminal loop
    /// needs.
    pub(crate) focus_events: bool,
}

/// How the application asked for mouse coordinates to be encoded. The
/// three schemes agree below column/row 95 and diverge past it, so
/// sending the wrong one fails only at a position boundary — which is a
/// miserable thing to debug from a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseEncoding {
    /// The original byte-valued form: `ESC [ M Cb Cx Cy`, unusable past
    /// coordinate 222.
    Legacy,
    /// SGR (mode 1006): `ESC [ < b ; col ; row M`, unbounded.
    Sgr,
    /// UTF-8 (mode 1005): like the legacy form, but coordinates above 95
    /// are encoded as two-byte UTF-8 rather than a bare byte.
    Utf8,
}

/// What a `DECRQM` request can be told about a private mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeState {
    /// The mode is implemented and currently set.
    Set,
    /// The mode is implemented and currently reset.
    Reset,
    /// Not implemented, or implemented but not tracked precisely enough
    /// to answer honestly. Reported as "not recognized", never guessed:
    /// an application told a mode is reset when it may be set is worse
    /// off than one told nothing.
    NotRecognized,
}

impl ModeState {
    /// The `Ps` value of the `DECRPM` reply.
    pub(crate) fn report_value(self) -> u32 {
        match self {
            ModeState::Set => 1,
            ModeState::Reset => 2,
            ModeState::NotRecognized => 0,
        }
    }
}

/// A VT emulator: consumes raw PTY bytes, maintains a screen grid.
pub(crate) trait Emulator: Send {
    /// Feed raw bytes from the PTY into the emulator. Stops early — with
    /// `consumed < bytes.len()` — when a synchronized update ends or the
    /// application issues a query, so the caller can act on the exact
    /// screen state at that instant before feeding the rest.
    fn process(&mut self, bytes: &[u8]) -> Processed;

    /// Snapshot the current screen as an owned value.
    fn snapshot(&self) -> Screen;

    /// True while the byte stream ends inside an unfinished escape sequence
    /// or an incomplete UTF-8 character — used by `wait_idle` to avoid
    /// declaring idleness mid-update.
    fn mid_sequence(&self) -> bool;

    /// True while the stream is inside a DEC 2026 synchronized update —
    /// the app has begun a repaint and not finished it.
    fn in_sync_update(&self) -> bool;

    /// The input-affecting modes the application has currently set.
    fn input_modes(&self) -> InputModes;

    /// Whether DEC private mode `mode` is set, for answering `DECRQM`.
    fn mode_state(&self, mode: u32) -> ModeState;

    /// Resize the emulated grid to `rows` × `cols`.
    fn set_size(&mut self, rows: u16, cols: u16);
}
