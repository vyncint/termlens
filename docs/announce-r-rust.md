# Announcement draft — users.rust-lang.org

Post at https://users.rust-lang.org → log in with GitHub → **New Topic** →
category **announcements** → paste title and body below. Once it is posted,
delete this file (through a PR — `main` is protected). It is tracked, but
deliberately not linked from anywhere.

The same text also works verbatim on the ratatui forum
(https://forum.ratatui.rs) and as a #rustlang post elsewhere.

---

**Title:**

termlens: test what your TUI actually renders — real PTY, emulated screen, insta snapshots

**Body:**

I kept wanting Playwright-style tests for terminal apps: spawn the real
binary, look at what a user would *see*, assert on that. Stream matchers
(rexpect/expectrl) can't answer "is the cursor on the third menu item?",
and in-process backends like ratatui's `TestBackend` skip your real
binary and the PTY layer entirely. So I built termlens:

```rust
let mut t = Terminal::builder()
    .size(80, 24)
    .env_clear()                      // hermetic: no host env leaks in
    .spawn(env!("CARGO_BIN_EXE_myapp"))?;

t.wait_until(|screen| screen.contains("Ready"))?;
insta::assert_snapshot!(t.screen()); // snapshot the rendered grid

t.send(Key::Char('q'))?;
assert!(t.wait_exit()?.success());
```

How it works: your app runs in a real kernel PTY; a reader thread drains
it continuously through a vt100 emulator into an immutable screen grid;
waits are all deadline-bounded, and a timeout error embeds the full
screen dump — so a CI log alone shows what the app was displaying.

Three things worth pointing at beyond the basics. If your app brackets its
repaints in DEC 2026 synchronized updates (crossterm's
`BeginSynchronizedUpdate`), `wait_frame` evaluates predicates only on
complete frames and hands back the one it matched — never a torn screen,
and each call observes a frame no earlier call did, so a burst is
assertable step by step. Apps that *probe* for the mode before using it
get a truthful `DECRQM` answer, so they enable it against termlens
unmodified. And the style model carries blink, conceal and strikethrough,
which is what stops a test asserting "the password field is masked" from
passing against an app that printed the secret in clear — the two are
identical text.

And behaviour that leaves the screen *identical* is assertable, which no
content predicate can manage: a repaint that drew nothing, a bell on a
rejected key, an inline image. Every `Screen` carries the counters, so
"one wheel notch became four repaints" is a test, and so is "this diagram
must render as box art and never go out as a Kitty image".

The same idea covers two things a modern TUI does that render as ordinary
text. An `OSC 8` hyperlink puts its URL nowhere on the screen, so a test
asserting your app linked the right issue used to pass just as happily
against one that emitted no link at all — `links()` reports each span with
its target and the text it wrapped. And `cursor_shape()` reports what the
app asked `DECSCUSR` for, so "the indicator says INSERT" and "the terminal
was actually put into insert" stop being the same assertion — and a program
that switches the cursor and forgets to switch it back is catchable.

The part I'm proudest of is the flake story. CI runs the whole suite 100
times on Linux **and** macOS before anything merges to the wait paths.
That gate caught, among other things, a genuine macOS kernel race where
pty teardown (`revoke()`) can hang up a *sibling thread's freshly opened
pty* because device numbers recycle instantly — termlens serializes pty
lifecycle edges behind a process-wide lock so your parallel tests don't
hit it.

Honest limitations (v0.8): Unix only for now (portable-pty supports
ConPTY, so Windows is planned); scrollback is retained but bounded, text
only, and not reflowed on resize; `wait_frame` needs the app to opt into
DEC 2026; inline graphics are captured and decodable but still never
rendered, so an image never reaches the screen grid and how a picture sits
over the text under it is not assertable; hyperlinks are captured but not
attributed to cells, so which cells sit inside a span is not assertable;
and a few questions (kitty
`CSI ? u`, DECRQSS, `OSC 52` *reads*) are deliberately left unanswered
rather than guessed — an app blocked on one is named in the next timeout
instead of hanging silently.

- crates.io: https://crates.io/crates/termlens (`cargo add termlens --dev`)
- GitHub: https://github.com/vyncint/termlens
- Design notes (wait semantics, snapshot format spec, the macOS pty war
  stories): https://github.com/vyncint/termlens/blob/main/docs/DESIGN.md

Feedback very welcome — especially from anyone testing ratatui apps in
CI today.
