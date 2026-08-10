# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Until 1.0, minor versions (0.x) may contain breaking changes; they are always
listed under a **Changed** or **Removed** heading.

## [Unreleased]

### Added

- `Terminal::wait_frame(pred)`: evaluates the predicate only on **complete
  frames** for applications that bracket repaints in DEC 2026 synchronized
  updates — a torn, half-painted repaint is never observable. Apps that
  don't emit synchronized output get a timeout error that says so and
  points at `wait_until`.

### Changed

- `wait_idle` no longer resolves while a synchronized update is open: a
  begun-but-unfinished repaint is mid-update by definition.

## [0.1.1] - 2026-08-09

### Changed

- Publishing runs exclusively through crates.io Trusted Publishing
  (short-lived OIDC tokens), bound to a tag-restricted GitHub
  environment; the repository stores no secrets at all.

### Fixed

- README install instructions now include `insta`, which the snapshot
  examples use — copying the example verbatim previously failed on an
  unresolved import.
- README comparison table links `teatest` like every other row (first
  external contribution).
- CONTRIBUTING documents the fork-PR experience: the first-run approval
  gate and where commit-policy failures are explained when the courtesy
  comment cannot post.

## [0.1.0] - 2026-08-09

### Changed

- Renamed the project from `termtest` to `termlens` before first publish:
  an active Go library of the same name occupies the identical niche
  (github.com/ActiveState/termtest), and the new name says what the crate
  actually does — assert on what is *seen* through the terminal.

### Added

- Initial implementation of the `termlens` crate: spawn any terminal program
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
- `Screen::cols()` and `Screen::rows()`: named size accessors, so nobody
  has to remember that `size()` is `(cols, rows)` while cells are
  addressed `(row, col)`.

### Performance

- Snapshots are cached per state generation: `Terminal::screen()` on a
  quiescent terminal now costs an `Arc` clone (~ns) instead of a full grid
  conversion (~16µs at 80×24), and `wait_until` skips re-evaluating its
  predicate on wakes where nothing changed. Streaming throughput is
  unaffected (verified with interleaved A/B benchmarks).

### Fixed

- PTY lifecycle edges (open+spawn / kill+reap+close) are serialized behind
  a process-wide lock: on macOS, a concurrent teardown's `revoke()` could
  hit another thread's freshly recycled pty device and kill its child at
  birth. Found by the stress workflow; see `docs/DESIGN.md` §2.
- The PTY reader thread now attaches **before** the child is spawned, so a
  program that writes and exits within its first millisecond meets a drain
  that is already running. This narrows (but cannot fully close — see the
  instant-exit caveat in `docs/DESIGN.md`) an output-loss race in the OS
  pty teardown, found by the stress workflow at roughly 1 in 80
  instant-exit spawns on macOS.
