---
name: termlens
description: Write, fix or review headless terminal tests for a Rust CLI or TUI (ratatui, crossterm, cursive, plain println) with the termlens crate — spawn the real binary in a PTY, wait on the rendered screen without sleeping, snapshot it with insta, assert on cells and styles. Use whenever a test spawns a terminal program, whenever a terminal test is flaky, sleeps or times out, and whenever someone asks how to test a TUI end to end.
---

# Testing terminal programs with termlens

Written against **termlens 0.9.0**. Every `rust` block below is a complete
integration test that is compiled against the crate in CI, so the API it
shows is the API that exists. The recipes spawn a binary called `myapp`
that draws a list with a `> ` highlight, a status line ending in
`Ready: j/k move, q quits`, and prints usage on `--help`; substitute your
application's own texts where the comments say so.

## 1. What termlens is, and when to reach for it

termlens spawns your **real binary** in a **real pseudo-terminal**, drains
its output on a reader thread through a VT emulator into an in-memory
**screen grid**, and lets a test wait on and assert against that grid —
Playwright for the terminal. Unix only (Linux, macOS).

Use it for the things an in-process mock cannot see:

- raw-mode and alternate-screen entry and exit, and whether the terminal is
  left broken after a panic or a `q`;
- what the user actually sees — box drawing, colours, wide characters,
  cursor position — after the bytes have been through a terminal;
- key, mouse, paste and resize handling as the terminal really encodes them
  under the modes the application enabled;
- a CLI's `--help`, its exit code, its behaviour when the terminal is 40
  columns wide;
- anything printed outside the framework: a stray `println!`, a logger, a
  panic message.

Keep using `ratatui::backend::TestBackend` (or plain unit tests) for widget
layout and rendering logic: it is faster and finer-grained. termlens is the
second layer — a small number of end-to-end flows through the real binary.

## 2. The model in sixty seconds

```text
your binary ──PTY──▶ reader thread ──▶ VT emulator ──▶ Screen (immutable snapshot)
                                                            ▲
your test ── send(Key) · click · paste · resize ──▶ PTY     └── wait_until / snapshot_after / wait_exit
```

- **A `Screen` is one consistent instant.** Every accessor on it reads the
  same snapshot; two `screen()` calls are two instants.
- **Every wait is deadline-bounded** (5 s by default) and **every failure
  embeds the screen**, so a timeout in CI shows what the application was
  displaying. There is no unbounded wait, on purpose.
- **Nothing sleeps.** The reader thread drains continuously; waits wake on
  new output. This is what makes the tests fast when green and readable
  when red.
- **Nothing is claimed that the emulator cannot see.** Terminal queries the
  application sends (cursor position, device attributes, mode probes) are
  answered truthfully or left unanswered and named in the next timeout.

## 3. Golden rules — read these before writing a test

1. **Never `thread::sleep`.** A sleep is either too short (flaky under CI
   load) or too long (slow every run), and it hides *what* you were waiting
   for. Wait on the screen instead: `wait_until(|s| …)` for a fact,
   `snapshot_after(|s| …)` for a fact followed by a whole-screen snapshot,
   `wait_stable(quiet)` for "the picture stopped changing". The single
   sanctioned delay is `send_after(delay, key)`, which exists because `Esc`
   followed immediately by another key is byte-identical to an `Alt` chord.

2. **Snapshot only a settled screen.** `wait_until(pred)` guarantees the
   bytes that made `pred` true were processed — and nothing more. A repaint
   has no end marker, so the predicate can fire on a half-painted screen,
   including half a row. Either wait on the **last** thing the application
   paints, or use `snapshot_after`, which waits for the predicate and then
   for the picture to hold still for 100 ms before handing you the screen.

3. **One predicate per instant.** Everything you assert about one moment
   goes into one closure: `wait_until(|s| s.contains("NORMAL") &&
   s.contains("Tasks 1/10"))`. `wait_until(a)` followed by
   `assert!(screen().b)` is a race between two instants.

4. **Spawn your own binary with `termlens::bin!("myapp")`.** It expands to
   the builder chain every test otherwise repeats — `size(80, 24)`,
   `env_clear()`, `timeout(5 s)`, `spawn(env!("CARGO_BIN_EXE_myapp"))` — and
   a misspelled name is a **compile error**, not a spawn failure at run
   time. Builder calls after the name override any default:
   `bin!("myapp", size(120, 40), env("NO_COLOR", "1"))`.

