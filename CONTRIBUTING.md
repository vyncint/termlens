# Contributing to termlens

Thanks for your interest! This document covers everything you need to get a
change from your editor into `main`.

## 1. Dev setup

```sh
git clone https://github.com/vyncint/termlens
cd termlens
cargo test --workspace        # full suite: unit + integration + doctests
```

The integration tests spawn real PTYs and small fixture apps from
`fixtures/`; there is nothing to install beyond a Rust toolchain
(`rust-toolchain.toml` pins stable). Snapshot tests use
[insta](https://insta.rs) — when a snapshot changes:

```sh
cargo install cargo-insta     # once
cargo insta review            # inspect and accept/reject each diff
```

## 2. Project layout

- `crates/termlens/` — the published library (PTY spawn → VT emulation →
  `Screen` snapshots → wait engine).
- `fixtures/` — deterministic terminal apps the integration suite drives;
  workspace members, never published.
- `docs/DESIGN.md` — the architecture in four layers, wait semantics, and the
  snapshot format spec. **Read this before touching `wait.rs`, `terminal.rs`,
  or the emulator.**
- `.github/` — CI, commit policy enforcement, release automation.

## 3. Testing policy

- Every feature or bug fix lands with tests. No exceptions; a test harness
  that is itself untested is a liability.
- Anything touching wait semantics or timing (`wait.rs`, the reader thread,
  `wait_idle`) must pass the **stress workflow** (`stress.yml` — the suite in
  a 100-iteration loop on Ubuntu and macOS). Trigger it from the Actions tab
  on your branch, or ask a maintainer to.
- Snapshot updates must be **reviewed diffs**: run `cargo insta review` and
  look at every change. Never blind-accept with `cargo insta accept` or
  `INSTA_UPDATE=always`. A snapshot diff you can't explain is a bug report.

## 4. Commit conventions

We use [Conventional Commits](https://www.conventionalcommits.org/):
`feat:`, `fix:`, `docs:`, `test:`, `ci:`, `chore:`, `refactor:` — scope
optional (`feat(screen): …`). Subject line: imperative mood, ≤ 72 characters.

## 5. Developer Certificate of Origin (DCO)

Every commit must be signed off:

```sh
git commit -s
```

This appends `Signed-off-by: Your Name <you@example.com>` and certifies you
wrote the change or otherwise have the right to submit it under the project
license — the [Developer Certificate of Origin](https://developercertificate.org),
the same lightweight model the Linux kernel uses. The sign-off email must
match the commit author email; CI enforces this on every commit in a PR.

**There is no CLA. DCO only.** You keep your copyright.

Forgot to sign off? `git commit --amend -s` for the last commit, or
`git rebase --signoff main` for a whole branch, then force-push.

## 6. AI tooling policy

You may use any AI tool to write code for this project — we do too. Two
rules:

1. **You are the author of record** and take full responsibility for what you
   submit, per the DCO.
2. **Do not add AI co-author trailers, "Generated with" footers, or bot
   identities to commits.** CI rejects them.

Contributions authored *by* an autonomous account (bot committer/author) are
not accepted.

## 7. PR flow

- Branch from `main`; name branches `feat/…`, `fix/…`, `docs/…`, etc.
- PRs are **squash-merged** — keep the PR title in Conventional Commit form,
  since it becomes the commit subject on `main`. Branches are deleted on
  merge.
- Required checks: `required-green` (fmt, clippy, tests on Ubuntu + macOS,
  MSRV, docs, cargo-deny) and `commit-policy` (DCO + attribution policy).
  All must pass before merge.
- Review: expect actionable review within a few days. Small, focused PRs get
  reviewed faster than large ones. Update `CHANGELOG.md` under
  `[Unreleased]` for any user-facing change.

## 8. Release process

Releases are cut by maintainers only; the checklist lives in
[docs/RELEASING.md](docs/RELEASING.md).
