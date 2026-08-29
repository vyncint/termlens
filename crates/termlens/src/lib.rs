//! Headless PTY test harness for CLI/TUI applications.
//!
//! `termlens` spawns your program in a **real pseudo-terminal**, feeds its
//! output through a **VT emulator** into an in-memory **screen grid**, and
//! lets tests **assert and snapshot on the rendered screen** instead of raw
//! bytes — Playwright for the terminal.
//!
//! - It is *not* an expect-style stream matcher (see `rexpect`/`expectrl`).
//! - It is *not* an SVG transcript generator for docs (see `term-transcript`).
//! - It *is*: real PTY + emulated screen + snapshot assertions.
//!
//! # Example
//!
//! ```
//! use std::time::Duration;
//! use termlens::{Key, Terminal};
//!
//! # fn main() -> termlens::Result<()> {
//! let mut t = Terminal::builder()
//!     .size(80, 24)
//!     .env("TERM", "xterm-256color") // the default; shown for completeness
//!     .timeout(Duration::from_secs(10))
//!     .args(["-c", r#"read line; echo "got: $line"; read quit"#])
//!     .spawn("sh")?;
//!
//! t.send_str("hello")?;
//! t.send(Key::Enter)?;
//! t.wait_until(|screen| screen.contains("got: hello"))?;
//!
//! t.send(Key::Enter)?; // release `read quit`; the script finishes
//! let status = t.wait_exit()?;
//! assert!(status.success());
//! # Ok(())
//! # }
//! ```
//!
//! Every `wait_*` call runs under a deadline — the builder's
//! [`timeout`](TerminalBuilder::timeout) (default 5s) or a per-call one
//! ([`wait_until_for`](Terminal::wait_until_for) and friends) — and a
//! timeout error [embeds the screen](Error::Timeout) so a CI log alone
//! shows what the application was displaying. A background reader thread
//! drains the PTY continuously — no output is lost between waits — and
//! answers the queries a real terminal answers, so capability-probing apps
//! run instead of hanging.
//!
//! Where the application brackets its repaints in DEC 2026 synchronized
//! updates, [`wait_frame`](Terminal::wait_frame) evaluates predicates only
//! on **complete frames** and returns the one it matched — never a torn
//! repaint, and each call observes a frame no earlier call did. Content
//! that scrolls off the top is retained as well, so
//! [`full_text`](Screen::full_text) answers "this reached the terminal"
//! without the test having to know which region currently holds it.
//!
//! Input is mode-aware: [mouse clicks](Terminal::click),
//! [pastes](Terminal::paste), modifier [chords](Chord), and cursor keys
//! are encoded exactly as the application configured its terminal — and a
//! [drag](Terminal::drag) reports one motion per cell crossed, so an
//! application that acts along the path sees the path. [Focus
//! events](Terminal::focus_out) go the other way, reaching an application
//! that enabled mode 1004 so the unfocused branch of a UI can be driven at
//! all. The terminal's out-of-band state — the window title, the
//! alternate-screen flag, the input modes, the last `OSC 52`
//! [clipboard](Screen::clipboard) write, the
//! [cursor shape](Screen::cursor_shape) the application asked for, and the
//! `OSC 8` [hyperlinks](Screen::links) it emitted — is readable from every
//! [`Screen`] as plain accessors. Both of those last two leave the grid
//! identical: a bar cursor and a block cursor draw the same cells, and a
//! hyperlink's label renders as ordinary text with its URL nowhere on the
//! screen, so a test asserting a link used to pass against an application
//! that emitted none.
//!
//! Behaviour that leaves the screen **identical** is assertable too, which
//! no content predicate can manage: [`repaints`](Screen::repaints) counts
//! completed frames (so "one input became four repaints" is catchable),
//! [`bells`](Screen::bells) counts `BEL`, and
//! [`graphics`](Screen::graphics) counts the inline images an application
//! transmitted — often to assert that it transmitted *none*.
//! [`frame_timings`](Terminal::frame_timings) adds what each repaint cost,
//! so a suite can hold a performance line as well as a correctness one.
//!
//! Needles are matched by what the terminal draws rather than by how it is
//! spelled: [`contains`](Screen::contains) and [`find`](Screen::find) fold
//! both sides to NFC, so a needle typed in an editor finds text an
//! application normalized the other way. The grid keeps exactly the
//! codepoints the application sent.
//!
//! With the default `insta` feature, snapshot-test whole screens:
//!
//! ```no_run
//! # fn main() -> termlens::Result<()> {
//! # let mut t = termlens::Terminal::builder().spawn("true")?;
//! #[cfg(feature = "insta")]
//! {
//!     insta::assert_snapshot!(t.screen());        // plain insta…
//!     termlens::assert_screen_snapshot!(t.screen()); // …or the bundled macro
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod emu;
mod error;
mod graphics;
mod keys;
mod screen;
mod terminal;
mod wait;

pub use error::{Error, Result};
#[cfg(feature = "decode")]
pub use graphics::{Bitmap, DecodeError};
pub use graphics::{
    GraphicsAction, GraphicsFormat, GraphicsPayload, GraphicsProtocol, GraphicsSeen,
};
pub use keys::{Chord, Input, Key};
pub use screen::{Cell, Clipboard, Color, CursorShape, Link, MouseMode, Screen, Style};
#[cfg(unix)]
pub use terminal::Signal;
pub use terminal::{
    ExitStatus, FrameTiming, Graphics, MouseButton, MouseChord, Scroll, ScrollChord, Terminal,
    TerminalBuilder,
};

/// Re-export of [`insta`](https://insta.rs) (feature `insta`, on by
/// default), so [`assert_screen_snapshot!`] always agrees with the `insta`
/// version doing the snapshotting.
#[cfg(feature = "insta")]
pub use insta;

/// Snapshot-assert anything that displays like a [`Screen`].
///
/// Sugar for [`insta::assert_snapshot!`] through the re-exported `insta`;
/// accepts the same optional inline-snapshot form:
///
/// ```no_run
/// # fn main() -> termlens::Result<()> {
/// # let t = termlens::Terminal::builder().spawn("true")?;
/// #[cfg(feature = "insta")]
/// {
///     termlens::assert_screen_snapshot!(t.screen());
/// }
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "insta")]
#[macro_export]
macro_rules! assert_screen_snapshot {
    ($screen:expr) => {
        $crate::insta::assert_snapshot!($screen)
    };
    ($screen:expr, @$inline:literal) => {
        $crate::insta::assert_snapshot!($screen, @$inline)
    };
}
