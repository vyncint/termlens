# Announcement draft — users.rust-lang.org

Post at https://users.rust-lang.org → log in with GitHub → **New Topic** →
category **announcements** → paste title and body below. Delete this file
after posting (it is deliberately untracked and not linked from anywhere).

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

t.send(Key::Char('q'));
assert!(t.wait_exit()?.success());
```

How it works: your app runs in a real kernel PTY; a reader thread drains
it continuously through a vt100 emulator into an immutable screen grid;
waits are all deadline-bounded, and a timeout error embeds the full
screen dump — so a CI log alone shows what the app was displaying.

The part I'm proudest of is the flake story. CI runs the whole suite 100
times on Linux **and** macOS before anything merges to the wait paths.
That gate caught, among other things, a genuine macOS kernel race where
pty teardown (`revoke()`) can hang up a *sibling thread's freshly opened
pty* because device numbers recycle instantly — termlens serializes pty
lifecycle edges behind a process-wide lock so your parallel tests don't
hit it.

Honest limitations (v0.1): Unix only for now (portable-pty supports
ConPTY, so Windows is planned), no scrollback assertions, and styles are
captured per-cell but not yet part of the text snapshot format.

- crates.io: https://crates.io/crates/termlens (`cargo add termlens --dev`)
- GitHub: https://github.com/vyncint/termlens
- Design notes (wait semantics, snapshot format spec, the macOS pty war
  stories): https://github.com/vyncint/termlens/blob/main/docs/DESIGN.md

Feedback very welcome — especially from anyone testing ratatui apps in
CI today.
