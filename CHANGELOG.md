# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Until 1.0, minor versions (0.x) may contain breaking changes; they are always
listed under a **Changed** or **Removed** heading, in a bullet that begins
**Breaking:** — the semver gate in CI reads that marker.

## [Unreleased]

### Changed

- **Breaking:** `Screen::unsupported()` returns an [`Unsupported`] view
  instead of `&[Arc<str>]`, and `Screen::unsupported_overflow()` is
  removed — the view carries the count (#330). The one breaking change of
  the 0.11 stability candidate, and the last before 1.0.

  The old return type was the storage: `Arc<str>` is how the screen keeps
  the list cheap to clone, and exposing the slice locked that
  representation into the public API for good. The ergonomics showed it —
  consumers wrote `.iter().map(|q| &**q)` to get at plain strings, and
  "nothing was dropped" took two calls. The view is `Copy`, borrows the
  screen, and compares equal to an array or slice of `&str` when the
  retained shapes match in order *and* nothing overflowed, so a pin is one
  line: `assert_eq!(s.unsupported(), ["^[[59m"]);`.

  | 0.10 | 0.11 |
  | --- | --- |
  | `s.unsupported().is_empty()` | unchanged (and now also false when shapes overflowed) |
  | `s.unsupported().len()` | unchanged — the retained count, at most 32 |
  | `s.unsupported().iter().map(\|q\| &**q)` | `s.unsupported().iter()` |
  | `s.unsupported().iter().map(\|q\| q.to_string()).collect::<Vec<_>>()` | `s.unsupported().iter().map(str::to_owned).collect::<Vec<_>>()` |
  | `s.unsupported().iter().any(\|q\| &**q == "^[[5m")` | `s.unsupported().contains("^[[5m")` |
  | `&*s.unsupported()[0]` | `s.unsupported().iter().next()` |
  | `s.unsupported_overflow()` | `s.unsupported().overflow()` |
  | `assert!(s.unsupported().is_empty()); assert_eq!(s.unsupported_overflow(), 0);` | `assert_eq!(s.unsupported(), []);` or `assert!(s.unsupported().is_empty())` |

  `Debug` prints `["^[[59m"]`, or `["^[[20h", …] (+8 more)` when shapes
  overflowed. `UnsupportedIter` is the view's `IntoIterator` type.

  What the semver gate saw, forced to `patch` against 0.10.3, in all three
  feature views: `inherent_method_missing: Screen::unsupported_overflow`.
  The changed return type of `unsupported()` itself is *not* a lint
  `cargo-semver-checks` 0.50.0 has, which is worth knowing about the gate:
  it catches a removed or gated item, not a re-typed one; the in-tree tests
  and every consumer's compile do.

### Added

- **A frozen saved-screen corpus** (#327). Nothing pinned that a file
  written by 0.10.1 still parses: the insta snapshots are re-recorded when
  output changes, so a change to the header, the `styles:` block or the
  JSON shape would have updated them and passed.
  `crates/termlens/tests/compat/0.10.1/` holds six shapes — plain text,
  every style token, a hidden cursor, wide and combining characters, a
  masked screen, and the style-blind text of a styled screen — each with
  its JSON twin, written by the **published** 0.10.1 through a registry
  dependency. A file there is never edited; a new version adds a
  directory. `tests/compat.rs` parses every text file and re-renders it
  byte for byte, reads every JSON twin as the same picture and
  re-serialises it to its own document, and the CLI suite renders each
  file with `termlens render --text`. Editing a corpus file was observed
  to fail the test.

- **The JSON a `Screen` serialises to carries `"format": 1`** (#329),
  first in the document. `termlens diff` and `termlens render` read that
  JSON back as a saved artifact, so it is a persisted format, not a wire
  between two copies of one version, and it now says which shape it is.
  A file written by 0.10 has no such field and reads as format 1, since
  that is what it is; a number this build does not know is refused with a
  message naming both numbers. The shape is specified in
  `docs/DESIGN.md` §3 beside the text format: field names, the cursor
  object, how a hidden cursor and wide/continuation cells are encoded,
  and that nothing a `Screen` holds is omitted.

- **`Screen::cursor_visible()`** (#331). The tuple `cursor()` returns is
  unchanged; the new accessor is for the assertion that reads
  `s.cursor().2` today and says nothing at the call site.

- **`termlens::ScreenWithStyles` is exported** (#332). `Screen::with_styles`
  returned it, but the type lived in a private module and was not in the
  crate's re-export list — reachable but unnameable, so it could not be
  stored in a field or returned from a helper. A doctest names it from a
  consumer.

- **`Location::is_on_screen()`, `is_in_history()` and `col()`** (#306),
  for the one-fact questions `Screen::locate` is asked most — "still
  visible?" and "what column?" — without a `match`. Deliberately no
  `row()`: a grid row and a history row are different things, and the
  rustdoc says so.

- **`ScreenDiff::changed_rows()` and `style_changes()`** (#308). The diff
  computed both and exposed neither, so a test that wanted "the highlight
  moved from row 1 to row 2 and nothing else changed" dedup'd `cells()` or
  matched a substring of the rendering. Both read fields already stored;
  the rendering is unchanged.

- **`Color` implements `Display`** (#315), producing exactly the `styles:`
  block token — `4` for an indexed colour, `#1e1e2e` for RGB, and the word
  `default` for [`Color::Default`], which the block never writes because a
  default-styled span is omitted. The block now uses it, so the two
  halves of the documented format — this and `Screen::parse` — sit
  together; every existing snapshot is unchanged.

- **A semver gate on every pull request, forced and per feature view**
  (#323). The only check was in `release.yml`, on the tag, and it let the
  version number decide how much to check: a `0.10 -> 0.11` bump is
  inferred as a major release and every check is skipped, so a public
  function removed in the same PR as the bump passed. And `--all-features`
  cannot see an item moved behind a feature — that view never loses the
  item — while Cargo calls exactly that a major change.

  `ci.yml` now runs `cargo-semver-checks` against the last published
  release with the release type forced to `patch`, three times: default
  features, only explicit features, all features. A deliberate break
  carries the `breaking` pull-request label and is recorded in this file as
  a `- **Breaking` bullet, which is what keeps `main` green after the merge
  where no label exists; either declaration switches the run to `major`
  (`minor` still fails a removed item on a 0.x crate — measured, and pinned).
  `tools/semver-gate-selftest/` holds three one-file model crates and a
  script that asserts the gate fails on a removed item and on a gated item,
  and passes the baseline against itself, so the gate was seen to fail
  before it was trusted to pass. The baseline is a literal in `ci.yml`,
  moved after each publish (`docs/RELEASING.md`).

- **The MSRV job compiles every feature configuration** (#325). It ran
  `--all-targets` under the default set only, so `decode`, `regex` and the
  library's `serde` code had never met Rust 1.85 in CI while the README
  advertised one MSRV for the crate. Both ends of the feature axis compile
  at the floor today, without a bump.

### Fixed

- **`termlens inspect … > file` saves a screen `diff` and `render` read**
  (#340). `inspect` was the only command that produced a screen and the
  only two that consumed one refused its output: the `--- exited: … ---`
  trailer went to stdout under the screen, so a redirect captured it and
  `render` exited 2 on line 26. Measured against the published 0.10.2; no
  flag suppressed it, and the workaround was a `sed` a user had to
  invent. The trailer now goes to **stderr** — a human at a terminal
  still sees both — and stdout carries the screen alone. The CLI also
  drops exactly those three trailer shapes when they are the last line of
  a file, so a screen saved by a 0.10 `inspect` reads too; a grid row
  that merely begins with `---` is content. `docs/DESIGN.md` §3 now says
  what a reader skips at each end of a file and that `Screen::parse`
  skips neither. `.github/scripts/check-cli-contract.sh` asserts the
  round trip against the published binary on every release; run against
  the published 0.10.3 it fails on exactly this.

- **The minimal library build is now actually tested** (#324). Cargo
  unifies features across every package in one invocation, and
  `termlens-cli` depends on the library with `features = ["serde"]`, so
  `cargo test --workspace --no-default-features` tested a library with
  `serde` on. The `features` job selects the library alone for each
  reduced configuration and asserts the isolation
  (`.github/scripts/check-feature-isolation.sh`). It was not hypothetical:
  `tests/record.rs` used `serde_json` unconditionally, and
  `cargo test -p termlens --no-default-features` did not build until this
  release added it as a dev-dependency.

## [0.10.3] - 2026-09-10

### Fixed

- **Every link on the crates.io page pointed at a 404** (#341). The root
  README is shipped as this crate's readme (`readme = "../../README.md"`),
  and crates.io rewrites a *relative* link against the crate's directory in
  the repository rather than the repository root — so `docs/DESIGN.md`
  became `…/blob/HEAD/crates/termlens/docs/DESIGN.md`, and all ten relative
  links on the 0.10.2 page were dead: the design, stability, limitations and
  backends documents, the skill, the changelog, contributing, security, both
  licences and the stress workflow.

  Nothing here could see it. The same file renders correctly on GitHub,
  where it really is at the root, so the bug existed only on the surface
  most new readers arrive at.

  The links are absolute now, and `.github/scripts/check-readme-links.sh`
  (in `ci.yml`) refuses a relative one — and separately checks that every
  absolute target still exists in the repository, offline, which catches a
  renamed file that a link checker asking GitHub would miss.

  Fixed for future releases only: crates.io renders each version's readme as
  published, so the 0.10.2 page and earlier stay as they are.

## [0.10.2] - 2026-09-10

### Added

- **The published CLI is installed and held to its documented contract**
  (#326). `install.yml` verified `cargo add termlens` and never
  `cargo install termlens-cli`, so #310 — `termlens inspect --version`
  exiting 2 — shipped in 0.10.0, shipped again in 0.10.1, and was found by a
  user of the published binary rather than by anything here.

  A `cli` job installs `termlens-cli` from crates.io on Linux and macOS —
  every run, never from a cache — and asserts every exit code
  `termlens --help` documents: `--version` in all four positions returning
  the same string, `inspect` printing a screen with its header and trailer,
  `render` in all four formats, and `diff` returning **0** for the same
  picture, **1** for a different one and **2** for input it cannot read.

  The assertions live in `.github/scripts/check-cli-contract.sh` and take
  the binary to exercise, so the same script runs against a path build. `ci.yml`
  does exactly that on every pull request: the published check is the net,
  the tree check is the fast feedback. Re-introducing #310 locally was
  observed to fail it.

### Fixed

- **`unsupported()` no longer names the four SGR parameters the attribute
  shadow implements** (#320). `5`/`25` (blink), `9`/`29` (strikethrough),
  and `6`/`8`/`28` with them are exactly what `emu/shadow.rs` exists to
  recover: vt100 drops them, the shadow parser puts the attribute on the
  cell, and the tracker then reported the *backend's* gap under an accessor
  documented as naming the emulator's.

  The consequence was the one the accessor was added to prevent, inverted: a
  screen said `Style::blink` was true and `unsupported()` said `^[[5m` had
  been dropped, at the same instant — so a reader checking the list before
  trusting a blink or masked-password assertion concluded a correct
  assertion was unreliable, and
  `assert!(screen.unsupported().is_empty())` could never pass for an
  application that uses either attribute.

  Found by `termlens-demo` driving a real ratatui application against the
  published 0.10.1, which is what the testing tier is for.

  **What still gets named.** An SGR with one parameter nobody models keeps
  the whole sequence: `^[[59m` (underline colour) alone, and `^[[5;59m`,
  where blink is recovered and `59` is not. Dropping a mixed sequence
  because part of it is implemented would hide a real gap.

  **The residue, measured rather than assumed.** `^[[1;5;31m` is still
  named although bold, red *and* blink all reach the cell — `1` and `31`
  are vt100's to implement, and this tracker knows what the shadow
  recovers, not what the backend does. Narrowing that needs vt100's own SGR
  surface enumerated, which is more than a patch should claim; a test
  records the behaviour so it is a documented edge rather than a surprise.

- **`termlens <subcommand> --version` is no longer an unknown option.**
  The flag was handled only in the top-level dispatch, so `termlens inspect
  --version` exited 2 while `-h`/`--help` worked in that position. All three
  subcommands now print the same version string as the top-level command. (#310)

## [0.10.1] - 2026-09-08

### Changed

- **The stress workflow runs on Windows too.** It hunted flakes on Linux
  and macOS only, which left the newest leg — required since 0.10 — as the
  one whose liveness is *synthesized* rather than observed: Unix reads the
  terminal-closed edge off the kernel (EOF on the master), while Windows
  manufactures it from a reaped child plus a drain grace, and polling with
  a grace window is the shape a one-in-N flake lives in. `fixtures`'
  in-test `cargo build -p` is the other, on the one filesystem that
  refuses to relink a running `.exe`. Same five `--test-threads` shards,
  different faults at the ends of the axis, and double the per-shard clock
  because a Windows iteration costs two to three times a Linux one. (#290)

### Fixed

- **An exported recording scrolled every frame up by one row.**
  `Screen::to_ansi` ends every row with a newline, the bottom one included
  — right for a file, wrong for a repaint: replayed, that last linefeed
  sits on the last row and scrolls the picture away, so `asciinema` showed
  a screen the test never saw. `Recording::to_asciicast` now drops exactly
  that newline. The regression test replays each exported event into a
  terminal of the recorded size and compares every row against the frame it
  came from, rather than checking the event's shape. (#295)

- **`Screen::parse` deleted grid rows that read like a styles block.** The
  split between grid and metadata was a search for a `styles:` marker, and
  a screen can *contain* that word: a snapshot of one came back blank. The
  header's row count now decides where the grid ends, which is unambiguous
  for anything `Display` wrote. The one input that cannot be read both ways
  — a hand-trimmed grid that also carries a styles block — resolves as
  content and says so in the rustdoc, because a wrong row of text shows up
  in a diff and a silently dropped one does not. (#296)

- **`Screen::parse` rejected a combining mark after a wide character** —
  its own format, for a cell the emulator stores correctly. A wide glyph
  advances two columns, so stepping one back lands on the continuation
  half, which holds no text; the mark now attaches to the leading cell that
  owns the glyph. (#297)

- **A hidden cursor made a snapshot differ from itself.** The text format
  records `cursor: hidden` without a position, so a parsed screen came back
  at `0,0` and `ScreenDiff::is_empty` compared the coordinates anyway —
  a difference with nothing visible behind it, on a screen state most TUIs
  are in. A hidden cursor draws nothing, so its position is no longer part
  of the picture, for `diff` and for `wait_stable` alike; visibility, and a
  visible cursor's position, still are. (#298)

- **`Screen::parse` panicked on a non-ASCII hex colour.** `fg=#a€bc` is six
  *bytes*, so the length check passed and slicing in pairs landed inside a
  character — an unwind out of the one function whose job is turning bad
  text into `Error::Parse`, and the CLI shares it. Every byte is now
  checked as an ASCII hex digit before any of them is read. (#299)

- **`mask_matching` left a needle that spans rows fully visible.** It is
  documented as matching "the way `find_all` matches", and `find_all`
  crosses row boundaries — but the mask ran its matcher one row at a time,
  where a newline can never appear. It reported the match and masked
  nothing, which is precisely the failure a mask exists to prevent.
  `find_all` and the masks now share one multi-row engine, so they cannot
  disagree about what matched. (#300)

- **A fixture announced it was ready before it could be resized.**
  `form-echo` (and the new `ratatui-app`) drew their first frame — the one
  a test synchronizes on — before the first `event::read()`, and crossterm
  registers its `SIGWINCH` listener on that call. A resize landing in the
  gap was lost for good, since SIGWINCH's default disposition is ignore,
  and `wait_frame` then ran out a full deadline against an application that
  had simply never heard. `resize-echo` already guarded against this; the
  other two now do the same. Found by the stress workflow on macOS at one
  thread. (#292)

- **A query test raced its own fixture's pause.**
  `a_query_the_app_moved_past_is_context_not_a_cause` spent one 400 ms
  budget on both of its waits — the one that must *succeed* and the one
  that must *expire* — while the fixture deliberately sleeps 200 ms before
  printing the marker the first one waits for. A spawn plus that pause on
  a loaded macOS runner at four threads exceeded the budget, and the test
  failed claiming the harness had blamed a probe it should not have. The
  short deadline now sits on the wait that must expire and nowhere else,
  the split `probe` already made. Found by the stress workflow's first run
  on the new three-OS matrix, at 1 in 5 iterations on that shard. (#290)

## [0.10.0] - 2026-09-08

### Added

- **Styled history, opt in: `TerminalBuilder::scrollback_styles(true)` and
  `Screen::scrollback_cell`.** A row lost every style the moment it scrolled
  off, so the masked-password assertion — the one v0.4 paid a second parser
  for — silently degraded to text the instant the screen filled. With the
  knob, the scrolled rows are retained as cells beside the text, the shadow
  parser keeps the same history and is read in lockstep so conceal, blink
  and strikethrough come along, and a snapshot still pays one refcount per
  retained row. Off by default, because the cost lands where a suite feels
  it — every read that scrolls captures cells, and a full history is re-read
  on each — and measured in the knob's rustdoc: about 90 ms against 40 ms
  for 20,000 lines. (#146)

- **`Screen::locate`**: where a needle is, on the grid or in history, as a
  `Location` that says which. The addressing half of the scrolled-off-text
  problem, honest about what history can promise: a history column is the
  row's display column as it was captured, not a grid coordinate, and does
  not survive a narrowing resize. (#147)

- **`Terminal::record()`.** Every complete frame from that moment on,
  timestamped, until `stop()`: `Recording::frames()` for asserting on the
  sequence of repaints an animation went through, and `write_asciicast()`
  for a file `asciinema` plays and `agg` turns into a GIF — each frame a full
  repaint through `to_ansi`. Bounded by `TerminalBuilder::record_budget`
  (cells; default about a thousand 80x24 frames), oldest frames dropped and
  the drop reported; refuses, with `wait_frame`'s diagnosis, an application
  that never emits synchronized updates, because a sampled recording is the
  torn-frame problem in a new hat. (#254)

- **`Screen::diff`.** Two screens that differed printed as two whole grids;
  `a.diff(&b)` renders only the rows that changed, side by side over a
  marker line under the changed columns, the size and cursor deltas, a count
  of unchanged rows and the style runs before → after. `is_empty()` and
  `cells()` for assertions; `assert!(a.diff(&b).is_empty(), "{}", a.diff(&b))`
  is the documented way to compare two screens outside insta. A `wait_frame`
  timeout now shows the diff from the frame it last returned to the live
  screen. Plain text, so CI logs stay readable. An erased cell and a
  written space in the same style are one blank, not a difference. (#246)

- **`docs/STABILITY.md`: what 1.0 means.** Not a feature list — three
  decisions written down with the measurement that decided each: Windows
  (screen assertions yes, frame assertions no; the ConPTY probe), the
  emulator backend (`vt100` stays and so does the shadow parser;
  `docs/BACKENDS.md` compares it with `alacritty_terminal`, which drops
  blink and brings an event loop, and `wezterm-term`, which is not on
  crates.io), and styled history (a knob, off by default, 90 ms against
  40 ms). Plus which public items the promise covers, which stay behind
  features, and which are heuristics. `docs/RELEASING.md` requires all
  three sections for a 1.0 tag. (#256, #150)

- **`termlens-cli`: the `termlens` command, and `Screen::parse`.** Two
  saved screens could be compared only by `cargo insta review`, on the
  machine that ran the tests. `cargo install termlens-cli` provides
  `termlens inspect` (the example grown a command: a program in a PTY, its
  screen printed, `--ansi` for colour), `termlens diff a b` (the cell diff
  of two saved screens — an insta `.snap` with or without its header, the
  block a wait error prints, a `TERMLENS_ARTIFACT_DIR` file, or the JSON
  the `serde` feature writes — coloured on a terminal, plain in a pipe,
  exit 1 when they differ) and `termlens render --svg|--html|--ansi|--text`.
  The genuinely new piece is in the library: `Screen::parse` reads the
  snapshot text format of DESIGN §3 back, `styles:` block included, so the
  format round-trips — `Screen::parse(&s.with_styles().to_string())`
  renders to the same text and diffs empty against `s`. A text that is not
  the format is `Error::Parse`, naming the line. The CLI's own tests drive
  it through a PTY with termlens. (#255)

- **`TERMLENS_ARTIFACT_DIR`, and the `report` action.** A failing wait's
  screen reached the CI log and stopped there. With the variable set, every
  error that carries a screen also writes it to that directory —
  `<test>-<n>.screen.json` with the `serde` feature, `.screen.txt` (the
  `with_styles` rendering) without — and
  `vyncint/termlens/.github/actions/report`, run with `if: failure()`,
  renders those files and every `.snap.new` (with the diff against its
  `.snap`) into the pull request's step summary, SVG and HTML uploaded as
  an artifact. Off when unset; documented on `Error`. This repository's
  own `test` job runs the action. (#251)

- **`fixtures/ratatui-app` and its fidelity test.** No fixture was a
  ratatui application, so the one check only a PTY harness can make for
  ratatui users was never made here. The fixture is a counter/list with
  every repaint in a DEC 2026 bracket, `j`/`k`/`q`, and a resize
  acknowledged on its status line; `fixtures/ratatui-app/tests/fidelity.rs`
  renders the same `draw` through the PTY and through `TestBackend` and
  diffs the two cell by cell — cells and styles, at two sizes with a resize
  between. The README's comparison section now lists what `TestBackend`
  structurally cannot see, each with the termlens assertion that does.
  ratatui 0.30 needs Rust 1.88, so the fixture carries its own
  `rust-version` and the MSRV check excludes it; the library's floor is
  unchanged. (#252)

- **`Screen::to_ansi`, `to_svg`, `to_html`.** Three renderings a person can
  see — paste the ANSI into a terminal and the failure is on screen in
  colour; the SVG is self-contained for a bug report or a README; the HTML
  is a `<pre>` of `<span style>`s for a step summary or a PR comment. Pure
  functions of the cells and styles, no dependency, a wide character one
  glyph over two columns. (#248)

- **A `serde` feature** (off by default): `Serialize`/`Deserialize` on
  `Screen`, `Cell`, `Style`, `Color`, `CursorShape`, `MouseMode`,
  `MouseModes`, `Link`, `Clipboard` and the graphics types. A `Screen` is
  rows of cells, so a JSON snapshot diffs by row; `Color` is a tagged enum
  (`{"indexed": 1}`, `{"rgb": [30, 30, 46]}`) so nothing needs a parser on
  the way back; deserializing re-checks that every row holds exactly `cols`
  cells. `insta::assert_json_snapshot!(t.screen())` works. (#247)

- **`Screen::find_all`.** Every occurrence of a needle in reading order,
  from the same scan `find` is the first element of — NFC-folded, trimmed
  per row, real columns across wide characters — so the two cannot drift.
  Non-overlapping, multi-row needles supported, a `Vec`; the reasons for each
  are in the rustdoc. "This warning appears exactly once" and "click the
  second item" are writable without opting out of normalisation. (#264)

- **A `regex` feature** (off by default): `Screen::matches`, `find_match`,
  `find_all_matches`, `mask_matches`, and `Terminal::wait_until_matches` /
  `_for`, which returns the screen it matched on. Matching is per row over
  the row's text as `contains` sees it, and the column reported is a cell
  column — the expect-style wait, pointed at a row of the rendered screen
  rather than at the byte stream. (#245)

- **Grid-aware masks: `Screen::mask_rect`, `mask_matching`, `mask_cells`.**
  Each returns a new `Screen` with cell *contents* replaced and the size,
  cursor, styles and wide-character structure kept, so a clock or a PID can
  be redacted without moving a column — which is what insta's text filters
  cannot do for a grid. A masked screen snapshots, `find`s and compares like
  any other, and its `styles:` block is unchanged, so a colour regression
  stays visible through the redaction. (#250)

- **`Screen::unsupported()`: the sequences the emulator did not implement,
  so a plausible-looking wrong grid can be told from a right one.** The
  backend reports every escape it cannot render and termlens installed the
  no-op callbacks and threw the list away. It is kept now — distinct shapes,
  first seen first, in the timeout messages' form (`^[[20h`), 32 kept and
  the rest counted in `unsupported_overflow()` — filtered to what *termlens*
  did not honour, since the character sets, tab stops, insert mode and every
  query the responder answers still reach the backend's callbacks. A request
  to resize the window is listed rather than obeyed; `visual_bells()` counts
  `ESC g` separately from `bells()`, since a flash is not a beep. (#266)

- **`Screen::row_wrapped()` and `Screen::logical_text()`.** A line long
  enough to wrap is two rows, and a needle spanning the wrap was found by
  nobody although a reader plainly saw it. The backend has always recorded
  which rows soft-wrapped and was never asked; each snapshot now carries the
  bit, and `logical_text()` joins wrapped rows back together so
  `logical_text().contains("brown fox jumps")` is true. `contains` and `find`
  are unchanged and document the trap beside the scrollback one. The two
  sentences that said history could not be reflowed *for lack of the
  record* now give the true reason, cost. (#265)

- **Windows.** The crate builds and the whole suite runs on `windows-latest`
  in CI, over ConPTY. Screen assertions work; the features ConPTY renders
  away — `wait_frame`, graphics, the responder's outbound claims, mouse
  modes, focus reporting, link ids — are documented as Unix-only in the
  README, with their tests marked so and the probe that measured them in
  `tests/conpty_probe.rs`. Two things a Windows user would have hit first
  are fixed on the way: `bin!` refused an absolute `C:\…` path as a bare
  program name, and a child that had exited was still "listening" — a
  pseudoconsole never reports EOF, so the reaped child now closes the
  terminal there. (#149)

- **A skill for AI coding agents.** `skills/termlens/SKILL.md` teaches an
  agent how to test a terminal program with termlens without the mistakes
  agents make on their own: no `sleep`, `snapshot_after` for whole-screen
  snapshots, the 2x2 geometry floor, `bin!` for hermetic spawns, the two
  coordinate orders, `wait_frame` only for applications that emit
  synchronized updates (stock ratatui does not), and four copy-paste recipes
  — a CLI snapshot, a ratatui navigation flow, overriding defaults, targeted
  cell and style assertions. Every Rust block in it is compiled against the
  crate in CI, and the README shows the one-line install for Claude Code.

### Changed

- **The `windows-latest` leg is a required check.** It ran non-required
  from #279 and held on every push to `main` since; a ConPTY surprise now
  blocks a merge, which is what claiming the platform in
  `docs/STABILITY.md` means. `windows.yml` stays as the on-demand report
  with the probe. (#280)

- **`assert_screen_snapshot!` earns its name.** It was `insta::assert_snapshot!`
  under another name. Given a `Terminal` it now settles the picture
  (`wait_stable(100ms)`, or `snapshot_after(pred)` with `after = pred`) and
  snapshots **with styles** by default; `styles = false` is text-only, the
  inline `@""` form still works, and a `Screen` argument still records the
  screen as it is. Built on `snapshot_after`/`wait_stable`, so it adds no
  waiting logic of its own; its rustdoc is now the home of the race rules.
  The README's first example uses it and drops the rule-2 paragraph it no
  longer needs. The macro uses `?`, so the test returns `Result` — which
  every test should. (#253)

- **`Screen::row_text` rejects an out-of-bounds row instead of returning an
  ambiguous empty string.** Callers with a potentially invalid index should
  bounds-check against `rows()` first, or use `cell(row, 0)` and treat `None`
  as absent. (#260)

- **`Terminal::drag` takes four column-first coordinate arguments instead of
  two unlabelled tuples.** This matches the other mouse methods and prevents a
  row-first `Screen::find` result from being passed through transposed. (#257)

- **`Style` is non-exhaustive so new terminal attributes can be added without
  another breaking change.** Downstream struct literals should migrate to
  `Style::default()` followed by assignments to relevant fields. (#259)

- **The `inspect` example clears the child environment by default.** It keeps
  `PATH` so bare program names work; `--env` adds selected values and
  `--inherit-env` restores the previous behavior. (#263)

### Fixed

- **Insert mode (`CSI 4 h`) is honoured.** `smir`/`rmir` are in the
  terminfo entry every child is handed, and ncurses uses the mode for
  `insch`; it was parsed and dropped, so an inserted character ate the rest
  of the line and a snapshot could bless the eaten tail. The flag lives in
  the sequence tracker beside the character sets; while it is set the
  emulator reserves room for each printable run with the `ICH` the backend
  does dispatch — in columns, so a wide character counts twice, and never
  past the right margin. `RIS` and `DECSTR` clear it, and
  `Screen::insert_mode()` reports an application that left it on. (#261)

- **Custom tab stops are honoured: `HTS`, `TBC`, `CHT` and `CBT` do what
  they say.** Stops were fixed at every eighth column — the backend's
  hardcoded value — and all four escapes reached a dispatch table with no
  entry for them and vanished, though `hts`, `tbc` and `cbt` are all in the
  terminfo entry termlens hands every child. An application that laid a
  table out by setting its own stops and tabbing between them drew every
  column in the wrong place, and did it *silently*: the characters were all
  present, so `contains` still passed and only a column assertion or a
  whole-screen snapshot could catch it. The stop set now lives in the
  sequence tracker beside the character sets, and the emulator rewrites each
  motion as a `CHA` the backend does dispatch. Plain `\t` is rewritten too,
  or the backend's eight and a custom stop would disagree the moment one was
  set. `RIS` and `DECSTR` restore the every-eighth default; a resize extends
  the set into its new columns with that same pattern and leaves the stops
  it already had alone. `CBT` is also what `Shift-Tab` sends, so an
  application echoing one now moves. (#262)

- **docs.rs labels APIs gated by the `decode` and `insta` features.** Optional
  items no longer appear to be available in every build. (#258)

## [0.9.0] - 2026-09-05

### Added

- **`Error::Emulator`: an emulator panic is a diagnosis, not a timeout.** The
  emulation runs on the reader thread, so a panic there propagated nowhere —
  the drain died, the screen froze, and every wait burned its full deadline
  reporting a predicate that could never come true. The reader now catches it,
  records it, and keeps draining (a stalled drain blocks the child writing
  into a full buffer); `wait_until`, `wait_frame` and `wait_idle` fail at once
  with the emulator's own message and the last screen taken before the
  failure. The emulator is never asked for a screen again — after a panic its
  state means nothing. `wait_exit` is deliberately unaffected: the child's
  exit status is still true. (#211)

- **`snapshot_after` and `wait_stable`: the whole-screen snapshot as one
  call, and a settle that output changing nothing cannot hold up.**
  `snapshot_after(pred)` waits for the predicate, then for the picture to
  hold still for 100ms, and returns that screen — DESIGN §2's three rules
  for race-free waits without having to remember them. `wait_stable(quiet)`
  is the settle on its own, and differs from `wait_idle` in what resets the
  clock: changes rather than bytes, so a bell, a cell rewritten with the
  glyph already in it or an answered query — output `wait_idle` can never
  see silence through — is invisible to it. Both have `_for` twins, refuse
  to settle inside an open synchronized update, count stillness that
  predates the call, and return the screen they settled on.
- **`termlens::bin!("myapp")` spawns one of your package's binaries under
  the harness defaults.** Every integration test of a binary opened with the
  same five lines — a fixed 80x24 grid, `env_clear()`, a five-second
  deadline, `spawn(env!("CARGO_BIN_EXE_myapp"))` — so the chain has a name.
  Builder calls follow the name and override any default:
  `termlens::bin!("myapp", size(120, 40), env("NO_COLOR", "1"))?`. A
  misspelled binary is a compile error naming the variable, not a spawn
  failure at run time.
- **`Screen::mouse_modes` reports every mouse tracking mode the application
  enabled, and `DECRQM` answers each one on its own evidence.** The backend
  collapses `?9`/`?1000`/`?1002`/`?1003` into the one protocol a terminal
  reports in — right for the input path, and unchanged there — so it could
  not say which members of the group an application asked for: crossterm's
  `EnableMouseCapture` sends three at once and only the last survived, a
  regression from any-motion to button-motion tracking (losing hover) was
  invisible, and a `DECRQM` probe for any member but the last had to be
  answered "not recognized". The sequence tracker now keeps the requested
  set; `mouse_mode()` still reports the protocol. (#151)
- **The fresh-install check verifies a `--no-default-features` consumer as
  well as a `decode` one.** `install.yml` is the only job that builds
  termlens from outside this workspace, and it did so in one shape — the
  one the fewest real consumers use: of the three in-house ones, two declare
  `default-features = false`. Its matrix now runs both shapes on Ubuntu and
  macOS, the registry check demands the `decode` feature only on the leg
  that asks for it, and the no-defaults leg fails if the consumer's tree
  still resolves `insta`. (#238)
- **The `inspect` example answers `--help`, and takes its deadline and
  silence window from flags.** `inspect --help` used to look for a program
  called `--help`, and both timings were hardcoded, so an application slower
  than five seconds to paint its first screen could not be inspected at all.
  `--timeout SECONDS` (default 5) and `--idle MILLIS` (default 300) now sit
  beside `--size`; `--help`/`-h` print one usage text to stdout and exit 0,
  a missing program prints the same text to stderr and exits 1, and
  `--version` names the termlens version the example was built from. An
  unknown option is refused rather than spawned. (#229, #236)

### Changed

- **The smallest terminal is 2x2, not 1x1.** One column panics the emulator on
  a double-width character, and one row panics it on a line that *wraps* — on
  the reader thread, in both profiles, where the panic propagates nowhere: the
  grid froze, every later wait ran to its deadline against a plausible-looking
  screen, and `cargo test` printed `test result: ok` over a suite that had
  stopped testing anything. `80x1` is an ordinary shape, not an exotic one.
  `spawn` and `resize` now refuse a dimension below 2 with `Error::Size`, the
  way they already refused 0 — #49's "no path can reach the emulator with a
  zero" was satisfied exactly one value too low. `2x8` and `2x2` render both
  trigger shapes correctly, so the floor is the smallest guard that closes
  them. (#211)

- **A child starts in the test process's working directory, not `$HOME`.**
  Without `current_dir` the PTY layer fell back to the home directory, so a
  relative `spawn()` path and a directory-sensitive program behaved unlike
  every other Rust process API — while `current_dir`'s rustdoc promised the
  test runner's directory all along. The default is now what
  `std::process::Command` does; `current_dir` still overrides it. A test that
  relied on the old fallback should say `.current_dir(std::env::home_dir())`
  explicitly. (#215)

### Fixed

- **G2/G3 designation and SS2/SS3 single shifts are modelled, so a
  one-character line-drawing shift draws `┌` rather than `l`.** `ESC * 0`
  / `ESC + 0` designate the DEC Special Graphics set into G2/G3, and
  `ESC N` (SS2) / `ESC O` (SS3) invoke that set for exactly one character
  before the locking shift resumes. The designation was already consumed;
  the shift did nothing, so a mixed line of text and box-drawing showed
  the letter. A pending single shift is consumed by the next character —
  including a multi-byte UTF-8 one — and does not survive `RIS`. Locking
  shifts remain G0/G1 only (`SO`/`SI`); `LS2`/`LS3` and `DECSC`/`DECRC` of
  charset state are still unmodelled. (#235)

- **`DECSC`/`DECRC` save and restore the character-set state.** Save,
  jump, draw the frame, restore is how a full-screen application draws a
  border, and the restore lost the designation, so the border after it
  rendered as `lqk` — the failure #204 fixed, arriving through a different
  door. `ESC 7` now saves G0–G3 and the locking shift alongside the cursor
  the backend already saved, `ESC 8` restores them, a restore with nothing
  saved returns to ASCII as xterm does, and `RIS` clears the slot so a
  restore cannot resurrect a designation from before the reset. (#232)
- **`DECSTR` (soft reset, `CSI ! p`) is modelled.** The polite reset a
  well-behaved TUI sends on startup and teardown parsed cleanly and did
  nothing, so text printed after it kept rendering in the character set the
  application had told the terminal to forget, while `RIS` got this right.
  It now returns the character sets and the `DECSC` slot to power-on, the
  cursor shape to the terminal's default, and turns off cursor-key mode,
  bracketed paste, every mouse tracking mode and encoding, and focus
  reporting; the cursor becomes visible and the alternate screen is left
  alone, as specified. Attributes, margins, origin and insert modes and the
  keypad are not replayed — nothing on `Screen` observes them — and the
  README says so. (#233)
- **The UK character set is translated: `ESC ( A` then `#` draws `£`.**
  The designation was parsed and then rendered as ASCII, so an application
  printing a price in the UK set showed `#42` on the grid, a test asserting
  `£42` failed against a correct application, and a snapshot that blessed
  `#42` kept passing. The set differs from ASCII in that one position, so
  that is the one byte translated; the alternate-ROM sets and the other
  national sets still read as ASCII, and the docs now say which sets are
  translated. (#234)
- **`find` no longer matches the blank padding past the end of a row.** Its
  single-row path searched the row padded out to the terminal width while
  `contains` searched the trimmed text, so `find("Total: ")` was `Some` on
  a screen whose row read `Total:` — against the invariant both rustdocs
  state, that a needle is found precisely when `contains` is true. Both now
  trim trailing whitespace per row first; the trim treats a drawn trailing
  U+00A0/U+3000 as padding too, and both rustdocs say so. (#212)
- **`env_clear()` no longer leaks the machine's login shell.** The PTY layer
  fills `SHELL` from the host when the variable is absent, so a child's
  supposedly hermetic environment differed between two machines with
  different shells. `SHELL=/bin/sh` is now pinned under `env_clear` the way
  `TERM` is, an explicit `.env("SHELL", …)` still wins, and a test asserts
  the whole environment rather than probing one name. (#221)
- **A bare program name under `env_clear()` is refused with the remedies.**
  Clearing the environment removes `PATH`, so `spawn("sh")` could not
  resolve and failed with the PTY layer's "Unable to resolve the PATH",
  which named neither the cause nor a way out. `spawn` now refuses it up
  front with an `Error::Spawn` that says `env_clear` removed `PATH` and
  offers both fixes: an absolute path, or `.env("PATH", …)`. (#222)
- **A byte that is not UTF-8 shows as U+FFFD instead of vanishing.** The
  backend drops both a byte it cannot decode and the U+FFFD its own parser
  substitutes for one, so a Latin-1 `é` in a file name left no trace on the
  grid and every column after it shifted left — a test asserting on the
  column of what followed passed against the wrong screen. Bytes are now
  decoded once on the reader thread: an invalid sequence becomes the
  replacement character a terminal shows (carried through the backend as a
  noncharacter it will draw, and restored in the snapshot), and a character
  split across two reads is carried rather than replaced. `wait_idle` treats
  a stream that stopped mid-character as not yet idle. (#217)
- **A highlight over CJK or emoji is one span again.** A wide character's
  continuation column carried no style, so `with_styles()` rendered a bar
  over `ab汉cd` as two spans with a hole and `cell(row, col).style()` on
  the second column reported `Default` — DESIGN §3 rule 5 and `find_by`'s
  rustdoc both promised the opposite. The snapshot now gives the
  continuation the leading cell's style, as a terminal paints it. (#218)

## [0.8.0] - 2026-08-29

### Added

- **DEC Special Graphics is translated, so an ncurses border reads as
  `┌───┐` rather than `lqqqk`.** `ESC ( 0` selects the line-drawing set —
  it is what `smacs`/`rmacs` are on an xterm terminfo, so it is how every
  ncurses application and plenty that are not draw their frames — and the
  bytes used to reach the grid untranslated. That put the crate's own
  promise in the wrong: a user sees a box, and a test asserting a border was
  right failed against an application that was correct, while a snapshot
  that had blessed `lqqqk` went on passing after the border broke. The
  `vt100` backend drops the designation entirely, so termlens tracks the
  G0/G1 designations (`ESC ( Ps` / `ESC ) Ps`) and the `SO`/`SI` locking
  shifts itself and hands both parsers the glyph a byte draws. Scope is
  stated rather than implied: only the DEC Special Graphics set is
  translated, other designations read as ASCII, G2/G3 and the single shifts
  are not modelled, and a hard reset (`RIS`) returns both sets to ASCII.

- **`Terminal::scroll_with`, with `Scroll::ctrl()` / `alt()` / `shift()`
  and a `ScrollChord` type.** `Ctrl`-wheel is zoom and `Shift`-wheel is
  horizontal scrolling in a large share of terminal applications, and
  neither could be sent: `scroll` took a bare direction, so the binding most
  likely to be wired to the wrong handler was the one with no coverage. The
  modifier bits ride on the wheel's button code exactly as they do on a
  click, and `scroll` is now the unmodified case of `scroll_with`, as
  `click` is of `click_with`. The wheel gets a chord type of its own rather
  than a wider `MouseChord`, so a wheel direction cannot be handed to
  `click_with`: a notch has no release, and the type keeps that from being a
  runtime surprise.

- **`Screen` implements `PartialEq` and `Eq`.** It was the only public
  value type without them, so "did anything change?" was written by
  comparing `to_string()` renderings — which is text only, and so passes
  two screens that differ in a highlight, a colour or a concealed field.
  Equality means *the same observation*: cells, cursor, size, and every
  piece of out-of-band state including the cumulative `repaints` and
  `bells` counters, so two visually identical snapshots either side of a
  bell compare unequal because they are different moments. The doc says
  which comparison to reach for when "looks the same" is what is meant.

- **A content wait that fails while rows have scrolled off says so.** The
  most-copied line in these docs, `wait_until(|s| s.contains(..))`, reads
  the visible grid, so text the application printed and then scrolled away
  in the same burst can never satisfy it — and the screen embedded in the
  timeout does not show the text either, so the failure read as "the app
  never printed it". `Error::Timeout` and `Error::Eof` from `wait_until`
  and `wait_frame` now say how many rows have scrolled off the top and
  point at `Screen::full_text`. The note is conditional, because the wait
  cannot know what an arbitrary closure was looking for; it is silent when
  nothing has scrolled. `contains` and `find` document where they stop.

### Changed

- **`resize` is refused once the child has released the terminal**, with
  the same `Error::Write` that `send` returns, naming the child and its exit
  status. It used to succeed, and the snapshot then reported a geometry no
  application ever rendered at with the dead child's last frame clipped
  underneath it — the one operation on a departed child that silently
  mutated observable state. The final screen stays readable at the size the
  child exited with. A size that cannot work is still `Error::Size`, checked
  first.

### Fixed

- **`Bitmap::colours` is linear in the pixel count, and its order is
  defined.** It counted distinct colours with a scan of the distinct set per
  pixel — O(pixels × colours), 9 ms for 96x96 and extrapolating to seconds
  for a screenshot, in a test suite, where a wait that takes seven seconds
  looks like a hang. It now counts in one pass. Ties, which the old
  implementation happened to order by first appearance through a stable
  sort, are now ordered that way on purpose and the doc says so: a test
  asserting on `colours()[0]` has one answer.

- **`wait_frame` after `resize` could miss the repaint that answered it.**
  `resize` sent the `SIGWINCH` first and took the frame cursor afterwards, so
  a fast application's acknowledging repaint could complete in that gap, be
  counted as a frame from *before* the resize, and never be offered — the
  wait then timed out reporting that the application had not repainted,
  while the live screen showed that it had. Found by the stress workflow at
  16 threads, once in 25 runs. The grid is now resized and the cursor taken
  before the signal goes out, under one lock, so a frame drawn in answer to
  the resize is always newer than the cursor and always lands in a grid of
  the new size.

- **`resize` documents the resize-then-type trap.** A keystroke that reaches
  a crossterm application in the same instant as the `SIGWINCH` can be lost:
  its event reader returns the `Resize` as soon as the poll reports the
  signal and abandons the input readiness delivered alongside, and the poll
  is edge-triggered, so the byte is not offered again until more input
  arrives. The stress workflow caught termlens's own suite doing exactly that
  — a resize followed at once by `Esc` hung the fixture about one run in
  forty. The test now waits for the application to acknowledge the resize,
  which is the advice the doc gives.

- **`resize` documents what happens to history.** Rows already in scrollback
  keep the width they were captured at and rows captured afterwards have the
  new width, so `full_text()` after a narrowing resize can hold both
  geometries. That was true before and said nowhere a caller would meet it;
  it is now a recorded decision on `resize` and `scrollback_text`, with the
  alternative — discarding history on resize — rejected in writing rather
  than by omission.

- **Doc comments on `TerminalBuilder::size`, `spawn` and `resize` name
  `Error::Size`** for a rejected size, not `Error::Input`. The links
  resolved, to the wrong variant, so following them gave a `match` arm that
  never fires — and `Error` is `#[non_exhaustive]`, so a wildcard elsewhere
  would have swallowed the mistake silently.

- **Colon-form SGR colours now reach cell styles.** `38:2::r:g:b`,
  `38:2:r:g:b`, and indexed foreground and background colours are normalized
  before the backend parses them, matching their semicolon-form equivalents.

- **Mouse `click` / `click_with` / `drag` / `scroll` refuse coordinates
  outside the current grid.** A real terminal cannot produce an off-window
  mouse event; sending one was the same class of mistake as clicking with
  no tracking enabled. The error names the position and the grid size at
  the time of the call (so a post-`resize` rejection is obvious). `drag`
  checks both endpoints only — the interpolated path cannot leave the
  rectangle they span. Separately, the SGR encoder no longer wraps or
  panics at `u16::MAX`: the 1-based `+ 1` is done in `u32`.

### Security

- **`SECURITY.md` no longer claims the crate has no `unsafe`.** It has two
  blocks, both FFI and both present since the features they serve landed:
  `dup(2)`, which opens the responder thread's writer on the PTY master, and
  `kill(2)` behind `Terminal::signal`. Neither touches memory the child can
  influence and the parsing path has none, which is the claim that matters;
  the policy now says exactly that rather than something stronger and
  false.

## [0.7.0] - 2026-08-27

### Added

- **`TerminalBuilder::envs` sets several child environment variables from an
  iterator of key-value pairs.** Values keep their iteration and builder-call
  order, and remain explicit when `env_clear` disables inherited variables.

- **`Screen::links` reports the `OSC 8` hyperlinks an application emitted.**
  A hyperlink changes no cell — its label renders exactly as unlinked text
  would — so the URL existed nowhere a test could reach, and an assertion
  that a TUI linked an issue, a file or a doc page **passed identically
  against an application that emitted no link at all, or linked the wrong
  target**. Captured rather than answered, on the same grounds as the
  `OSC 52` clipboard: the only evidence otherwise available is the
  application's own visible output, which proves the code path ran and
  nothing about where it points.

  Each span reports its `uri`, its `id` (spans sharing one are one logical
  link), the `label` it wrapped, and whether the application ever `closed`
  it — an unterminated link is a real defect, because in a real terminal
  every character written afterwards joins it. Two bounds keep the capture
  honest: the log holds the most recent 64 spans and evicts oldest-first, so
  a TUI that redraws its links every frame still reports the current
  frame's; and a label past the capture bound is reported as *unknown*
  rather than as a prefix, since a prefix of the wrong length is a wrong
  answer.

- **`Screen::cursor_shape` and `Screen::cursor_blink` report `DECSCUSR`.**
  A screen where the application asked for a bar and one where it never
  asked used to be the same `Screen`. The shape is load-bearing behaviour
  rather than decoration — a modal editor switches to a bar for insert and
  back to a block for normal, and "the mode indicator says INSERT" and "the
  terminal was actually put into insert" are different claims. It also makes
  the *restore* assertable, which is the half that ships broken: a program
  that changes the cursor and never changes it back leaves the user's
  terminal wrong after exit, the same class of defect `alternate_screen()`
  already catches.

  Shape and blink are one `DECSCUSR` parameter but two facts, so they are
  reported apart. `CursorShape::Default` — the application never sent the
  escape — is a third state and is reported as itself rather than folded
  into `Block`.

- **`Key::Insert`**, encoding `ESC [ 2 ~`, and chording like its
  neighbours (`Key::Insert.shift()` → `ESC [ 2 ; 2 ~`). It was the `2`
  missing from a navigation run that already had `3`, `5` and `6`, so an
  application binding Insert could not be tested without hand-writing the
  escape.

### Changed

- **`Key` and `Signal` are now `#[non_exhaustive]`.** This is breaking for
  downstream code that `match`es either without a wildcard arm; adding a
  `_ => …` fixes it, and equality and construction are unaffected.

  Worth doing now rather than later. Adding a variant to an exhaustive
  public enum is itself a breaking change, so every future key and every
  future signal would have cost a version of its own — `Key` has no F13+ and
  no keypad, and `Signal` carries seven of POSIX's thirty, missing
  `SIGWINCH` and `SIGCONT`, which are exactly what a terminal application
  reacts to. Both types are *constructed* far more often than matched
  (`t.send(Key::Enter)`, `t.signal(Signal::Int)`), so the cost falls almost
  entirely on the crate and not on its users. `#[non_exhaustive]` is
  breaking to add, which makes the cheapest moment the earliest one.

  `Color` is deliberately left exhaustive. Default, palette index and 24-bit
  RGB is the whole terminal colour model — there is no fourth variant
  waiting — and `Color` is the one enum here that downstream code really
  does match on.

### Security

- **A decoded image can no longer choose how much memory it allocates.**
  `GraphicsPayload::decode` trusted four sizes that the program under test
  writes: kitty's `s=`/`v=`, sixel's raster attributes, its `!n` repeat count
  and its `#n` colour-register index. A compressed kitty payload was also
  inflated with no output limit, and zlib reaches about 1000:1. Each turned a
  handful of bytes into a request for tens of gigabytes — `!4294967295~` is
  twelve bytes; a declared `65535x65535` is about twenty and asks for 17 GB
  before the pixel data is touched at all.

  Decoding is now bounded: no image above 4096x4096 (far beyond what a
  terminal can place, and 64 MiB of RGBA once built), sixel colour registers
  capped at 65536, and a compressed payload inflated only as far as its
  declared size needs. Refusals are a new `DecodeError::TooLarge` rather than
  a silent clamp — `DecodeError` is `#[non_exhaustive]`, so matching on it
  already required a wildcard.

  Present in every release before this one. It sits behind the off-by-default
  `decode` feature and is reached only when a test calls `decode()`, so a
  suite that merely counts images was never exposed. `SECURITY.md` now
  enumerates these bounds with the rest.

### Fixed

- **A hard reset (`RIS`, `ESC c`) returns the cursor shape to the terminal's
  default and closes any open `OSC 8` span.** `printf '\033c'` is one of the
  ways a program hands the terminal back on exit, so reporting the last
  `DECSCUSR` after one claimed a shape the terminal no longer held — and it
  did so in exactly the case `cursor_shape` exists to check. The window
  title, the clipboard, the bell count and the link *log* are deliberately
  left alone: the title is a window property `RIS` does not restore in
  xterm, and the rest are records of what the application emitted rather
  than state the terminal still holds.

- **The crate's doctests build with default features disabled.** The bundled
  snapshot macro example is compiled only when its `insta` feature exists,
  and CI now runs `cargo test --workspace --no-default-features` so this
  supported configuration cannot silently rot again.

## [0.6.1] - 2026-08-23

### Fixed

- **`spawn` no longer fails when the machine is briefly out of PTY devices.**
  On macOS a PTY is torn down with `revoke()` and its device recycled, and a
  suite asking for devices faster than the kernel returns them gets `ENXIO` —
  "Device not configured", which reads like a broken machine and is really a
  queue. `cargo test` runs one test per core by default, so this is what a
  sixteen-core Mac does with any suite of this shape; the failure was not
  exotic, it was Tuesday. `openpty` is now retried for about 1.6 seconds
  before giving up, **releasing the PTY lifecycle lock between attempts** —
  that lock is the one a teardown also takes, so waiting under it would have
  blocked the only work capable of freeing a device.

  Found by the stress workflow the first time it ran the suite at sixteen
  threads, on macOS; Linux had run the same suite twenty-five times over
  without noticing. `tests/concurrency.rs` now applies the same pressure on
  purpose — two dozen terminals at once, and eight rounds of open-and-recycle
  — so it is reproducible rather than a matter of which shard drew the short
  straw.

## [0.6.0] - 2026-08-21

What an application *drew*, as against how many bytes it spent drawing it.

Inline graphics were observable only as a count and a size: an image had
gone out, and it had been about so big. Three things were wrong with that,
and the first two were wrong rather than merely thin — the count was of
escapes, not of images, so the kitty protocol's own 4096-byte chunking
inflated it and a delete posed as a transmission.

### Added

- **`GraphicsSeen::payloads` — the transmissions themselves.** Each
  `GraphicsPayload` carries its protocol, action, format, compression,
  image id, the pixel size and cell extent the application declared, the
  bytes it cost, the chunks it took, and the data itself. Placement is the
  one fact that lives in the grid rather than in the payload, so `at()`
  reports the cursor position at the terminator — the image's top-left
  corner for both protocols. An application that lays out in characters and
  draws in pixels can now be held to keeping the two in step, which is a
  failure nothing on screen shows: a picture that slides out from under its
  own labels leaves every cell exactly as it was.
- **`GraphicsSeen::deletes`**, counting kitty `a=d` — images taken *off*
  the screen — apart from images transmitted.
- **The `decode` feature: `GraphicsPayload::decode` and `Bitmap`.** Kitty
  `f=24`/`f=32`, zlib'd or not, and the sixel data stream decode into
  pixels, so an assertion can be about the picture rather than about its
  size. Off by default: it is the one thing here needing a dependency of
  its own (zlib), and every other fact about a payload stays free. Refusals
  name their reason — `f=100` (PNG) is unsupported rather than guessed, a
  delete carries no image, and a payload past the capture bound says so
  instead of decoding a prefix of itself into a plausible wrong picture.
- **`TerminalBuilder::capture_graphics`**, the retention budget: 4 MiB by
  default, `0` to keep counts and drop every byte. Bounded like scrollback,
  and the counters stay exact whatever the bound.
- **The `image-echo` fixture**, which transmits a known image over kitty
  (compressed, plain, and chunked), over sixel, and with a delete after it.

### Fixed

- **A chunked kitty transmission is one image, not one per escape.** The
  protocol caps a payload at 4096 bytes and continues with `m=1`, so a
  4.9 KB chart counted as two images and the continuations — which carry no
  control block — counted as pictures nothing could be said about. The
  chunks are joined before anything is counted.
- **A kitty delete is no longer counted as an image transmitted.** `a=d`
  carries no picture. Every byte of it is still counted in `bytes()`: a
  delete is traffic.

### Changed

- **`GraphicsSeen` is `Clone` rather than `Copy`**, since it now carries the
  payload list. Existing code that reads a counter is unaffected; code that
  copied the value into two bindings needs a `clone`.

## [0.5.0] - 2026-08-20

What the harness could not observe, could not reach, and quietly got wrong.

Seventeen issues, every one verified against the published 0.4.2 before a
line was written — four by reproductions that contradicted the report, and
one of those by a reproduction that contradicted *me*. Three themes:
behaviour a test could not see at all (repaints, bells, images, focus),
applications that could not be driven down a path they probe for first, and
accessors that answered confidently where they had nothing to say.

Two API changes are breaking, both in the direction of honesty:
`send`/`send_str`/`paste` return `Result`, and `ExitStatus::code` returns
`Option`.

### Changed

- **`send`, `send_str` and `paste` return `Result<()>`** and no longer
  panic. Every input call in the crate is now fallible, so a write that
  cannot be delivered is something a test can see, handle, or propagate
  with `?` — previously the only route from a failed write to the test was
  aborting it. Call sites grow a `?`; that is the whole migration.
- **Typed input to a closed terminal is refused identically on Linux and
  macOS.** It was not: a write to a master whose slave descriptors are all
  closed fails with `EIO` on macOS and *succeeds* on Linux, queueing the
  bytes for a reader that no longer exists. The same keystroke was
  therefore an error on one CI runner and silently discarded on the other.
  Every sender now checks for a closed terminal before writing, so the
  answer is the same everywhere and no keystroke is lost quietly.
- **A batch of startup probes is answered in full**, and the reply queue is
  now bounded by **memory rather than by queue slots** — which took three
  attempts to get right, each one teaching what the invariant actually is.
  200 queries asked back to back returned 173 answers; 400 returned 235; 1000
  returned 285. The stated cause — the application had stopped reading — was
  wrong: the same 200 queries a millisecond apart were all answered, so
  nothing was blocked anywhere. The reader was enqueueing one entry per
  *reply* while the writer issued one `write(2)` per entry, so it outran the
  writer and the 64-slot queue overflowed. Batching per *read* fixed that on a
  fast machine — but on a slow one an application's writes dribble out, the
  same 400 queries arrive in hundreds of small reads, and 64 slots ran out
  again at 235 of 400. Slots were never the thing worth bounding: the queue is
  now unbounded with a 1 MiB ceiling on undelivered reply *bytes*, so the
  reader can never block, a real application is never shorted, and a hostile
  one still cannot grow memory without limit. The writer coalesces whatever is
  queued into a single write.
- **Undelivered replies are counted whether dropped or blocked mid-write**, so
  a non-reading application is named in the wait error rather than producing a
  plain timeout.
  **One diagnosis got weaker on Linux, and that is the price of the fix
  above.** The note used to appear there because replies overflowed *our* queue
  — the same overflow that was losing a well-behaved application's answers.
  With that fixed, the replies reach the kernel, and the platforms diverge: a
  write into a full terminal input queue blocks on macOS, where the backlog
  stays visible and the count is exact, while Linux's `n_tty` *discards* input
  once its 4 KB buffer is full — the write succeeds, the bytes are gone, and
  nothing distinguishes that from delivery. We cannot report what we were never
  told. `docs/DESIGN.md` §1 states the split; the trade is a diagnosis for a
  pathological application in exchange for a well-behaved one actually
  receiving its answers.
- **`drag` reports one motion per cell crossed**, on a straight interpolated
  path, instead of a single report at the destination. Seven cells crossed
  used to produce one motion event. Invisible to an application that only
  asks "where did it start, where is it now" — which is why it went unnoticed
  — and wrong for every application that does something *along* the path: a
  drawing surface painting each crossed cell, a selection highlighting
  incrementally, a drag that must cross a pane edge to register. The
  mode-aware refusals are unchanged: `?1000` still hears no motion at all,
  and X10 is still a typed error.
- **A mouse action at a departed child names the child.** `click`, `drag`
  and `scroll` check liveness *before* the mouse-tracking mode, because a
  child that has exited necessarily never enabled tracking either — so the
  old order reported a missing `CSI ?1000 h` for a terminal whose
  application was simply gone. The tracking-mode error is unchanged for a
  live application that really has not enabled it.
- **`ExitStatus::code` returns `Option<u32>`**, `None` when a signal killed
  the child. A signalled process has no exit status — POSIX gives one or
  the other — and the OS placeholder (1) that filled the slot made
  `assert_eq!(status.code(), 1)` pass on a `SIGTERM` path, which would keep
  passing if the application later started exiting 1 for a real reason.
  `Display` no longer prints the invented `(code 1)` tail either. Mirrors
  `std::process::ExitStatus::code`.
- **`Screen::rect_text` panics on a backwards range** instead of returning
  `""` or a bare `"\n"`, and both axes now behave identically — they did
  not. It reads as "this pane is empty", a plausible assertion outcome, so
  a call with its arguments swapped passed for the wrong reason and kept
  passing. A panic rather than an error for the same reason `&slice[3..0]`
  panics: a backwards literal range is a mistake in the calling source, not
  a fact about the terminal. Out-of-range bounds are a different thing and
  stay clamped.
- **An implausible terminal size is refused**, at most 1000 per axis, with
  the limit named. `5000x5000` used to spawn happily and then spend 16
  seconds inside the first wait before timing out with a message about the
  predicate — a transposed `.size()` turned a sub-second test into a wedged
  one with no hint of why. `resize` is held to the same limit.

- **`Screen::contains` and `Screen::find` fold both sides to NFC**, so a
  needle finds text the application normalized the other way. A terminal
  draws `caf\u{e9}` and `cafe\u{301}` identically — and so do the failure
  output and the diff, which is what made the mismatch a trap rather than a
  limitation: an author types NFC (what editors produce) while text from a
  filesystem path, a git author name or macOS input is frequently NFD.
  Unconditional, and no escape hatch is needed because the raw form is never
  taken away: `text`, `row_text`, `rect_text`, `cell` and `title` all still
  return exactly the codepoints the application sent. One consequence worth
  knowing: matching is grapheme-shaped, so on a screen showing `caf\u{e9}`,
  `contains("cafe")` is now false — the screen does not show `cafe`.

### Added

- **`Screen::repaints`** — how many synchronized updates the application has
  completed, as of this observation, on every snapshot **including the frames
  `wait_frame` returns**. It counts *repaints, not changes*, so a
  Begin/End pair that drew nothing still counts, which is exactly the
  property an amplification test needs: "one wheel notch produced four
  repaints" is invisible to every content predicate, because each
  intermediate frame shows correct content.
- **`Terminal::frame_timings` and `FrameTiming`** — per-repaint wall-clock cost
  and printable-character count, so a suite can hold a performance line as well
  as a correctness one. A TUI's most common regression is not wrong output; it
  is a repaint that got slower or larger, and no content predicate sees either.
  Both ends of the span are stamped at the byte carrying the marker, not when
  the read arrived, so a burst delivered in one read is still timed per frame.
  The docs state what the span includes rather than leaving it to be assumed:
  it is measured through a PTY and covers the application's write pacing, so it
  is a trend to watch and not a render benchmark. Bounded at 512 repaints,
  independently of the eight frames `wait_frame` retains, since a timing is
  three words where a frame is a whole grid.
- **`Screen::bells`** — how many times the application rang `BEL`. The bell
  is often the only feedback a rejected input produces, so "an invalid key
  does nothing" and "an invalid key is refused with a bell" used to be the
  same screen. A count, not a flag, so twice differs from once; and only a
  `BEL` in ground state counts, since the one terminating an `OSC` string is
  punctuation and one inside a DCS-class string is payload.
- **`Screen::graphics`** and **`GraphicsSeen`** — kitty (`APC G … ST`) and
  sixel (`DCS q … ST`) payloads transmitted, by protocol, with total bytes.
  The assertion this exists for is as often the negative one —
  `assert!(s.graphics().is_empty())`, "this must render as text in every
  terminal and never go out as an image" — so `is_empty` is a method rather
  than something to spell out. Observing is not rendering and claims
  nothing: DA1 still declines both protocols.
- **The kitty graphics query is diagnosed.** `APC _G…a=q…ST` was swallowed
  whole — no answer *and* no mention in the timeout note, alone among the
  startup probes, because `string_final` inspected only `+q`/`$q` and an APC
  matches neither. An application blocked on it now gets the same one-line
  diagnosis `^[[?u` and `^[P+q…` already got. Only an explicit `a=q` counts
  as a question: a transmission is an instruction, and treating one as a
  query would put "the application queried the terminal" into the next
  timeout of every application that draws.
- **`XTGETTCAP` is answered** — the last of the common startup probes with no
  reply. A capability termlens genuinely implements gets a truthful
  `DCS 1 + r <name>=<value> ST`; anything else gets an explicit
  `DCS 0 + r <name> ST`, which is the half that turns a hang into a decision:
  the application learns the answer is no instead of waiting for one. The set
  is `TN`/`name` (whatever `TERM` the child was actually given, so the two
  cannot disagree), `Co`/`colors`, and the cursor, home/end, delete, page and
  backspace keys — each the exact bytes `Key::encode` emits, checked against
  the code that emits them rather than copied from a terminfo file. One reply
  per requested capability, because the status flag is per-reply and a mixed
  request cannot be answered in one frame without lying about half of it.
- **`TerminalBuilder::cell_size`** — pixels per character cell, which is the
  one number every layout decision in an image-drawing application rests on.
  `CSI 16 t` then answers `CSI 6 ; h ; w t`, `CSI 14 t` answers the window
  size in pixels, and `TIOCGWINSZ` carries the same geometry instead of
  contradicting it; a `resize` recomputes all three. Opt-in: unset, the two
  reports stay unanswered and the ioctl reports zero pixels — which is what a
  real terminal reports when it has none, so the default is not a lie and no
  existing suite moves onto a pixel branch.
- **`TerminalBuilder::graphics` and `Graphics`** — declare the inline-graphics
  support of the terminal being simulated. `Graphics::Sixel` adds `4` to the
  DA1 reply; `Graphics::Kitty` answers the `a=q` capability probe with
  `APC _G i=<id> ; OK ST`, echoing the id the probe named. Default unchanged:
  nothing claimed. This is not the harness lying — it is the test author
  stating which terminal is simulated, the way `background_rgb` states a
  background — and it matters because for an application that *probes first*
  the pixel path is not merely unasserted, it is unreachable: the code never
  runs, so nothing about it is testable.
- **`Terminal::send_after`** — wait, then send, so this write and the previous
  one land in separate reads. The remedy for the `Esc` wire ambiguity when the
  `Esc` has no observable effect to wait for: a vim-style TUI where `Esc`
  leaves insert mode silently and `j` then moves down could not be driven at
  all, because sending them together is byte-identical to `Alt+j`. The delay
  is a named argument, not a hidden constant, and `send(Key::Esc)` carries no
  default separation — most suites send `Esc` with nothing behind it, and a
  hidden sleep would slow all of them for a hazard they do not have while
  making the tests that need it work for a reason invisible at the call site.
- **`Terminal::focus_in` / `Terminal::focus_out`** and
  **`Screen::focus_events`** — focus reporting (mode 1004). The unfocused
  branch of a UI was not merely unasserted, it was **unreachable**: no input
  existed that could enter it, so the code never ran. Mode-aware like every
  other input — refused with a typed error when the application never enabled
  1004, exactly as `click` is refused without mouse tracking. `DECRQM` now
  answers for 1004 as well, since termlens tracks it exactly, which is the
  honesty rule's precondition; it previously reported "not recognized" even
  immediately after the application enabled it.
- **`Error::Write`**, carrying the screen at the moment of the failed
  write, the way `Error::Timeout` and `Error::Eof` already do.
  `Error::screen()` returns it.
- **`Screen` is 40 bytes instead of 80**, with all out-of-band state behind
  one `Arc`. A `Screen` is embedded in every `Error`, so this shrinks every
  `Result` in the crate, and a clone — taken on each wait evaluation — is
  now one refcount bump rather than a field-by-field copy.

### Documented

- **The README, crate docs and design notes describe 0.5.** The README's
  headline example now propagates the `Result` that `send` returns — it is the
  first code anyone copies — its limitations section drops `XTGETTCAP` (0.5
  answers it) and gains the two bounds this release introduced: graphics are
  observed and offered but never rendered, and a reply the terminal's own
  input queue cannot hold may not arrive, undetectably so on Linux. The
  docs.rs landing page and `docs/DESIGN.md` §6 gained the observability
  counters, focus events, per-cell drag motion and `send_after`.
- **`SECURITY.md`'s resource bounds match the code again.** The reply queue is
  a 1 MiB byte cap rather than "a fixed depth", and the note now says why a
  depth bounds the wrong thing: two earlier versions counted slots and both
  shorted a well-behaved application, while a byte bound leaves the queue
  unbounded so the drain can never block on it.
- **What a large grid costs**, on `TerminalBuilder::size`: a snapshot holds
  one entry per cell and is rebuilt on every state change, so the cost is
  O(cells) and shape-independent, while repeat reads of an unchanged screen
  are cached and free. The table gives release *and* debug figures, because
  `cargo test` builds unoptimized by default and the two differ by 16-29x —
  the debug column is the one most suites actually see.

## [0.4.2] - 2026-08-19

The documentation set, brought up to what 0.4 actually does.

Two statements were wrong and the rest understated the crate by a
release or two. No library code changed.

*0.4.1 was tagged for exactly this content and never published: its
release run caught a latent race in this suite's own UTF-8 mouse test —
padding written after a click could be read by the script's exit guard
instead of by `head`, ending the child early so the next write failed
with EIO. Fixed before publishing, so the version on crates.io is the one
whose gates all passed.*

### Fixed

- **The README no longer contradicts itself about scrollback.** Its
  limitations section was still headed `(v0.3)` and still opened with "No
  scrollback assertions" — sixty lines below the paragraph explaining that
  scrollback is retained, 1000 rows by default. Since the README is the
  crates.io front page, the first thing a reader learned about 0.4's
  headline feature was that it did not exist. The section now states the
  bounds that actually hold: history is capped, text only and unreflowed;
  `wait_frame` needs the application to opt into DEC 2026 and retains eight
  frames; and the questions termlens declines to guess at are named.
- **`SECURITY.md` no longer claims the emulator runs with zero
  scrollback.** That sentence was the whole memory-bound argument in the
  resource-exhaustion note, and 0.4 made it false — the emulator is
  constructed with the configured history length. Every bound a child's
  output can reach is listed in its place: history length, retained frames,
  the read buffer, the `OSC 52` capture cap, the reply queue, and the
  diagnostics set.

### Documented

- **The crate-level docs describe 0.4, not 0.2.** The docs.rs landing page
  never mentioned `wait_frame`, retained scrollback, per-call deadlines or
  the clipboard accessor, so the crate's own front page understated it by
  two releases. It now names them, in the same breath as the guarantees
  that make them worth using.
- **`docs/DESIGN.md` §2 records the per-call deadlines.** The document that
  calls itself the contract for wait semantics had never mentioned the
  `_for` variants. It also now records the `wait_idle` timeout that names
  an unfinished frame instead of reporting silence against a quiet
  terminal.
- **`docs/HANDOFF.md` is marked as the historical v0.1 record it is**,
  rather than reading as a description of the project today — it described
  a private repository and an unfinished go-public checklist. The checklist
  keeps its original text, with the outcome recorded beneath it, including
  the one item resolved differently on purpose (required approvals stay at
  0: a solo maintainer cannot approve their own pull request).
- The announcement draft carried v0.1's limitations, two of which have
  since shipped; it now describes 0.4, and no longer claims to be untracked
  while sitting in the repository. The bug-report template no longer offers
  `0.1.0` as its example version.

No library code changed in this release.

## [0.4.0] - 2026-08-18

What termlens could not do, and where it quietly did the wrong thing.

Three gaps each made a whole category of subject untestable, and four
defects were found by adversarially probing the 0.3.0 release rather than
by reading its source — two of them undercutting the frame guarantee that
is this crate's headline.

### Changed

- **`wait_frame` and `wait_frame_for` return `Result<Screen>`** — the
  frame the predicate matched. Assert on that rather than on a later
  `screen()`, which can already be a newer state; the old shape let a
  test assert on one instant and read another. The dominant
  `t.wait_frame(..)?;` call form still compiles unchanged.
- **`wait_frame` no longer offers a frame twice.** Each call scans only
  frames newer than the one it last returned. A frame that satisfied a
  wait cannot satisfy the next, so N calls observe N distinct frames, a
  burst is observable in emission order (asking backwards now fails), and
  `send(key)` followed by `wait_frame(|s| s.contains(OLD_STATE))` times
  out instead of passing on the superseded frame while the assertion
  after it reads the old screen. A frame completed before the call but
  never yet returned still matches, deliberately: a fast application must
  not be able to slip one past two waits. `resize` advances the cursor
  too — a frame drawn at the old size is not the repaint that answers the
  new one.
- **`Style` gained the public fields `blink`, `conceal` and
  `strikethrough`**, so struct literals need updating
  (`..Style::default()` keeps working). `with_styles()` emits the new
  tokens in SGR order — `bold dim italic underline blink reverse conceal
  strikethrough` — which leaves an existing span's tokens unchanged
  unless the cell carries one of the three.
- **Scrollback is retained by default** (1000 rows;
  `TerminalBuilder::scrollback(0)` restores the old behaviour). Snapshots
  now carry history, which is invisible in the text rendering, so
  existing snapshot files stay valid.

### Added

- **Scrollback retention.** Content that scrolled off the top used to
  cease to exist, which ruled out every application that hands finished
  output *back* to the terminal — a pager, a log view, a TUI that commits
  completed blocks into native scrollback and keeps a small live region.
  `TerminalBuilder::scrollback(rows)` sizes the history, and `Screen`
  gained `scrollback_rows`, `scrollback_text` and `full_text` — history
  followed by the visible screen, which is the assertion an author
  actually writes when the application moves content between regions as
  it runs. Two limits are stated rather than papered over: history is
  bounded, and resize does not reflow. It costs nothing where unused: the
  alternate screen accumulates no history at all.
- **`Style::conceal`, `blink` and `strikethrough`.** `SGR 5`/`6`, `8` and
  `9` reached nothing, so three renderings collapsed into one value.
  Conceal was not a missing nicety but a trap: a test asserting that a
  password field is masked **passed against an application that printed
  the secret in clear**, and `with_styles()` could not break the tie
  either. That was the one failure mode in this crate where a green test
  certified the bug it was written to catch.
- **`OSC 52` clipboard capture.** `Screen::clipboard()` reports the most
  recent write — the decoded text and the target selections as the
  application named them — so "did it copy the right thing?" is
  answerable instead of resting on the application's own toast. An
  undecodable payload reports `None`, never `Some("")`: bad base64, bytes
  that are not UTF-8 and a payload past the capture bound are all
  distinct from a real write of nothing. Clipboard *reads* stay
  named-but-unanswered.

### Fixed

- **A stray `?2026l` no longer publishes a phantom frame.** The frame
  publisher fired on any End, whether or not a Begin was seen. The
  damaging case was not a false pass but a suppressed diagnosis:
  applications reset terminal modes defensively at startup and on crash,
  and such a string contains `?2026l`, so one stray End pushed the frame
  count off zero and replaced "the application never emitted a DEC 2026
  synchronized update — use `wait_until`" with a count implying the
  predicate was at fault. A frame is now one *completed* update; a
  Begin/End pair that changed nothing still counts, because the count is
  of repaints rather than of changes.
- **`DECRQM` no longer calls the mouse tracking modes "not recognized"
  when none is active.** The old answer set was self-contradictory —
  claiming the SGR mouse *encoding* while denying the tracking *modes*
  those reports come from — and it closed a loop on itself: an
  application doing ordinary probe-then-enable detection concluded the
  terminal had no mouse, never enabled tracking, and `click` then refused,
  blaming it for a decision termlens caused. With nothing tracking,
  nothing was collapsed and every tracking mode is genuinely reset. The
  ambiguous case — probing `1000` while `1002` is active — stays "not
  recognized", since the backend keeps only the last of a group.
- **`wait_idle` timeouts name an unfinished frame.** An application stuck
  inside an open synchronized update is silent, so it used to time out
  "waiting for 100ms of output silence", which reads as nonsense next to
  a quiet terminal. The message now says the application is inside an
  unfinished DEC 2026 update and that the screen below is a half-painted
  frame.
- Timeout messages from `wait_frame` carry the reason as well as the
  count: when every frame has already been returned, the message says the
  application has not repainted rather than implying the predicate is
  wrong. Pluralization fixed while there.

### Documented

- **A snapshot may be a half-painted frame**, including for an
  application that brackets every repaint in DEC 2026 exactly as
  intended: `wait_frame` is frame-gated, `screen()` is not. Now stated on
  `screen`, on `wait_until`'s third rule, and in `docs/DESIGN.md` §2 with
  the three routes to a frame-consistent read — one per way of waiting.
  The behaviour is deliberate: substituting the newest complete frame
  would let a `wait_until` predicate match content the following
  `screen()` does not show, and a torn read is what you want when
  diagnosing an application hung mid-repaint.
- **`wait_idle` will not declare idleness while a synchronized update is
  open** — it treats a begun-and-unfinished repaint the way it treats a
  half-received escape sequence. That is what makes the "settle before
  whole-screen snapshots" rule work, and it is now a stated guarantee
  rather than an implementation detail.

## [0.3.0] - 2026-08-17

The features the first real user's coverage study asked for, in the
order it ranked them, plus the terminal-query work that lets
capability-probing applications run against termlens unmodified.

### Changed

- `Scroll` gained `Left` and `Right` variants and is now
  `#[non_exhaustive]`. Exhaustive `match` on it needs a wildcard arm;
  marking it non-exhaustive means later additions won't break code
  again.

### Added

- Writes now respect the terminal's deadline. `send`, `send_str`,
  `paste`, `click` and `scroll` used to block indefinitely if the
  application stopped reading its input and the PTY buffer filled — the
  one place the crate's own "no unbounded waits" rule wasn't applied.
  They now fail at the deadline with the screen attached and a message
  naming the real cause, instead of hanging a CI job.
- A fuller mouse API: `click_with(button, col, row)` for middle and
  right buttons, `drag(button, from, to)`, modifier chords
  (`MouseButton::Left.ctrl()`, mirroring `Key::Right.ctrl()`), and
  horizontal wheel via `Scroll::Left` / `Scroll::Right`. Everything
  stays mode-aware: encoded for the tracking mode and encoding the
  application enabled, and refused with a typed error when the mode
  cannot express the gesture — a drag under X10, which reports no
  release, is an error rather than a misleading half-gesture.
- termlens answers **`DECRQM`** ("is private mode *n* set?"), so an
  application that probes before using synchronized output enables it
  against termlens — `wait_frame` works against programs nobody
  modified for the harness. Replies are truthful or absent: modes whose
  state the emulator holds exactly report set/reset, everything else
  reports "not recognized" rather than a guess. `DECRQSS`, `OSC 4`
  palette reads and `OSC 52` clipboard reads are now recognized too, so
  an application blocked on one is named in the timeout instead of
  hanging silently.
- `TerminalBuilder::foreground_rgb` configures the `OSC 10` answer,
  which was hardcoded white. Applications that pick a theme by
  comparing foreground and background luminance can now be tested
  against both.
- Every wait now takes a per-call deadline: `wait_frame_for`,
  `wait_idle_for` and `wait_exit_for` join `wait_until_for`. One
  known-slow step no longer forces the builder timeout up for every
  other wait in the suite — which is what made a genuinely stuck
  application burn the long timeout on its first failure. Timeout errors
  report the deadline that actually applied.
- `wait_frame` retains the **last 8 completed frames** and evaluates
  them oldest first, so a burst of frames arriving in a single read is
  observable step by step — a progress counter ticking `1`, `2`, `3` in
  one write used to be visible only at `3`. The retention bound and its
  two consequences (a longer burst drops its oldest frames; a retained
  frame stays matchable, so a predicate satisfied earlier resolves at
  once) are documented on `wait_frame` and in `docs/DESIGN.md` §2.

## [0.2.1] - 2026-08-12

Correctness patch. Every entry below was found by probing the published
0.2.0 rather than reading the source, and each one is a case where the
harness quietly did the wrong thing, panicked inside a dependency, or —
in the worst of them — hung itself.

### Changed

- `paste` now transforms the text the way a real terminal does, so what
  the application receives matches what a user pasting would produce.
  Line breaks become `\r` (the byte Enter sends; applications in raw
  mode never see `\n` from a terminal), and while bracketed paste is
  active, paste markers embedded in the text are removed — previously an
  `ESC[201~` inside the text ended the paste early and the remainder
  arrived as ordinary key presses. `send_str` remains the untransformed
  path.

### Fixed

- **The harness can no longer deadlock itself.** Query replies were
  written by the reader thread, so an application that emitted queries
  faster than it read the answers filled the PTY's input queue, blocked
  that write, and stopped the drain — after which the child blocked
  writing and neither side could proceed. Reproduced in the default
  configuration with no test input at all: the wait timed out with a
  stale screen and then `Drop` never returned. Replies now go to a
  dedicated responder thread, so the drain never writes; undeliverable
  replies are counted and reported ("the application is not reading its
  input") instead of stalling anything. `Drop`'s reap is bounded too —
  teardown must always terminate.
- Mouse reports now follow the **UTF-8 encoding** (mode 1005) when the
  application selects it. The encoding was collapsed to "SGR or not", so
  a 1005 application received the legacy form — identical below column
  95, and a bare non-UTF-8 byte past it, which such an application
  cannot decode.
- The unanswered-query diagnosis no longer misattributes unrelated
  failures. It was recorded once and never cleared, so a single
  deliberately-unanswered probe at startup (kitty's `CSI ? u` is the
  common one) claimed to be the cause of every later timeout. A query is
  now only blamed while the application has produced no output since
  asking; otherwise it is reported as context. Every unanswered query is
  named rather than just the most recent, the set is bounded, and
  `wait_frame` and the `Eof` errors carry the note too — previously
  `wait_frame` withheld it, which is the worst place for it to be
  missing, since an application blocked on a probe never reaches its
  first repaint.
- `wait_frame` timeouts embed the **live** screen, like every other
  wait. They previously embedded the last completed frame — which can be
  arbitrarily old — under a header saying "screen at timeout", so the
  one place a CI log is the only evidence showed the wrong screen. The
  count of observed frames is still in the message.
- A zero terminal dimension is now a typed `Error::Input` from `spawn`
  and `resize` instead of a panic inside the emulator. In release builds
  — the profile the stress workflow uses — it was worse than a panic:
  the arithmetic wrapped, both calls returned `Ok`, and the emulator
  panicked on the reader thread, silently killing the drain so every
  later snapshot was blank and a careless test went green.
- `current_dir` pointing at a path that is not an existing directory now
  fails the spawn instead of being **silently ignored**: the PTY layer
  falls back to the home directory, so a directory-sensitive test could
  pass against the wrong tree with no error anywhere.
- `spawn("")` now fails with a one-line `Error::Spawn` naming the problem
  instead of surfacing the PTY layer's entire `PATH` search. Genuine
  "program not found" failures keep their underlying diagnosis.

## [0.2.0] - 2026-08-11

### Added

- The three rules for race-free waits — one predicate, wait on the last
  thing painted, settle before whole-screen snapshots — and the resize
  **stale-frame trap** are now documented where you'll meet them: on
  `wait_until` and `resize` in the rustdoc, and in `docs/DESIGN.md` §2
  with the first real user's before/after failures.
- Process ergonomics: `TerminalBuilder::current_dir(dir)` runs the child
  in a chosen working directory (no more `cd … && …` through a shell);
  `Terminal::pid()` exposes the child's process id;
  `Terminal::signal(Signal::Term)` (Unix) delivers real signals so
  graceful-shutdown paths are testable — with a guard that refuses to
  signal an already-reaped pid, which the OS may have reused; and
  `wait_until_for(pred, timeout)` gives the one known-slow wait its own
  deadline instead of dragging the builder default up for every wait.
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
