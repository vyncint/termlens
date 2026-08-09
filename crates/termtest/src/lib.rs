//! Headless PTY test harness for CLI/TUI applications.
//!
//! `termtest` spawns your program in a real pseudo-terminal, feeds its output
//! through a VT emulator into an in-memory screen grid, and lets your tests
//! assert on — and snapshot — the *rendered screen* instead of raw bytes.
//!
//! The public API lands module by module; see the repository README for the
//! full v0.1 surface.

#![warn(missing_docs)]
