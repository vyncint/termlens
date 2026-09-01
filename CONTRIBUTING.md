# Contributing to termlens

Thanks for your interest! This document covers everything you need to get a
change from your editor into `main`.

> **These four projects share one contributor pattern** — the same commit
> rules, the same DCO, the same AI policy, the same CI and release shape:
> [termlens](https://github.com/vyncint/termlens),
> [mossaic](https://github.com/vyncint/mossaic),
> [launchbound](https://github.com/vyncint/launchbound),
> [reconverge](https://github.com/vyncint/reconverge). Learn it once.

## 1. Dev setup

```sh
git clone https://github.com/vyncint/termlens
cd termlens
cargo test --workspace --all-features   # the whole suite: unit + integration + doctests, `decode` included
```

`--all-features` matters: `decode` is off by default, and without it sixteen
tests never build. That is the `test` job. CI gates on more than the test
job, and every gate is reproducible locally — run these before pushing and
nothing in CI should surprise you:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo clippy --workspace --all-targets --no-default-features --features decode -- -D warnings
cargo test --workspace --no-default-features
cargo test --workspace --no-default-features --features decode
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features
cargo deny check                          # cargo install cargo-deny
cargo +1.85 check --workspace --locked    # the MSRV: `rust-version` in Cargo.toml
```

The list is written out here rather than left as a pointer because a first
PR that fails on `cargo fmt` after following the setup to the letter is an
avoidable bad first experience. If it ever disagrees with
[`.github/workflows/ci.yml`](.github/workflows/ci.yml), the workflow is the
source of truth and this list is the bug.

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
`feat:`, `fix:`, `docs:`, `test:`, `ci:`, `chore:`, `refactor:`, `perf:` —
scope optional (`feat(screen): …`). Subject line: imperative mood,
≤ 72 characters.

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

One exception, and it is GitHub's rather than ours: a pull request
**squash-merged through the web UI** has its author email rewritten by GitHub
*after* the sign-off was written, so an exact match is impossible by
construction. Such a commit must carry a sign-off, but is not matched against
an author it did not choose. The commits that went into the PR were already
checked, address and all, on the branch.

GitHub also *writes* that message, and it drops the trailers of the commits it
squashed whenever the branch contained a merge commit — pressing **Update
branch** is enough to cause it. The merge then lands on main carrying no
sign-off, and main is linear and non-fast-forward, so it cannot be repaired.
The check therefore exempts exactly one commit — the tip of a push to main,
which can only get there through a pull request that was already checked
strictly. **Keep your branch up to date by rebasing, not merging:**

```sh
git fetch origin && git rebase origin/main
git push --force-with-lease
```

That also matches what main requires: linear history, so a merge commit on
your branch is only ever going to be squashed away.

## 6. AI tooling policy

**AI assistance is welcome here — use whatever helps.** Every one of these
projects was built with it. There is an [AGENTS.md](AGENTS.md) briefing coding
agents on the layout, the commands, and the house style.

**AI attribution is not welcome.** No `Co-Authored-By` trailer naming an
assistant, model or vendor; no "Generated with …" footer; no robot emoji; no
bot identity as author or committer, save the one carve-out below. Whoever
opens the pull request is the author of record, takes responsibility under the
DCO, and the history should say so — a tool cannot certify the DCO, which is
the whole point of it.

This is enforced, not requested: `commit-policy.yml` runs
[`check-no-ai-attribution.sh`](.github/scripts/check-no-ai-attribution.sh) and
[`check-dco.sh`](.github/scripts/check-dco.sh) over every commit in a pull
request. Run them yourself first — both take a range:

```sh
.github/scripts/check-dco.sh main..HEAD
.github/scripts/check-no-ai-attribution.sh main..HEAD
```

If a check fails, rewrite the message rather than arguing with it:

```sh
git commit --amend            # the last commit
git rebase -i main            # several, marking each `reword`
git push --force-with-lease
```

`.claude/settings.json` turns co-author trailers off for agents that read
repository settings. That is a courtesy; the check in CI is the boundary.
Contributions authored *by* an autonomous account are not accepted.

**One carve-out: a named dependency bot.** Dependabot is exempt from the
*identity* half of the check and from nothing else. The rule exists so that a
human is not displaced as the author of record, and a version bump has no
human to displace — it is not somebody's work with the credit misassigned. The
message rules still apply in full, so a bot cannot carry an AI co-author
trailer, a "Generated with" footer or a robot emoji past the check either.
Adding another bot means naming it in `check-no-ai-attribution.sh`: the
allowlist is a list on purpose, so that widening it is a visible decision.

## 7. PR flow

- Branch from `main`; name branches `feat/…`, `fix/…`, `docs/…`, `ci/…`.
- PRs are **squash-merged** — keep the PR title in Conventional Commit form,
  since it becomes the commit subject on `main`. Branches are deleted on merge.
- Required checks: `required-green` (fmt, clippy, tests on Ubuntu + macOS, MSRV, docs, cargo-deny, zizmor), plus `commit-policy` (DCO + attribution). All
  must pass before merge; direct pushes to `main` are blocked by a ruleset.
- **Every change lands with a test, and the test must be able to fail.** If
  you add a guard, break it once and watch it go red before you commit.
- **Say what you did not do.** A PR that lists what it left out and why is
  worth more than one implying completeness. An honest gap is cheap; a false
  claim is expensive.
- **Contributing from a fork?** Two things are normal. On your first PR the
  workflows wait for a maintainer to approve them — GitHub's standard
  first-time-contributor safeguard, nothing you did wrong. And when
  `commit-policy` fails on a fork PR it cannot post its explanatory comment
  (fork PRs get a read-only token); the job log carries the full explanation,
  including the offending commit and the command that fixes it.
- Review: expect actionable review within a few days. Small, focused PRs get
  reviewed faster. Update `CHANGELOG.md` under `[Unreleased]` for any
  user-facing change.

## 8. Release process

Releases are cut by maintainers only; the checklist lives in
[docs/RELEASING.md](docs/RELEASING.md).
