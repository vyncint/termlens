# What 1.0 means

termlens is `0.x`. Until 0.11 every minor was allowed to break a consumer;
**from 0.11.0, the stability candidate, no promised item changes
incompatibly before 1.0** — the promise is the last section of this page.
1.0 is not a feature list — it is **three decisions written down with the
measurement that decided each**, plus that statement of which public items
the promise covers and what checks each part of it. A 1.0 tag requires all
three sections below to be filled in ([RELEASING.md](RELEASING.md) says
so), and each was allowed to come out *no*: an honest no is a 1.0 that
means something.

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

**0.11.0 is the stability candidate**: from that release no promised item
changes incompatibly before 1.0. A change that must break one ships as a
new candidate (0.12.0) with a migration table and restarts the observation
window in [#335](https://github.com/vyncint/termlens/issues/335); a patch
release does not. 1.0 follows the readiness criteria in that issue, not a
date.

The criteria are an external pilot, eight weeks of stable use counted from
its first green run, every maintained consumer on the candidate from
crates.io, and the daily fresh-install evidence. The README and the
CHANGELOG header carry the statement above in the same words, and
`.github/scripts/check-candidate-statement.sh` fails when one drifts.

Additive change is not a break: a new item, a new variant of a
`#[non_exhaustive]` enum such as `Error`, a new field of a
`#[non_exhaustive]` struct such as `Style`, a new CLI flag, a new key in
the JSON `state` object.

Every sentence below names the job or test that checks it, or says that
nothing does. The jobs are in `.github/workflows/ci.yml`; a sentence with
no check is a promise kept by review alone, and the list of those is meant
to be short.

### Promised

- **The documented public Rust API of every item termlens owns, in every
  supported feature configuration** — `default` (`insta`),
  `--no-default-features`, each single feature (`decode`, `regex`, `serde`)
  and all features together. Semver over all of it: an item is not removed,
  renamed, re-typed or moved behind a feature in any of those
  configurations without a new candidate. *Checked by* the `semver` job
  (`.github/scripts/check-semver.sh`: cargo-semver-checks against the last
  published release, release type forced to `patch`, in the default,
  explicit-only and all-features views; `tools/semver-gate-selftest/` proves
  the gate fails on a removed and on a gated item) and by the `features`
  job, which builds and tests each reduced configuration for the library
  alone (`.github/scripts/check-feature-isolation.sh`). *Not checked, and
  said so:* a method whose signature changes under the same name — the
  0.11 change to `unsupported()` was exactly that, and cargo-semver-checks
  0.50.0 reported only the removed `unsupported_overflow()` beside it. That
  class is caught by the crate's own tests and every consumer's compile,
  and by review.
- **The snapshot text format** of [DESIGN.md](DESIGN.md) §3, which
  `Screen::parse` reads back: the header, the grid, the `styles:` block and
  its tokens. A file written by any release from 0.10.1 on parses in every
  later release and renders back byte for byte. *Checked by*
  `crates/termlens/tests/compat.rs` over the frozen corpus in
  `tests/compat/<version>/` — the definition of "the format stays valid" —
  and by `crates/termlens-cli/tests/cli.rs`, which renders every corpus
  file through the CLI.
- **The JSON shape** (`serde` feature), format `1`, specified in DESIGN §3.
  JSON written by 0.10 or later is readable by every later release; a new
  format number is a new candidate, never a silent change. *Checked by* the
  same corpus test (every JSON twin reads as the same picture as its text
  and re-serialises to its own document) and by `tests/export.rs`.
- **The CLI's contract**: the commands `inspect`, `diff` and `render`, their
  accepted flags, the exit-code meanings (0 ran, 1 `diff` found a
  difference, 2 the tool could not run) and the saved-screen input formats
  — the text format with or without an insta header or a pre-0.11 `inspect`
  trailer, and the JSON. Flags may be added; `inspect`'s stdout is a saved
  screen. *Checked by* `.github/scripts/check-cli-contract.sh` against the
  tree on every pull request (`test` job) and against the **published**
  binary on every release and every day (`install.yml`, `cli` job).
- **The coordinate conventions**: `(row, col)` for cells, `(cols, rows)` for
  geometry, both zero-based. *Checked by* the crate's own suite; there is no
  separate gate, since every accessor test encodes them.
- **The platform list** of §1: Linux and macOS in full, Windows for what
  survives ConPTY's re-render, each exclusion named in
  [LIMITATIONS.md](LIMITATIONS.md). *Checked by* the `test` matrix and the
  required `windows` leg on every pull request, and `stress.yml` on all
  three before a release.
- **The MSRV policy**: `rust-version` in `Cargo.toml` is real for every
  supported feature configuration, and a bump is a minor release. *Checked
  by* the `msrv` job, which compiles the workspace, `-p termlens
  --all-features` and `-p termlens --no-default-features` at that toolchain
  against the committed lockfile.

### Third-party boundaries, named as such

Three things follow a dependency's versioning rather than termlens's, and
nothing else is delegated:

- the `termlens::insta` re-export and what `assert_screen_snapshot!`
  expands to follow **`insta`**'s versioning;
- the `regex::Regex` parameter type of `matches`, `find_match`,
  `find_all_matches`, `mask_matches` and `wait_until_matches` follows
  **`regex`**'s major version;
- the `Serialize`/`Deserialize` impls follow **`serde`** 1.x. The JSON
  *shape* does not: it is termlens's, format-numbered above.

### Not promised

- Human-readable prose: timeout and error messages (the prefixes in the
  skill's failure table are kept, the rest is prose), `inspect`'s trailer,
  the painting of `termlens diff` on a terminal, `Debug` output.
- Internal representation: the `Emulator` trait and everything under
  `emu/`, the storage behind any accessor (which is why `unsupported()`
  returns a view), the fixtures.
- The *timing* of the heuristics: `wait_idle`, `wait_stable` and
  `snapshot_after`'s settle are judgements about a quiet window; their
  shape is promised, their thresholds are not. The frame-history and
  record budgets keep their shape and their defaults may move.
- The SVG, HTML and ANSI renderings' exact bytes: what they show is what
  the screen shows, and the markup may change.

*Nothing checks these, by construction — they are the room the crate keeps
for itself.*
