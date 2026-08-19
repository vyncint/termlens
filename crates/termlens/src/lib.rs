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
//! are encoded exactly as the application configured its terminal. And
//! the terminal's out-of-band state — the window title, the
//! alternate-screen flag, the input modes, the last `OSC 52`
//! [clipboard](Screen::clipboard) write — is readable from every
//! [`Screen`] as plain accessors.
//!
//! With the default `insta` feature, snapshot-test whole screens:
//!
//! ```no_run
//! # fn main() -> termlens::Result<()> {
//! # let mut t = termlens::Terminal::builder().spawn("true")?;
//! insta::assert_snapshot!(t.screen());        // plain insta…
//! termlens::assert_screen_snapshot!(t.screen()); // …or the bundled macro
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod emu;
mod error;
mod keys;
mod screen;
mod terminal;
mod wait;

pub use error::{Error, Result};
pub use keys::{Chord, Input, Key};
pub use screen::{Cell, Clipboard, Color, GraphicsSeen, MouseMode, Screen, Style};
#[cfg(unix)]
pub use terminal::Signal;
pub use terminal::{
    ExitStatus, Graphics, MouseButton, MouseChord, Scroll, Terminal, TerminalBuilder,
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
/// termlens::assert_screen_snapshot!(t.screen());
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
