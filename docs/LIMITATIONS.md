# Known limitations

The user-facing list of what termlens does not model, does not render, or
does not claim — each with the reason and, where one exists, the accessor
that says so. `docs/STABILITY.md` §1 (Windows) and §3 (styled history) are
the decisions behind two of these; `docs/DESIGN.md` has the mechanics. An
entry here is a documented boundary, not a bug; a change to one is a
CHANGELOG entry.

## Grid and geometry

- **Terminal dimensions are 2–1000 cells per axis.** At one column, a
  double-width character overflows the backend's arithmetic; at one row, a
  line that wraps does the same (a one-row terminal that scrolls by newline
  is fine). Larger grids are refused because every snapshot costs one entry
  per cell.

## Scrollback

- Scrollback is **bounded** (1000 rows by default) and **text only unless
  asked**: `scrollback_styles(true)` on the builder retains cells too, so
  `Screen::scrollback_cell` keeps a masked-password assertion alive after
  the line scrolls off, at a measured cost the knob's docs quote. Either
  way `Screen::locate` says which region — grid or history — holds a
  needle, and a history column is the row's *as captured*: history is not
  reflowed, so it does not survive a narrowing resize. Otherwise a
  scrolled-off row has no styles and no cell addressing — and is **not
  reflowed** by a `resize`: rows keep the width they were captured at, by
  decision (`Terminal::resize` says why). The visible grid stays the
  fully-featured surface.

## Character sets, modes and tab stops

- **Character sets: G0–G3 designation, SO/SI locking shifts, SS2/SS3
  single shifts, and two sets translated.** `ESC ( ) * + Ps` designations,
  the `SO`/`SI` locking shifts, and `ESC N`/`ESC O` (SS2/SS3, one character)
  are modelled; the DEC Special Graphics set (`0`) and the UK set (`A`,
  `£` at `#`) are translated, and every other designation — the alternate
  ROMs, the other national sets — is acknowledged and reads as ASCII.
  `DECSC`/`DECRC` save and restore this state with the cursor. Locking
  shifts remain G0/G1 only (`LS2`/`LS3` are not modelled).
- **Insert mode (`IRM`, `CSI 4 h`) pushes the rest of the row right**, as
  ncurses's `insch` expects on a terminal advertising `smir`; `RIS` and
  `DECSTR` clear it, and `Screen::insert_mode()` reports an application
  that left it on. Other ANSI modes the backend drops (`LNM` and the rest)
  are not modelled — and *say so*: `Screen::unsupported()` lists every
  sequence the emulator did not implement, in the form `^[[20h`, so a test
  can tell a plausible-looking wrong grid from a right one.
- **A soft-wrapped line is two rows.** `contains` and `find` read the grid
  row by row and do not span the wrap; `Screen::row_wrapped(row)` reports
  the backend's record of where a line wrapped, and
  `Screen::logical_text()` joins wrapped rows back together for the
  assertion that spans one.
- **Tab stops are the application's to set.** `HTS` (`ESC H`), `TBC`
  (`CSI g`, `CSI 3 g`), `CHT` (`CSI I`) and `CBT` (`CSI Z`) all work, and a
  plain `\t` honours whatever stops are set rather than a fixed eight.
  `RIS` and `DECSTR` restore the every-eighth default, and a resize extends
  the set into its new columns with that pattern while leaving existing
  stops alone. Back-tab moves to the nearest stop *strictly* left of the
  cursor, as xterm does. Only `TBC 0` and `TBC 3` are modelled; the rest of
  that family clears *line* tab stops, which this crate has no notion of.

## Waits, frames and queries

- `wait_frame` needs the application to bracket its repaints in DEC 2026
  synchronized updates, and only the last 8 completed frames are retained;
  everything else waits with `wait_until`, under the three rules in
  [docs/DESIGN.md](docs/DESIGN.md) §2.
- Some questions stay deliberately unanswered — kitty's `CSI ? u`, DECRQSS,
  DA3, `OSC 12`, `OSC 52` *reads*, and the non-pixel `CSI … t` reports —
  because a guessed reply is worse than none. An application blocked on one
  is **named in the next timeout** rather than left to hang unexplained.

## Graphics and hyperlinks