5. **Geometry is `(cols, rows)`, from 2 to 1000 per axis.** `size(0, 0)` and
   `size(1, 1)` are refused with `Error::Size`: one column panics the
   emulator on a double-width character and one row panics it on a line
   that wraps. Grids past 1000 per axis are refused because every snapshot
   costs one entry per cell. 80x24 is the default and is what you want.

6. **Two coordinate orders exist; do not mix them.** Everything that
   addresses a cell is **row-first**: `find` → `(row, col)`, `cell(row,
   col)`, `row_text(row)`, `cursor()` → `(row, col, visible)`. Everything
   that speaks of terminal geometry or a pointer is **column-first**:
   `size()` → `(cols, rows)`, `resize(cols, rows)`, `click(col, row)`,
   `scroll(col, row, …)`, `drag(button, (col, row), (col, row))`. Never
   pass a `find` result straight into `drag` — the tuple types match and
   the axes do not.

7. **Always finish the process.** Send the quit key, `wait_exit()?` and
   assert on the `ExitStatus` (`success()`, `code()`, `signal()`), then
   assert `!t.screen().alternate_screen()` so an application that leaves
   the user's terminal in the alternate screen fails the test. `Drop` kills
   and reaps whatever is left, so a failing test never leaks a process.

8. **`wait_frame` only works for applications that emit DEC 2026
   synchronized updates.** Stock ratatui 0.30 with crossterm does **not**
   (measured: `repaints()` stays 0), so `wait_frame` times out against it
   with a message saying exactly that. Default to `snapshot_after`. Use
   `wait_frame` only if the application brackets its repaints in
   `BeginSynchronizedUpdate` / `EndSynchronizedUpdate`.

9. **Return `termlens::Result<()>` from the test and use `?`.** The
   `Display` of every error carries the screen, so a failing wait prints
   the grid the application was showing instead of `called unwrap() on Err`.

10. **Snapshot the `Screen`, not its text.** `insta::assert_snapshot!(screen)`
    records the header (`size: 80x24  cursor: 3,5` or `cursor: hidden`) and
    the grid; `screen.with_styles()` adds a `styles:` block that catches a
    colour regression. `.text()` drops the header and `format!("{:?}")` is
    the same as `Display`. Review changes with `cargo insta review`; never
    blind-accept with `INSTA_UPDATE=always`.

11. **The environment is hermetic by default — set what the app reads.**
    Under `env_clear()` (which `bin!` applies) the child sees only
    `TERM=xterm-256color`, `SHELL=/bin/sh` and what you set with `env(…)`.
    No `HOME`, no `LANG`, no `COLORTERM`, no `NO_COLOR`, no `PATH` — so a
    bare program name cannot resolve (use an absolute path or `bin!`), and
    an application that checks `NO_COLOR` or `COLORTERM` needs them set
    explicitly for the case under test.

12. **The grid is Unicode-aware; think in cells.** A double-width character
    (CJK, most emoji) occupies two cells: the leading one `is_wide()`, the
    next `is_wide_continuation()`. `find` reports real terminal columns.
    `contains` and `find` fold both sides to NFC and search the **visible
    screen only** — text that scrolled off is in `full_text()`, and a line
    that wrapped is two rows, so a needle spanning the wrap is not found.

## 4. Setup

```toml
[dev-dependencies]
termlens = "0.9"
insta = "1"          # for the snapshot recipes; termlens also re-exports it as `termlens::insta`
```

- Put the tests in `tests/` **of the package that owns the `[[bin]]`**:
  Cargo sets `CARGO_BIN_EXE_<name>` only there, and `bin!` needs it at
  compile time. For a binary in a sibling crate, build it and pass the path
  to `Terminal::builder().spawn(path)` instead.
- The binary is built by `cargo test` before the tests run. Tests run in
  parallel by default; each spawns its own PTY, which is fine.
- Gate the test file with `#![cfg(unix)]` if the crate must also build on
  Windows.
- `add --features decode` only if you assert on the pixels of inline images.

## 5. Recipes

### Recipe A — hermetic CLI snapshot (`myapp --help`)

