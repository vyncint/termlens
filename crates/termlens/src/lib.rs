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
//! With the default `insta` feature, snapshot-test whole screens — after
//! waiting for what the application paints and for the picture to hold
//! still, which [`snapshot_after`](Terminal::snapshot_after) does in one
//! call:
//!
//! ```no_run
//! # fn main() -> termlens::Result<()> {
//! # let mut t = termlens::Terminal::builder().spawn("true")?;
//! let screen = t.snapshot_after(|s| s.contains("Ready"))?;
//! #[cfg(feature = "insta")]
//! insta::assert_snapshot!(screen); // or termlens::assert_screen_snapshot!(screen)
//! # Ok(())
//! # }
//! ```
//!
//! Testing a binary of your own package? [`bin!`] spawns
//! `CARGO_BIN_EXE_<name>` under the harness defaults — a fixed grid, a
//! cleared environment, a deadline — with builder calls after the name to
//! override any of them.

#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

mod emu;
mod error;
mod graphics;
mod keys;
mod screen;
mod terminal;
mod utf8;
mod wait;

pub use error::{Error, Result};
#[cfg(feature = "decode")]
#[cfg_attr(docsrs, doc(cfg(feature = "decode")))]
pub use graphics::{Bitmap, DecodeError};
pub use graphics::{
    GraphicsAction, GraphicsFormat, GraphicsPayload, GraphicsProtocol, GraphicsSeen,
};
pub use keys::{Chord, Input, Key};
pub use screen::{
    Cell, Clipboard, Color, CursorShape, Link, MouseMode, MouseModes, Screen, ScreenDiff, Style,
};
#[cfg(unix)]
pub use terminal::Signal;
pub use terminal::{
    ExitStatus, FrameTiming, Graphics, MouseButton, MouseChord, Recorder, Recording, Scroll,
    ScrollChord, Terminal, TerminalBuilder,
};

/// What [`assert_screen_snapshot!`] snapshots: a [`Terminal`], settled, or a
/// [`Screen`] as it is. Implemented for `&mut Terminal` and `&Screen`, so the
/// macro's method-call syntax borrows a `Terminal` mutably and a `Screen`
/// immutably — whichever it was handed.
#[doc(hidden)]
pub trait SnapshotSource {
    /// The screen to snapshot. `after` is the predicate to wait for first;
    /// a [`Screen`] is already one instant, so it refuses one.
    fn screen_for_snapshot(self, after: Option<&mut dyn FnMut(&Screen) -> bool>) -> Result<Screen>;
}

impl SnapshotSource for &mut Terminal {
    fn screen_for_snapshot(self, after: Option<&mut dyn FnMut(&Screen) -> bool>) -> Result<Screen> {
        match after {
            Some(predicate) => self.snapshot_after(|screen| predicate(screen)),
            None => self.wait_stable(std::time::Duration::from_millis(100)),
        }
    }
}

impl SnapshotSource for &Screen {
    fn screen_for_snapshot(self, after: Option<&mut dyn FnMut(&Screen) -> bool>) -> Result<Screen> {
        if after.is_some() {
            return Err(Error::Input(
                "assert_screen_snapshot!(screen, after = …): a Screen is already one instant, \
                 so there is nothing to wait for — pass the Terminal instead"
                    .to_owned(),
            ));
        }
        Ok(self.clone())
    }
}

/// Re-export of [`insta`](https://insta.rs) (feature `insta`, on by
/// default), so [`assert_screen_snapshot!`] always agrees with the `insta`
/// version doing the snapshotting.
#[cfg(feature = "insta")]
#[cfg_attr(docsrs, doc(cfg(feature = "insta")))]
pub use insta;

