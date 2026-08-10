//! Emulator abstraction.
//!
//! The public types ([`Screen`](crate::Screen) et al.) are termlens's own;
//! the VT emulator sits behind this small internal trait so the backend can
//! be swapped (e.g. for `wezterm-term` or `alacritty_terminal`) without any
//! public API change. v0.1 ships one backend: the `vt100` crate.

mod seq;
mod vt100;

pub(crate) use self::seq::Query;
pub(crate) use self::vt100::Vt100Emulator;

use crate::Screen;

/// Why the emulator stopped consuming mid-segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    /// A synchronized update ended (DEC 2026 ESU): the screen at this
    /// instant is a complete frame.
    FrameComplete,
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

/// Which mouse events the application asked the terminal to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseMode {
    /// No mouse tracking enabled.
    None,
    /// X10: presses only (mode 9).
    Press,
    /// Presses and releases (mode 1000), possibly with motion (1002/1003).
    PressRelease,
}

/// Input-affecting terminal modes the application has set — the emulator
/// knows them, and the input path uses them so sent bytes match what the
/// application configured the "terminal" to send.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InputModes {
    pub(crate) mouse: MouseMode,
    /// SGR (1006) mouse encoding active.
    pub(crate) sgr_mouse: bool,
    pub(crate) bracketed_paste: bool,
    pub(crate) application_cursor: bool,
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

    /// Resize the emulated grid to `rows` × `cols`.
    fn set_size(&mut self, rows: u16, cols: u16);
}