```rust
use termlens::Terminal;

#[test]
fn help_renders_and_exits_zero() -> termlens::Result<()> {
    // 80x24, cleared environment, 5 s deadline, compile-time-checked path.
    let mut t = termlens::bin!("myapp", args(["--help"]))?;

    // Wait for the LAST line of the help text before waiting for exit. A
    // program that prints and exits within a millisecond can, rarely and
    // under load on macOS, lose its tail to PTY teardown; waiting on the
    // tail first turns that into a loud timeout instead of a truncated
    // snapshot that passes.
    t.wait_until(|s| s.contains("q    quit"))?;   // your help's last line
    let status = t.wait_exit()?;
    assert!(status.success(), "exit status: {status}");

    // The header records the size and cursor; the body is the grid.
    insta::assert_snapshot!(t.screen());
    Ok(())
}

#[test]
fn a_bad_flag_is_reported_with_an_exit_code() -> termlens::Result<()> {
    let mut t = Terminal::builder()
        .size(80, 24)
        .env_clear()
        .timeout(std::time::Duration::from_secs(5))
        .args(["--definitely-not-a-flag"])
        .spawn(env!("CARGO_BIN_EXE_myapp"))?;
    // stderr lands on the same screen as stdout: it is one terminal.
    t.wait_until(|s| s.contains("unexpected argument"))?;
    let status = t.wait_exit()?;
    // Assert what your CLI promises; clap exits 2 on a usage error.
    assert_eq!(status.code(), Some(2), "status: {status}");
    assert_eq!(status.signal(), None, "exited, not killed: {status}");
    Ok(())
}
```

### Recipe B — interactive ratatui navigation and keystrokes

```rust
use termlens::Key;

#[test]
fn moving_the_highlight_and_quitting_cleanly() -> termlens::Result<()> {
    let mut t = termlens::bin!("myapp")?;

    // Predicate, then a 100 ms settle, then the screen: the safe sequence
    // for a whole-screen snapshot. Name the last thing the app paints.
    let first = t.snapshot_after(|s| s.contains("Ready"))?;
    assert!(first.alternate_screen(), "a TUI should be on the alternate screen:\n{first}");
    assert!(first.contains("> Alpha"), "{first}");
    insta::assert_snapshot!("initial_frame", first);

    // Send a key, then wait on what the key CHANGES — not on text that was
    // already true before the key, or the wait returns the old screen.
    t.send(Key::Char('j'))?;
    let moved = t.snapshot_after(|s| s.contains("> Beta"))?;
    assert!(!moved.contains("> Alpha"), "{moved}");

    // Arrow keys and chords encode as the terminal would (DECCKM-aware).
    t.send(Key::Down)?;
    t.wait_until(|s| s.contains("> Gamma"))?;
    t.send(Key::Up)?;
    t.wait_until(|s| s.contains("> Beta"))?;

    // Finish: quit, assert the exit, and assert the terminal was restored.
    t.send(Key::Char('q'))?;
    let status = t.wait_exit()?;
    assert!(status.success(), "status: {status}");
    assert!(!t.screen().alternate_screen(), "the app left the terminal in the alternate screen");
    Ok(())
}
```

If the flow needs `Esc` followed by another key, use the one sanctioned
delay — `t.send_after(Duration::from_millis(20), Key::Char('j'))?` — so the
application's read boundary falls between the two writes and it sees two
presses rather than one `Alt-j` chord.

### Recipe C — overriding the defaults (size, environment, deadline)

```rust
use std::time::Duration;
use termlens::Color;

#[test]
fn honours_no_color_at_a_custom_size() -> termlens::Result<()> {
    // Builder calls after the name override bin!'s defaults; the rest stay.
    let mut t = termlens::bin!(
        "myapp",
        size(100, 30),                       // (cols, rows)
        env("NO_COLOR", "1"),                // the app reads this at startup
        timeout(Duration::from_secs(10)),    // every wait's default deadline
    )?;
    let s = t.snapshot_after(|s| s.contains("Ready"))?;

    assert_eq!(s.size(), (100, 30), "{s}");

    // With NO_COLOR the title is drawn in the default colour, not cyan.
    let (row, col) = s.find("myapp").expect("title is on screen");
    let title = s.cell(row, col).expect("in range");
    assert_eq!(title.style().fg, Color::Default, "{}", s.with_styles());
    assert!(!title.style().bold);

    // One slow step gets its own deadline instead of a slower suite.
    t.wait_until_for(|s| s.contains("Ready"), Duration::from_secs(30))?;
    Ok(())
}
```