/// Snapshot a terminal's screen the way a TUI snapshot has to be taken:
/// **settled**, **with its styles**, through [`insta::assert_snapshot!`].
///
/// ```no_run
/// # fn main() -> termlens::Result<()> {
/// # let mut t = termlens::Terminal::builder().spawn("true")?;
/// termlens::assert_screen_snapshot!(t);                                  // settle 100ms, styles on
/// termlens::assert_screen_snapshot!(t, styles = false);                  // text only
/// termlens::assert_screen_snapshot!(t, after = |s| s.contains("Ready")); // wait for it, then settle
/// termlens::assert_screen_snapshot!(t.screen());                         // a Screen you already hold
/// termlens::assert_screen_snapshot!(t, @"");                             // inline, filled by `cargo insta review`
/// # Ok(())
/// # }
/// ```
///
/// # The three decisions it makes
///
/// A snapshot of a TUI needs three decisions every time, and forgetting any
/// one produces a test that passes for the wrong reason:
///
/// 1. **Wait for the picture to settle.** `wait_until(pred)` guarantees the
///    bytes that made `pred` true were processed — and nothing more. A
///    repaint has no end marker, so the predicate can fire on a half-painted
///    screen, including half a row. Given a [`Terminal`], this macro takes
///    the screen after it has held still for 100 ms
///    ([`wait_stable`](Terminal::wait_stable)); with `after = pred` it waits
///    for the predicate first and then for the stillness
///    ([`snapshot_after`](Terminal::snapshot_after)). Name the *last* thing
///    the application paints, and rule 2 of `docs/DESIGN.md` §2 is met.
/// 2. **Snapshot the styles, not only the text.** A TUI regression is as often
///    a colour as a character — a highlight on the wrong row, a masked field
///    printed in clear — and the text rendering cannot see either. Styles are
///    on by default; `styles = false` is the text-only snapshot.
/// 3. **Snapshot one instant.** Every accessor of the [`Screen`] the macro
///    records reads the same snapshot, so what insta shows is one consistent
///    picture, never two waits' worth.
///
/// Given a [`Screen`] instead of a terminal, the macro records it as it is
/// (`after =` is refused: an instant has nothing to wait for). Failures come
/// from insta unchanged; review them with `cargo insta review`. The macro
/// uses `?`, so the test returns [`Result`] — which every test should, since
/// the `Display` of every error carries the screen.
///
/// `insta::assert_snapshot!(t.screen())` remains the low-level spelling for
/// a screen already waited for by hand.
#[cfg(feature = "insta")]
#[cfg_attr(docsrs, doc(cfg(feature = "insta")))]
#[macro_export]
macro_rules! assert_screen_snapshot {
    ($source:expr $(,)?) => {
        $crate::assert_screen_snapshot!($source, styles = true)
    };
    ($source:expr, @$inline:literal $(,)?) => {{
        use $crate::SnapshotSource as _;
        let __screen = ($source).screen_for_snapshot(::core::option::Option::None)?;
        $crate::insta::assert_snapshot!(__screen.with_styles(), @$inline)
    }};
    ($source:expr, styles = $styles:expr $(,)?) => {{
        use $crate::SnapshotSource as _;
        let __screen = ($source).screen_for_snapshot(::core::option::Option::None)?;
        if $styles {
            $crate::insta::assert_snapshot!(__screen.with_styles());
        } else {
            $crate::insta::assert_snapshot!(__screen);
        }
    }};
    ($source:expr, after = $after:expr $(,)?) => {
        $crate::assert_screen_snapshot!($source, after = $after, styles = true)
    };
    ($source:expr, after = $after:expr, styles = $styles:expr $(,)?) => {{
        use $crate::SnapshotSource as _;
        let __after: &mut dyn ::core::ops::FnMut(&$crate::Screen) -> bool = &mut $after;
        let __screen = ($source).screen_for_snapshot(::core::option::Option::Some(__after))?;
        if $styles {
            $crate::insta::assert_snapshot!(__screen.with_styles());
        } else {
            $crate::insta::assert_snapshot!(__screen);
        }
    }};
}

/// Spawn one of this package's binaries under the harness defaults.
///
/// `termlens::bin!("myapp")` is the chain every integration test of a
/// binary starts from:
///
/// ```ignore
/// Terminal::builder()
///     .size(80, 24)                       // a fixed grid, so snapshots are stable
///     .env_clear()                        // nothing on the host leaks into the app
///     .timeout(Duration::from_secs(5))    // a hang is a readable failure, not a stuck job
///     .spawn(env!("CARGO_BIN_EXE_myapp"))
/// ```
///
/// Any builder method can follow the name as a call, and later calls
/// override the defaults:
///
/// ```ignore
/// let mut t = termlens::bin!("myapp")?;
/// let mut t = termlens::bin!("myapp", size(120, 40), env("NO_COLOR", "1"))?;
/// let mut t = termlens::bin!("myapp", timeout(Duration::from_secs(30)), args(["--fast"]))?;
/// ```
///
/// `CARGO_BIN_EXE_<name>` is set by Cargo for the integration tests of the
/// package that owns the binary, so this works from that package's `tests/`
/// and a misspelled name is a compile error naming the variable rather than
/// a spawn failure at run time. The examples above are not compiled as
/// doctests for the same reason: this crate has no binary called `myapp`.
/// To spawn a program that is not one of your own binaries, or with
/// different defaults, use [`Terminal::builder`] directly — the macro adds
/// nothing else.
#[macro_export]
macro_rules! bin {
    ($name:literal $(, $method:ident $args:tt)* $(,)?) => {
        $crate::Terminal::builder()
            .size(80, 24)
            .env_clear()
            .timeout(::std::time::Duration::from_secs(5))
            $(.$method $args)*
            .spawn(::std::env!(::std::concat!("CARGO_BIN_EXE_", $name)))
    };
}
