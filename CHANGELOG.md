# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Until 1.0, minor versions (0.x) may contain breaking changes; they are always
listed under a **Changed** or **Removed** heading.

## [Unreleased]

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
  completed, as of this observation. It counts *repaints, not changes*, so a
  Begin/End pair that drew nothing still counts, which is exactly the
  property an amplification test needs: "one wheel notch produced four
  repaints" is invisible to every content predicate, because each
  intermediate frame shows correct content.
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
