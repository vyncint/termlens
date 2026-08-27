# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Until 1.0, minor versions (0.x) may contain breaking changes; they are always
listed under a **Changed** or **Removed** heading.

## [Unreleased]

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