### Recipe D — targeted screen and style assertions

```rust
use termlens::Color;

#[test]
fn cells_styles_and_wide_characters() -> termlens::Result<()> {
    let mut t = termlens::bin!("myapp")?;
    let s = t.snapshot_after(|s| s.contains("Ready"))?;

    // Text: visible screen, NFC-folded, trailing padding never matched.
    assert!(s.contains("Alpha") && s.contains("Beta"), "{s}");
    assert_eq!(s.find("Ready"), Some((23, 0)), "status line sits on the last row: {s}");

    // Cells and styles: the highlighted row is drawn in reverse video.
    let (row, col) = s.find("> Alpha").expect("highlight");
    let cell = s.cell(row, col).expect("in range");
    assert!(cell.style().reverse, "{}", s.with_styles());
    // A coloured, bold title: ratatui's Cyan is ANSI colour 6.
    let (trow, tcol) = s.find("myapp").expect("title");
    let title = s.cell(trow, tcol).unwrap().style();
    assert_eq!((title.fg, title.bold), (Color::Indexed(6), true));

    // Find a cell by a property of the cell rather than by its text.
    assert_eq!(s.find_by(|c| c.style().reverse), Some((row, col)));

    // Wide characters: one glyph, two cells, real columns reported.
    let (crow, ccol) = s.find("東京").expect("CJK item");
    assert!(s.cell(crow, ccol).unwrap().is_wide());
    assert!(s.cell(crow, ccol + 1).unwrap().is_wide_continuation());
    assert_eq!(s.row_text(crow).trim_matches(['│', ' ']), "東京");

    // Regions and the cursor. rect_text is (cols, rows), like size().
    let list_pane = s.rect_text(0..20, 0..6);
    assert!(list_pane.contains("Gamma"), "{list_pane}");
    let (_, _, visible) = s.cursor();
    assert!(!visible, "a list view hides the cursor: {s}");

    t.send(termlens::Key::Char('q'))?;
    assert!(t.wait_exit()?.success());
    Ok(())
}
```

## 6. Reading a failure

Every error's `Display` ends with the screen, under a header that says
which screen it is (`--- screen at timeout ---`, `--- final screen ---`).
Read the first line for the cause:

| First line says | Meaning | Do |
|---|---|---|
| `timed out after 5s while waiting for the screen predicate to hold` | the predicate never became true | look at the embedded grid; the text is usually spelled differently, on another row, or scrolled off (the note says how many rows scrolled) |
| `… note: N rows have scrolled off the top` | the text went into history | assert with `full_text()` / `scrollback_text()` |
| `… note: the application queried the terminal (^[[?u …) and received no answer` | the app is blocked on a probe termlens deliberately does not answer | the app needs a fallback; see the termlens README's Known limitations |
| `terminal closed (EOF) while waiting for …` | the app exited before the predicate held | check `wait_exit()` first, or the app crashed — the final screen shows why |
| `the application never emitted a DEC 2026 synchronized update` | `wait_frame` against an app without synchronized output | use `snapshot_after` / `wait_until` (rule 8) |
| `input not receivable: the application has not enabled mouse tracking` | `click`/`drag`/`scroll` before the app enabled the mouse | `wait_until(|s| s.mouse_mode() != MouseMode::None)` first |
| `input not receivable: mouse at (50, 2) is outside the 20x5 grid` | coordinates swapped or out of range | rule 6 |
| `failed to spawn \`sh\`: \`sh\` is a bare program name and env_clear() removed PATH` | bare program name under `env_clear` | absolute path, `bin!`, or `.env("PATH", …)` |
| `invalid terminal size: a terminal needs at least 2 columns and 2 rows` | geometry below 2x2 (past 1000 has its own message) | rule 5 |
| `the terminal emulator failed and the screen stopped advancing` (`Error::Emulator`) | a bug in the emulation, not in your app | report it to termlens with the detail it names |

## 7. API cheat sheet

