# Working on termlens

Instructions for coding agents — and useful to humans. **termlens** is a headless PTY test harness: spawn a terminal program in a real pseudo-terminal, render its output with a VT emulator, and assert on the screen grid a user would see.

This file is the canonical brief; `CLAUDE.md` points here. `CONTRIBUTING.md`
is the full contributor document and wins wherever the two disagree.

## Layout

- `crates/termlens/` — the published library. `terminal.rs` is the runtime,
  `screen.rs` the snapshot types, `emu/` the VT emulation behind a small
  internal trait, `graphics.rs` the inline-image capture and decoder.
- `fixtures/` — deterministic terminal programs the integration suite spawns
  in real PTYs. Never published; `publish = false`.
- `docs/DESIGN.md` — four layers, the wait-semantics contract, the snapshot
  format. **Read it before touching `wait.rs`, `terminal.rs`, or `emu/`.**

## Build and test

```sh
cargo test --workspace --all-features   # the whole suite
cargo test -p termlens --test graphics  # one integration file
cargo insta review                      # snapshots, after an intended change
```

Stable toolchain, MSRV 1.85 (checked in CI against the committed lockfile).

## Things that will bite you here

- **Wait and timing changes must pass the stress workflow.** `stress.yml`
  runs the suite many times over, split across five machines at different
  `--test-threads`. It has found real bugs on its first run more than once.
  Trigger it on your branch before asking for review.
- **Never blind-accept a snapshot.** `cargo insta review` and look at every
  diff. A snapshot change you cannot explain is a bug report.
- **A `Screen` is embedded in every error**, so its size is load-bearing.
  Adding a field is not free.
- **Nothing is claimed that the emulator cannot render.** A query is answered
  truthfully or left unanswered and named in the next timeout. Do not invent
  a reply to make a test pass.

## The rules that will fail CI

Three, and they are the same in every one of these repositories.

1. **Conventional Commits.** `feat:`, `fix:`, `docs:`, `test:`, `ci:`,
   `chore:`, `refactor:`, `perf:` — imperative mood, subject line under 72
   characters, scope optional (`fix(screen): …`).
2. **DCO sign-off.** `git commit -s`, and the `Signed-off-by:` email must
   match the commit author's. Forgot? `git commit --amend -s --no-edit`, or
   `git rebase --signoff main` for a branch.
3. **No AI attribution.** See below — this one is about you, and it is the
   rule most likely to catch an agent out.

Run them yourself before pushing; both scripts take a commit range:

```sh
.github/scripts/check-dco.sh main..HEAD
.github/scripts/check-no-ai-attribution.sh main..HEAD
```

## Using AI here

**You are welcome.** Every one of these projects was built with AI assistance
and says so in its CONTRIBUTING. Use whatever helps.

**You are not a contributor.** Do not add yourself to the history:

- no `Co-Authored-By:` trailer naming an assistant, a model, or a vendor,
- no "Generated with …" footer, no robot emoji,
- no bot account as author or committer — save a named dependency bot, which
  is exempt from the identity rule only and still checked on its message.

The human who opens the pull request is the author of record and takes
responsibility for the change under the DCO. That is what the sign-off
certifies, and it cannot be certified by a tool. `.claude/settings.json`
turns co-author trailers off for agents that read it; the check in CI is the
boundary, and it reads every commit in the range.

If CI catches one, the fix is to rewrite the message, not to argue with it:

```sh
git commit --amend            # the last commit
git rebase -i main            # several, marking each `reword`
git push --force-with-lease
```

## What good work looks like here

These repositories share a house style, and it is stricter than most:

- **Evidence over assertion.** A bug report says what was measured against
  which released version. "Reproduced against 0.4.0" is the standard; "the
  code looks wrong" is not. Issues in these repos read *Today / Why it is
  worth fixing / Fix / Done when*, with a concrete reproduction.
- **Every change lands with a test**, and the test must be able to fail. If
  you add a guard, prove it catches the thing — break it once and watch it go
  red before you commit.
- **Comments say *why*, never *what*.** The diff shows what. A comment earns
  its place by recording the reason, the alternative rejected, or the failure
  that motivated the line.
- **Say what you did not do.** A pull request that lists what it left out and
  why is worth more than one that implies completeness. If something is
  unverified, say so — an honest gap is cheap and a false claim is expensive.
- **Documentation is checked, not maintained.** Where a README states a fact
  the code owns, there is usually a test asserting the two agree. Do not
  break that pattern by hand-editing the doc.

## Pull requests

Branch from `main` (`feat/…`, `fix/…`, `docs/…`, `ci/…`). PRs are
**squash-merged**, so the PR title becomes the commit subject on `main` —
write it as a Conventional Commit. Update `CHANGELOG.md` under
`[Unreleased]` for anything user-facing.

Direct pushes to `main` are blocked by a ruleset; everything goes through a
pull request, including releases.

## Releasing

Tag `vX.Y.Z` on `main`; `release.yml` gates, publishes via Trusted Publishing, and cuts the GitHub Release. See `docs/RELEASING.md`.
