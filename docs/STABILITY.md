# What 1.0 means

termlens is `0.x`, and every minor is allowed to break a consumer. 1.0 is
not a feature list — it is **three decisions written down with the
measurement that decided each**, plus a statement of which public items
the promise covers. A 1.0 tag requires all three sections below to be
filled in ([RELEASING.md](RELEASING.md) says so), and each was allowed to
come out *no*: an honest no is a 1.0 that means something.

Decided in 0.10 (September 2026). The issue each was decided in links
back here.

## 1. Windows — screen assertions yes, frame assertions no (#149)

**The question.** Does termlens run on Windows, and if so which of
`wait_until`, `wait_frame` and the query responder are supported there?

**The measurement.** `tests/conpty_probe.rs` sends twenty-one escape
sequences through a ConPTY (`portable-pty` 0.9, no passthrough mode) and
reports, per sequence, what the master read. Six came out verbatim. The
rest ConPTY eats (DA1, OSC 10/11, XTGETTCAP, DECRQM, mouse and focus mode
sets, kitty and sixel payloads, HTS/TBC), rewrites equivalently (SGR
reordered, OSC 2 as OSC 0, DEC graphics as UTF-8, a tab as `CUF`, link ids
of its own) or reorders: a DEC 2026 bracket closes *before* the content it
wrapped. It also sends every child a preamble (`CSI 6 n`, `?9001h`,
`?1004h`, a title, `?25h`) and does not start the child until the `6n` is
answered, and a child's exit is not EOF on the pipe. The mechanics are in
[DESIGN.md](DESIGN.md) §1.

**The decision.** Windows is a supported platform for what survives the
re-render — the grid (text, cells, styles, cursor, wide characters, box
drawing, title, links by URL, clipboard, bracketed paste, cursor shape,
bell, the alternate screen), resize, typed input, `wait_until`,
`wait_stable`, `snapshot_after`, `wait_exit`, `bin!` — and is documented
as Unix-only for what does not: `wait_frame`, `frame_timings`, `record`,
graphics, the responder's outbound claims (`Graphics`, `background_rgb`,
`foreground_rgb`, `cell_size`), `mouse_mode(s)`, `focus_events`, link
ids, `Terminal::signal`, non-UTF-8 bytes. Each test the platform cannot
honour is `#[cfg_attr(windows, ignore = "<the probe row>")]`. The
`windows-latest` leg runs the whole suite on every pull request and is a
required check; the `windows` workflow re-runs the probe on demand. The
[LIMITATIONS.md](LIMITATIONS.md) carries the user-facing list. A change to what
ConPTY does is a change to this section, not a bug in termlens.

## 2. Backend — `vt100` stays, and so does the shadow parser (#150)

**The question.** Does the emulator behind the `Emulator` trait stay
`vt100`, or move to `wezterm-term` or `alacritty_terminal` — which would
retire the second parser that recovers blink, conceal and strikethrough,
and give mouse modes and grapheme clusters a stronger footing?

**The measurement.** [BACKENDS.md](BACKENDS.md) holds the comparison:
attribute coverage, grapheme handling, mouse-mode fidelity, reflow,
dependency weight, MSRV, licence, publishability and maintenance cadence
of the three, against the seven methods the trait needs. The numbers that
decided it: `wezterm-term` is not on crates.io (a git dependency cannot be
published, and `deny.toml` refuses it); `alacritty_terminal` tracks
conceal and strikethrough but **not blink**, so a swap keeps one carrier
attribute in a shadow or a fork anyway, and brings 27 crates (an event
loop, `polling`, `rustix`, `signal-hook` — its tty half is not optional)
against `vt100`'s 9, under a single Apache-2.0 licence and an MSRV that
moves with the Alacritty release train; and the shadow's measured cost is
noise (260 ms with, 263 ms without, for 40,000 lines through 80x24).

**The decision.** `vt100` stays, and the shadow stays on purpose: two
parsers over one stream is a mechanism whose soundness is argued from
vt100's own code (attributes never influence geometry) and checked by a
debug assertion on every snapshot. What the backend does not do, termlens
does in front of it — character sets, tab stops, insert mode, the
unsupported-sequence record — on the same staged stream, which is why the
shadow keeps its shape. The trait remains the swap point, and BACKENDS.md
names what would reopen the question: upstream vt100 gaining the three
attributes (the shadow deletes), or a candidate that is published, tracks
all three, and can be driven headless without an event loop.

## 3. Styled history — a knob, off by default (#146)

**The question.** What is a scrolled-off row: text, or cells with styles?
The masked-password assertion — the one the shadow parser exists for —
silently degraded to text the moment the screen filled.

**The measurement.** With history retained as cells, 20,000 lines
through an 80x24 screen cost about 90 ms against 40 ms text-only (release
build, `tests/styled_history.rs`'s ignored probe). The cost lands where a
suite feels it: every read that scrolls captures cells, and a snapshot
pays one refcount per retained row.

**The decision.** History is text by default and cells on request:
`TerminalBuilder::scrollback_styles(true)` retains the styled rows, the
shadow parser keeps the same history and is read in lockstep so the three
recovered attributes come along, and `Screen::scrollback_cell` and
`styled_scrollback` read them back. `Screen::locate` answers the
addressing half — grid or history, and honest that a history column is
the row's *as captured*, since history is not reflowed (the reason is
`Terminal::resize`'s: a record exists, and the cost is what decides it).
Off by default because a suite that never scrolls should not pay, and
because the one assertion that needs it can ask.

## What the promise covers

Once the three sections above stand, 1.0 promises semver over:

- every `pub` item exported from `lib.rs` without a feature gate: the
  `Terminal`/`TerminalBuilder` surface, `Screen` and its accessors,
  `Cell`, `Style`, `Color`, the mode enums, `Error` (`#[non_exhaustive]`,
  so variants may be *added*), `bin!`;
- the snapshot text format of [DESIGN.md](DESIGN.md) §3, which
  `Screen::parse` reads back — a snapshot file recorded under 1.0 stays
  valid;
- the coordinate conventions: `(row, col)` for cells, `(cols, rows)` for
  geometry.

Behind features, versioned with the feature's dependency rather than with
termlens: `insta` (the macro follows insta), `decode`, `regex`, `serde`
(the JSON shape follows the types' derives).

Documented as heuristics, promised to keep their *shape* and not their
timing: `wait_idle` and `wait_stable` (a quiet window is a judgement),
`snapshot_after`'s 100 ms settle, and the frame-history and record
budgets, whose defaults may move.

Not covered: the `Emulator` trait and everything under `emu/` (internal),
the exact text of error messages (they carry screens; the prefixes in the
skill's failure table are kept stable, the rest is prose), the fixtures,
and `termlens-cli`'s output format beyond its exit codes.
