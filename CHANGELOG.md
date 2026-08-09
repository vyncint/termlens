# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Until 1.0, minor versions (0.x) may contain breaking changes; they are always
listed under a **Changed** or **Removed** heading.

## [Unreleased]

### Added

- Initial implementation of the `termtest` crate: spawn any terminal program
  in a real PTY, drive it with typed key input, and assert or snapshot on the
  emulated screen grid.
- `Terminal` / `TerminalBuilder` with strict environment control, size
  configuration, and a default deadline applied to every `wait_*` call.
- `Screen` value type with cell/row/cursor accessors and a deterministic
  text `Display` format designed for `insta` snapshots.
- `Key` enum covering chars, control chords, alt chords, and xterm special
  keys (arrows, Home/End, PageUp/Down, F1–F12, Tab/BackTab, Delete, …).
- `wait_until`, `wait_idle`, `wait_exit`, and `resize` (TIOCSWINSZ +
  SIGWINCH) semantics; timeout errors embed the current screen dump.
- `insta` cargo feature (default): re-exports `insta` and provides the
  `assert_screen_snapshot!` helper macro.
- Deterministic PTY fixture apps (`hello-tui`, `form-echo`, `resize-echo`,
  `unicode-torture`) used by the integration suite.
- `inspect` example: run any command and print its rendered screen.
- `ExitStatus::signal()`: the terminating signal's name when the child died
  from a signal instead of exiting; `Display` now says
  `killed by signal: … (code …)` so harness-level kills are never mistaken
  for application exit codes. (`ExitStatus` is `Clone` but no longer `Copy`.)
- `Screen`'s `Debug` is now the compact header+text rendering: a failing
  `Result` test prints a readable screen instead of a one-line cell dump.

### Fixed

- The PTY reader thread now attaches **before** the child is spawned, so a
  program that writes and exits within its first millisecond meets a drain
  that is already running. This narrows (but cannot fully close — see the
  instant-exit caveat in `docs/DESIGN.md`) an output-loss race in the OS
  pty teardown, found by the stress workflow at roughly 1 in 80
  instant-exit spawns on macOS.