**Spawn** — `termlens::bin!("name" $(, method(args))*)` or
`Terminal::builder()`:

| Builder method | Meaning |
|---|---|
| `.size(cols, rows)` | 2..=1000 each; default 80x24 |
| `.timeout(Duration)` | default deadline for every wait (5 s) |
| `.arg(a)` / `.args([..])` | program arguments |
| `.env(k, v)` / `.envs([..])` / `.env_clear()` | environment; `env_clear` keeps `TERM` and `SHELL` pinned and drops the rest |
| `.current_dir(path)` | default: the test process's directory |
| `.scrollback(rows)` | history retained (default 1000, text only) |
| `.spawn(program) -> Result<Terminal>` | program is a path or a name on `PATH` |

**Wait** (all return `termlens::Result`, all embed the screen on failure, all have a `_for(…, timeout)` twin):

| Method | Returns | Use for |
|---|---|---|
| `wait_until(\|s\| bool)` | `()` | a fact about the screen |
| `snapshot_after(\|s\| bool)` | `Screen` | a fact, then a settled whole-screen snapshot |
| `wait_stable(quiet)` | `Screen` | the picture unchanged for `quiet`; bells and no-op repaints do not reset it |
| `wait_idle(quiet)` | `()` | no *bytes* for `quiet` — a weaker, older sibling of `wait_stable` |
| `wait_frame(\|s\| bool)` | `Screen` | complete DEC 2026 frames only (rule 8) |
| `wait_exit()` | `ExitStatus` | the child's exit; `success()`, `code() -> Option<u32>`, `signal() -> Option<&str>` |

**Drive**: `send(Key)`, `send_str("text")` (no Enter — send `Key::Enter`
yourself; `"\n"` would send LF, not CR), `paste("text")` (bracketed if the
app enabled it), `send_after(delay, Key)`, `click(col, row)`,
`click_with(MouseButton::Right, col, row)`, `drag(MouseButton::Left, (c, r),
(c, r))`, `scroll(col, row, Scroll::Down)`, `resize(cols, rows)`,
`focus_in()` / `focus_out()`, `signal(Signal::Term)` (Unix), `pid()`.

**Keys**: `Key::Char('j')`, `Enter`, `Esc`, `Tab`, `BackTab`, `Backspace`,
`Delete`, `Insert`, `Up`/`Down`/`Left`/`Right`, `Home`/`End`,
`PageUp`/`PageDown`, `F(1..=12)`, `Ctrl('c')`, `Alt('x')`; chords on any key:
`Key::Right.ctrl()`, `Key::F(5).ctrl().shift()`.

**Screen** (immutable; every accessor reads one instant):

| Accessor | Returns |
|---|---|
| `contains(&str)` / `find(&str)` | `bool` / `Option<(row, col)>` — visible grid, NFC-folded |
| `find_by(\|&Cell\| bool)` | `Option<(row, col)>` |
| `cell(row, col)` | `Option<&Cell>`: `contents()`, `style()`, `is_wide()`, `is_wide_continuation()` |
| `row_text(row)` / `text()` / `rect_text(cols, rows)` | `String` |
| `full_text()` / `scrollback_text()` / `scrollback_rows()` | history + screen / history / count |
| `size()` / `cols()` / `rows()` | `(cols, rows)` |
| `cursor()` | `(row, col, visible)`; `cursor_shape()`, `cursor_blink()` |
| `alternate_screen()`, `bracketed_paste()`, `application_cursor()`, `focus_events()` | mode flags |
| `mouse_mode()` / `mouse_modes()` | reporting protocol / the set the app enabled |
| `title()`, `clipboard()`, `links()`, `bells()`, `repaints()`, `graphics()` | out-of-band state |
| `with_styles()` | `Display` with a `styles:` block; snapshot this to catch colour regressions |

**Style** (`Copy`, public fields): `fg`, `bg` (`Color::Default` /
`Color::Indexed(u8)` / `Color::Rgb(u8, u8, u8)`), `bold`, `dim`, `italic`,
`underline`, `reverse`, `blink`, `conceal`, `strikethrough`. Overline and
double underline are not modelled.

