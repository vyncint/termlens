# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Until 1.0, minor versions (0.x) may contain breaking changes; they are always
listed under a **Changed** or **Removed** heading.

## [Unreleased]

### Added

- Three `Screen` query helpers, each earned by a documented pain in the
  first real user's test suite: `rect_text(cols, rows)` — the text
  inside a rectangle (any range expression, clamped to the screen), for
  asserting on one pane of a split layout; `find_by(|cell| …)` — the
  first cell matching a predicate, for "where did the highlight go";
  and `find` now locates **multi-row needles** (`find("one\ntwo")`)
  with exactly the matching semantics `contains` always had for them.
- Out-of-band terminal state is readable from every `Screen` snapshot:
  `title()` (tracked from `OSC 0`/`OSC 2` by termlens itself — the
  emulator backend doesn't need to support it), `alternate_screen()`,
  `bracketed_paste()`, `application_cursor()`, and `mouse_mode()` (the
  new public `MouseMode` enum, reporting the exact tracking mode the
  application enabled). State that previously could only be inferred
  from grid contents is now a plain assertion:
  `wait_until(|s| s.alternate_screen())`. None of it appears in the
  snapshot text format — existing snapshot files stay valid.
- Cursor keys are **mode-aware**: while the application has DECCKM
  (application cursor mode) set, `send(Key::Up)` emits the `ESC O A`
  form a real terminal would — the emulator knows the mode. `Key::encode`
  still documents the default-mode bytes, and the `Esc`-then-key wire
  ambiguity (identical to an Alt chord) is now documented on `Key::Esc`
  with the working idiom.
- `Terminal::paste(text)`: pastes the way a terminal pastes — wrapped in
  bracketed-paste markers when the application enabled mode 2004 (one
  `Paste` event, not a burst of key presses), plain bytes when it
  didn't.
- Modifier chords over special keys: `Key::Right.ctrl()`,
  `Key::Up.shift()`, `Key::F(5).ctrl().shift()` — the xterm
  CSI-modifier encodings, chainable, accepted by the same
  `Terminal::send`. Character chords stay `Key::Ctrl(c)` / `Key::Alt(c)`
  (the builder methods say so loudly if you mix them up).
- `Terminal::click(col, row)` and `Terminal::scroll(col, row, Scroll)`:
  typed mouse input, encoded exactly as the tracking mode and encoding
  **the application enabled** (SGR 1006 or the legacy byte form), with a
  press-only form for X10 mode. Clicking while the app never enabled
  mouse tracking is a typed `Error::Input` instead of bytes the app
  would misparse.
- `Screen::with_styles()`: the plain snapshot rendering followed by a
  compact `styles:` block (run-length spans per row, format specified in
  `docs/DESIGN.md` §3) — a highlight moving to another row or a color
  changing is now a visible snapshot diff. Plain snapshots stay
  text-only; this is the opt-in.
- termlens now **answers terminal queries** (on by default): DSR cursor
  position — exact as of the query byte — operating status, DA1/DA2
  device attributes, `CSI 18 t` text-area size, and `OSC 10/11` color
  queries (`TerminalBuilder::background_rgb` configures the reported
  background). Capability-probing applications run instead of hanging.
  Recognized-but-unanswerable questions (XTGETTCAP, kitty `CSI ? u`,
  pixel-size reports, …) are named inside the next wait timeout error,
  turning a silent hang into a diagnosis; `answer_queries(false)` mutes
  the responder while keeping the diagnosis.
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
