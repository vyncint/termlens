//! Emulator abstraction.
//!
//! The public types ([`Screen`](crate::Screen) et al.) are termlens's own;
//! the VT emulator sits behind this small internal trait so the backend can
//! be swapped (e.g. for `wezterm-term` or `alacritty_terminal`) without any
//! public API change. v0.1 ships one backend: the `vt100` crate.

mod seq;
mod vt100;

pub(crate) use self::vt100::Vt100Emulator;

use crate::Screen;

/// A VT emulator: consumes raw PTY bytes, maintains a screen grid.
pub(crate) trait Emulator: Send {
    /// Feed raw bytes from the PTY into the emulator.
    fn process(&mut self, bytes: &[u8]);

    /// Snapshot the current screen as an owned value.
    fn snapshot(&self) -> Screen;

    /// True while the byte stream ends inside an unfinished escape sequence
    /// or an incomplete UTF-8 character — used by `wait_idle` to avoid
    /// declaring idleness mid-update.
    fn mid_sequence(&self) -> bool;

    /// Resize the emulated grid to `rows` × `cols`.
    fn set_size(&mut self, rows: u16, cols: u16);
}
