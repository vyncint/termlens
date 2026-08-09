//! Headless PTY test harness for CLI/TUI applications.
//!
//! `termtest` spawns your program in a **real pseudo-terminal**, feeds its
//! output through a **VT emulator** into an in-memory **screen grid**, and
//! lets tests **assert and snapshot on the rendered screen** instead of raw
//! bytes — Playwright for the terminal.
//!
//! The full API lands module by module; see the repository README.

#![warn(missing_docs)]

mod error;
mod keys;
mod screen;

pub use error::{Error, Result};
pub use keys::Key;
pub use screen::{Cell, Color, Screen, Style};