**Errors** (`termlens::Error`, `#[non_exhaustive]`): `Timeout { waiting_for,
timeout, screen }`, `Eof { waiting_for, screen }`, `Spawn { command, reason }`,
`Size(String)`, `Input(String)`, `Write { what, screen }`, `Emulator {
detail, screen }`, `Pty(String)`, `Io(std::io::Error)`. `err.screen()` returns
the embedded screen when there is one.

## 8. Pitfalls an agent falls into, and the fix

| You are about to write | Write instead |
|---|---|
| `std::thread::sleep(Duration::from_millis(500)); let s = t.screen();` | `let s = t.snapshot_after(\|s\| s.contains("…"))?;` |
| `t.wait_until(\|s\| s.contains("title"))?; insta::assert_snapshot!(t.screen());` | `let s = t.snapshot_after(\|s\| s.contains("…last painted…"))?; insta::assert_snapshot!(s);` |
| `t.wait_until(a)?; assert!(t.screen().b);` | `t.wait_until(\|s\| a(s) && b(s))?;` |
| `.size(0, 0)` / `.size(1, 1)` | leave the 80x24 default, or `.size(cols, rows)` with both in 2..=1000 |
| `Terminal::builder().spawn("myapp")` | `termlens::bin!("myapp")?` — a name on `PATH` is not your binary, and under `env_clear` there is no `PATH` |
| `t.click(row, col)` / `t.drag(b, s.find("x").unwrap(), …)` | `t.click(col, row)`; destructure the `find` result and swap |
| `t.send_str("quit\n")` | `t.send_str("quit")?; t.send(Key::Enter)?;` |
| `t.wait_frame(…)` against a ratatui app | `t.snapshot_after(…)` unless the app emits synchronized updates |
| `t.click(3, 4)?` as the first thing after spawn | `t.wait_until(\|s\| s.mouse_mode() != MouseMode::None)?;` first |
| `.timeout(Duration::from_secs(60))` to stop a flake | find the race (rules 2 and 3); use `_for` on the one slow step |
| `insta::assert_snapshot!(t.screen().text())` | `insta::assert_snapshot!(t.screen())` or `.with_styles()` |
| `assert!(t.screen().contains("done"))` after the app printed a lot | `t.screen().full_text().contains("done")` — it scrolled |
| `.unwrap()` everywhere in a `fn test()` | `-> termlens::Result<()>` and `?`, so the failure prints the screen |
| a test that never quits the app | send the quit key, `wait_exit()?`, assert `!alternate_screen()` |
| `INSTA_UPDATE=always cargo test` | `cargo insta review`, and read every diff |

## 9. Pairing with insta

- `insta::assert_snapshot!(screen)` — text grid with header. Stable across
  runs as long as the application draws nothing volatile.
- `insta::assert_snapshot!(screen.with_styles())` — adds `styles:` runs like
  `0: 1-5 fg=6 bold`; use it where a colour or a highlight is the point.
- `insta::assert_snapshot!("name", screen)` — several snapshots in one test.
- Inline snapshots work: `insta::assert_snapshot!(screen, @"")`, then
  `cargo insta review` fills the literal.
- `termlens::assert_screen_snapshot!(screen)` is the same call through the
  `insta` termlens re-exports, for crates that do not want their own `insta`
  dev-dependency.
- Volatile content (a clock, a PID, a spinner) breaks whole-screen
  snapshots. insta's text filters are not grid-aware — a shorter replacement
  shifts every column after it — so prefer asserting the stable region with
  `rect_text(cols, rows)` and the volatile field with `contains`/`find`,
  and snapshot the whole screen only when nothing on it moves.
- Snapshot files live in `tests/snapshots/`; commit them. Review every
  change with `cargo insta review`; a diff you cannot explain is a bug.

## 10. A checklist before you finish

- [ ] No `sleep` anywhere; every wait names what it waits for.
- [ ] Every whole-screen snapshot comes from `snapshot_after` or a wait on the last-painted text.
- [ ] Each test quits the app, asserts the exit status, and asserts `!alternate_screen()`.
- [ ] Coordinates: `(row, col)` from `find`/`cell`, `(col, row)` into `click`/`size`.
- [ ] Environment set explicitly for anything the app reads; `bin!` used for own binaries.
- [ ] Tests return `termlens::Result<()>`; snapshots reviewed with `cargo insta review`.
