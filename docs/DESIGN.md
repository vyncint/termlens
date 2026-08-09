# termtest — Design

Condensed architecture notes. This document is the contract for anything
touching wait semantics, the snapshot format, or the emulator boundary —
change those only with a matching change here.

## 1. Four layers

```
┌────────────────────────────────────────────────────────────┐
│ 4. Assertions      wait_until / wait_idle / wait_exit,     │
│                    Screen queries, insta snapshots          │
├────────────────────────────────────────────────────────────┤
│ 3. Screen          immutable, Arc-backed grid snapshots     │
│                    (Cell, Style, cursor) — termtest's own   │
├────────────────────────────────────────────────────────────┤
│ 2. Emulator        pub(crate) trait Emulator                │
│                    { process, snapshot, mid_sequence,       │
│                      set_size } — v0.1 backend: vt100       │
├────────────────────────────────────────────────────────────┤
│ 1. PTY             portable-pty: real kernel pty, spawn,    │
│                    resize (TIOCSWINSZ→SIGWINCH), reader     │
│                    thread draining master → emulator        │
└────────────────────────────────────────────────────────────┘
```

Data flows up: child writes → PTY master → reader thread → emulator →
screen snapshots. Input flows down: `Key::encode()` → PTY master → child.

**The reader thread** is the linchpin. It drains the PTY *continuously*, not
just inside `wait_*` calls, so a chatty program can never fill the kernel
PTY buffer and deadlock, and no output is lost "between" waits. It owns
nothing but a reader handle and the shared `Monitor<EmuState>` (mutex +
condvar): every chunk is fed to the emulator under the lock, then waiters
are notified. On EOF (child exited and released the terminal — reported as
0-read on macOS, EIO on Linux) it sets the `eof` flag and exits. It is
never joined: a grandchild holding the PTY open must not hang `Drop`.

## 2. Wait semantics

Every wait runs under the terminal's **default deadline** (builder
`timeout`, 5s default). There is deliberately no unbounded wait: a hung
TUI in CI must produce a readable failure, not a 6-hour job timeout. On
expiry the error **embeds the full screen dump** — a CI log alone answers
"what was the app showing?".

- `wait_until(pred)` — re-evaluates `pred` on a fresh snapshot whenever the
  reader delivers bytes (condvar notification), with a 50ms poll cap as a
  missed-wakeup backstop. Fails fast with `Error::Eof` the moment the PTY
  closes while `pred` is false: more waiting can never succeed.
- `wait_idle(quiet)` — resolves when **no bytes for `quiet`** AND the
  stream does not end mid-escape-sequence (or mid-UTF-8-character). The
  second condition comes from a minimal sequence tracker (`emu/seq.rs`) —
  not a VT parser, just enough state to answer "did the stream stop inside
  an update?". EOF counts as idle. **This is a heuristic**: silence is
  evidence of a finished render, not proof. Prefer `wait_until` on visible
  content. Roadmap: DEC private mode 2026 (synchronized output) gives true
  frame boundaries — a future `wait_frame` will use it where apps opt in.
- `wait_exit()` — polls `try_wait` on a capped backoff ladder (1→20ms),
  then grace-drains the PTY (≤500ms) so the final screen is complete
  before returning. Idempotent via a cached status.

`Drop` kills + reaps the child unconditionally (unless already reaped):
tests never leak zombies, including on panic.

### The instant-exit caveat (macOS pty teardown)

A child that **writes and exits within its first milliseconds** races the
platform's pty teardown: on macOS, bytes still buffered in the kernel when
the slave side closes can be discarded, and teardown can even surface as a
signal-death instead of the real exit code. Stress-testing found this at
roughly 1 in 80 instant-exit spawns under load. termtest narrows the window
as far as userspace allows — the reader thread is attached *before* the
child is spawned — but cannot close it.

The deterministic pattern (used throughout our own suite): make the child's
last action a `read` on stdin, assert on its output, then send Enter and
`wait_exit`. Real TUI applications are unaffected — they live far longer
than the window and exit on request.

## 3. Snapshot text format (spec)

`Display for Screen` produces:

```
size: 80x24  cursor: 2,3
╭──────────────────────────────╮
│ repoatlas                    │
│ > main.go                    │
╰──────────────────────────────╯
```

Rules:

1. Header line first: `size: <cols>x<rows>  cursor: <row>,<col>` — two
   spaces between the fields; cursor coordinates are `row,col` zero-based.
   A hidden cursor renders as `cursor: hidden` (TUIs hide the cursor
   deliberately; snapshots should say so).
2. Then the grid **verbatim**, one terminal row per line, top to bottom.
3. **Trailing whitespace is stripped per line** (snapshot files stay
   readable and diff-able), but blanks are *preserved inside* the `Screen`
   grid, so `cell()`, `row_text()`, and `find()` coordinates are unaffected.
4. Wide characters occupy their real columns: the character appears once;
   its continuation cell contributes nothing to the text but does count
   for column arithmetic (`find` returns true terminal columns).
5. Styles are captured per-cell (`Cell::style`) but not rendered in v0.1.
   v0.2 adds `Screen::with_styles()`, appending a styles block after the
   grid; the format will be specified here before it ships.

The `insta` feature (default) re-exports `insta` and ships
`assert_screen_snapshot!` so the snapshotting insta version can't drift
from the one the macro targets.

## 4. Emulator abstraction — why

`vt100` is the v0.1 backend: small, pure, battle-tested by its own suite.
But it is an implementation detail:

- Public types (`Screen`, `Cell`, `Style`, `Color`) are termtest's own.
  vt100 types never appear in the API, so swapping the backend is a
  non-breaking change.
- The `Emulator` trait is four methods (`process`, `snapshot`,
  `mid_sequence`, `set_size`) — deliberately the *narrowest* surface that
  the terminal loop needs, so candidate backends (wezterm-term for wider
  escape coverage, alacritty_terminal for fidelity to a real terminal's
  quirks) can be evaluated behind a feature flag without touching layer 3+.
- Known vt100 limits we accept in v0.1: no scrollback assertions (parser
  runs with scrollback 0), no reflow of scrollback on resize, cluster
  handling for exotic emoji is whatever vt100 does (pinned by the
  unicode-torture snapshot rather than promised).

## 5. Environment policy

Tests must be hermetic. `TERM=xterm-256color` is always set (unless the
test overrides it) so escape output matches what the emulator speaks
regardless of host. `env_clear()` blocks inheritance while keeping
`env()`-set variables — order-independent, unlike `std::process::Command`,
because a builder that silently drops your explicit `TERM` depending on
call order is a trap.

## 6. Input encoding

`Key::encode` maps to xterm *default-mode* sequences (CSI arrows, SS3
F1–F4, CSI-tilde function keys, DEL for Backspace). Applications that
enable application-cursor mode (DECCKM) still parse the CSI forms in every
mainstream input stack, so v0.1 does not track terminal modes for output.
If a real-world app under test needs mode-aware encoding, the emulator
already knows the mode — the hook exists, the complexity is deferred until
someone actually needs it.