- **Graphics are captured, not rendered.** termlens can tell an application
  that kitty or sixel is available (`graphics()`, `cell_size()`), collect
  what it then transmits, and — with the `decode` feature — decode a payload
  into pixels. It still draws none: an image never reaches the screen grid,
  so what a picture looks like *composited over the text under it* is not
  assertable, and `f=100` (PNG) payloads are reported unsupported rather
  than decoded, since termlens carries no image codec. Retention is bounded
  (4 MiB by default, `capture_graphics`); past it a payload is counted and
  described but its bytes are dropped, and it says so rather than decoding a
  prefix of itself. Support stays opt-in, so by default an application that
  probes is truthfully told there is none. Decoding also refuses anything
  above 4096x4096: every size in a payload is chosen by the program under
  test, and a sixel `!n` repeat or a declared `65535x65535` would otherwise
  set the allocation directly.
- **Hyperlinks are captured, not attributed to cells.** `links()` reports
  every `OSC 8` span with its target, its `id`, and the text it wrapped, so
  "did it link the right place?" is assertable — but a `Cell` does not carry
  its link, so *which* cells sit inside a span is not, and a span whose label
  was later overwritten is still reported, because this is a record of what
  the application emitted rather than a property of the grid. Retention is
  bounded to the most recent 64 spans, and a label longer than the capture
  bound is reported as unknown rather than as a prefix.

## Out-of-band state and styles

- **Out-of-band state is what the application last asked for, not what a
  terminal would infer.** The cursor shape follows `DECSCUSR` and is cleared
  by a hard reset (`RIS`); the window title is not, because in xterm the
  title is a window property that `RIS` does not restore, and guessing either
  way would be the same error. `DECSTR` (soft reset) resets what a `Screen`
  can observe — cursor keys, bracketed paste, mouse tracking, focus
  reporting, the cursor's visibility and shape, the character sets — and
  leaves the alternate screen alone; attributes, margins, origin and insert
  modes and the keypad are not modelled.
- **Two SGR style attributes are not modeled.** Overline (`SGR 53`) and double
  underline (`SGR 21`) do not reach [`Style`](https://docs.rs/termlens/latest/termlens/struct.Style.html),
  so `with_styles()` cannot distinguish those attributes from a plain cell.
  And bold and dim are **one intensity state**, not two: the last of
  `SGR 1`/`SGR 2` written wins, so a cell never reports both.

## The terminal input queue

- **A reply the terminal's own input queue cannot hold may not arrive.**
  termlens no longer drops answers of its own accord, but the tty input
  queue is small (~1 KB on macOS, ~4 KB on Linux), so an application that
  asks thousands of questions without reading has to read as it asks — as
  it would against a real terminal. On Linux the kernel discards silently,
  so that loss is undetectable and goes unreported; macOS blocks instead,
  where it is counted and named.

## Windows

- **Windows: screen assertions yes, frame assertions no.** The crate builds
  and the whole suite runs on `windows-latest` in CI, over ConPTY through
  `portable-pty`. ConPTY is not a passthrough — it renders the child's
  output into a screen of its own and re-emits *that* — so what termlens
  can honestly claim there is what survives the re-render: the grid (text,
  cells, styles, cursor, wide characters, box drawing, title, links by URL,
  clipboard, bracketed paste, cursor shape, bell, the alternate screen),
  resize, typed input, `wait_until` / `wait_stable` / `snapshot_after`,
  `bin!`. What it cannot claim, and documents as Unix-only: `wait_frame` and
  `frame_timings` (ConPTY closes a DEC 2026 bracket *before* the content it
  wrapped); `GraphicsPayload` and everything under `graphics` (kitty and
  sixel never arrive); the responder's outbound claims — `Graphics`,
  `background_rgb`, `foreground_rgb`, `cell_size` — since DA1, OSC 10/11,
  XTGETTCAP and DECRQM are answered by ConPTY itself and never reach
  termlens; `mouse_modes` and `mouse_mode`; `focus_events` (ConPTY turns
  1004 on for itself); link ids (ConPTY assigns its own); `Terminal::signal`;
  and bytes that are not UTF-8 (Rust's console stdio refuses to write
  them). The tests for each are `#[cfg_attr(windows, ignore = "…")]` with
  the reason in the attribute; the probe that measured all of this is
  `tests/conpty_probe.rs`, and the `windows` workflow re-runs it on demand.
  The `windows-latest` leg is a required check. This is decision 1 of
  [docs/STABILITY.md](docs/STABILITY.md).

## Process lifetime and Unicode

- A child that writes and exits within its first milliseconds can lose
  output to the OS PTY teardown (macOS especially). Long-lived TUIs are
  unaffected; for run-and-exit programs, end the script with a `read` and
  release it after asserting — see the "instant-exit caveat" in
  [docs/DESIGN.md](docs/DESIGN.md).
- Exotic grapheme clusters render as the vt100 crate renders them; the
  unicode-torture fixture pins the current behavior.
