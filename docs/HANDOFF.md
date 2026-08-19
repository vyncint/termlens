# termlens (née termtest) — Build Handoff Report

> **Status: historical record of the v0.1 build.** Everything below describes
> the project as it stood on 2026-08-09 and is kept as the record of that
> work, not as a description of termlens today. Since then the repository has
> been made public, the crate has been published, and the API has grown
> through 0.2, 0.3 and 0.4 — `CHANGELOG.md` says what each release changed,
> and `README.md` plus `docs/DESIGN.md` describe the current design. The
> go-public checklist in §3 has its outcome recorded beneath it.

Date: 2026-08-09. Repo: <https://github.com/vyncint/termlens> (private at
the time; public since).
Everything below was verified against live GitHub state, not assumed.
"The build brief" below means the commissioning document this build
executed. It is **not** part of the repository, so wherever a requirement
of it matters, the requirement is restated inline.

## 1. What was built

A complete, release-ready v0.1 of `termlens` — a headless PTY test harness
(real PTY → vt100 emulation → `Screen` snapshots → deadline-bounded waits),
as commissioned by the build brief. Highlights:

- **Library** (`crates/termlens`): `Terminal`/`TerminalBuilder` (size,
  strict env control, default deadline), `Screen`/`Cell`/`Style`/`Color`
  value types with the documented snapshot `Display` format, `Key` with
  xterm encodings, `wait_until` / `wait_idle` / `wait_exit` (every error
  embeds the screen dump), `resize` (TIOCSWINSZ + SIGWINCH), zombie-free
  `Drop`, emulator behind an internal trait, `insta` feature (default) with
  re-export + `assert_screen_snapshot!`.
- **Fixtures** (`fixtures/`, publish = false): `hello-tui`, `form-echo`,
  `resize-echo`, `unicode-torture` — all deterministic by construction.
- **Tests**: 48 green (29 unit, 10 `basic.rs` against `/bin/sh`, 5 fixture
  tests incl. three reviewed insta snapshots, 4 doctests). The form-echo
  test round-trips every special-key encoding through crossterm's parser.
- **Docs**: README (the project's storefront), `docs/DESIGN.md` (4 layers,
  wait-semantics contract, snapshot format spec, emulator rationale),
  `docs/RELEASING.md`, CONTRIBUTING/SECURITY/MAINTAINERS/CoC/CHANGELOG.
- **Automation**: `ci.yml` (fmt, clippy `-D warnings`, test matrix
  ubuntu+macos, MSRV read from Cargo.toml, rustdoc `-D warnings`,
  cargo-deny, `required-green` fan-in), `commit-policy.yml` (DCO +
  no-AI-attribution, scripts under `.github/scripts/`), `stress.yml`
  (suite ×100 on both OSes), `release.yml` (tag/version guard → full CI
  via `workflow_call` → semver-checks with graceful first-release skip →
  Trusted-Publishing publish → CHANGELOG-extracted GitHub release).
  All workflows pass `actionlint`.

**Commits**: built as 20 commits on `main` (21 with this report), every
one Conventional, DCO-signed-off by `Vyncint Ng <vyncint@icloud.com>`,
zero AI attribution — verified by running both policy scripts over the
full history locally *and* by the `commit-policy` runs on every push.
*(History note: the development history was squashed to a single commit
before the repository was made public; the run and pull-request links in
this report remain live as the original evidence.)*

**Actions run links** (verified):

| What | Run |
| --- | --- |
| First full-green CI on `main` (all 8 jobs, first push) | <https://github.com/vyncint/termlens/actions/runs/31297033139> |
| `commit-policy` green over all pushed commits | <https://github.com/vyncint/termlens/actions/runs/31297033128> |
| **Negative test: `commit-policy` red** on a crafted bad commit (PR #2, since closed, branch deleted). Log shows all five violation classes caught: missing DCO, bot co-author trailer, "Generated with" watermark, bot author + committer identity. The one-time PR comment fired with the policy line. | <https://github.com/vyncint/termlens/actions/runs/31297145908> |
| Stress ×100 — run 1: macos green, ubuntu red (cold-start: prebuild missed fixture bins → compiled mid-iteration; fixed in PR #4) | <https://github.com/vyncint/termlens/actions/runs/31297100536> |
| Stress ×100 — run 2: **both OSes red**, and the failures were gold: they exposed a real instant-exit pty race (see below; fixed in PR #5) | <https://github.com/vyncint/termlens/actions/runs/31297578368> |
| Stress ×100 — run 3: red again, one level deeper — a snapshot test waited on a midway marker and raced the trailing newline at a chunk boundary (fixed in PR #6; run cancelled once diagnosed) | <https://github.com/vyncint/termlens/actions/runs/31298405233> |
| Stress ×100 — run 4: **ubuntu 100/100 green for the first time**; macOS died at iteration 41 with the new forensics pointing at a kernel-level pty race (fixed in PR #7) | <https://github.com/vyncint/termlens/actions/runs/31298690497> |
| Stress ×100 — final run after PRs #5–#7, both OSes green | <https://github.com/vyncint/termlens/actions/runs/31299282165> |

Real-world findings the pipeline caught (working exactly as designed):

- The **stress workflow found a genuine platform race on day one**: a
  child that writes and exits within its first milliseconds can lose
  output to the OS pty teardown (macOS especially), or die "by signal"
  instead of reporting its exit code — at roughly 1 in 80 instant-exit
  spawns under load, reproduced locally at ~1/10 full-suite runs. Fixes
  (PR #5): the reader thread now attaches *before* the child spawns;
  `ExitStatus` reports terminating signals by name; `Screen`'s `Debug` is
  compact (the derived one produced single log lines so large that CI
  dropped them — which is why the first failures looked outputless); and
  every instant-exit script in the suite now ends with a stdin `read`
  guard, released only after its output is asserted. The residual
  kernel-side caveat is documented in `docs/DESIGN.md` §2 and the README.
  Validation: 30/30 local release-mode full-suite iterations clean
  (previously ~1 failure per 10), then the green CI run above.
- Stress run 3 then found the **next flake one level deeper**: a snapshot
  test waited on `contains("done")` and snapshotted — but `wait_until`
  only guarantees bytes *up to the marker*, so the trailing newline could
  still be in flight and the snapshot caught the cursor mid-line. Rule
  (PR #6, documented in `docs/DESIGN.md` §2): before snapshotting, wait on
  the **last** thing the app draws — the frame's bottom corner or the
  cursor's resting position — never a midway line. Another 25/25 local
  iterations clean on top of the earlier 30/30.
- Stress run 4 (ubuntu 100/100) surfaced the deepest one: on macOS a
  freshly-spawned child died at birth (EIO on the first write, blank
  screen) at iteration 41. Root cause: **macOS tears ptys down with
  `revoke()` and recycles pty device numbers immediately**, so a
  concurrent Terminal teardown can revoke a sibling thread's just-opened
  pty and hang up its brand-new session. Fix (PR #7): the library
  serializes every pty lifecycle edge (open+spawn / kill+reap+close)
  behind a process-wide lock — microseconds per edge, steady-state I/O
  untouched. This one is a genuine library improvement for every consumer
  running tests in parallel, and the stress workflow is the only reason
  it was found before users hit it.
- The **first squash merge** exposed the classic DCO-vs-GitHub-squash
  collision: GitHub rewrites the squash commit's author email to the
  account address (here the account noreply, because `vyncint@icloud.com`
  isn't verified on the account — see §2.2). `check-dco.sh` now exempts
  GitHub web-flow merge commits from the email match (a sign-off must
  still be present; the underlying PR commits were already fully checked).
  The one red `commit-policy` run on `main` from before that fix is
  expected history, not an unresolved failure.

## 2. Manual steps for you

1. **crates.io publishing auth** (needed before the first `v*` tag):
   - Preferred — Trusted Publishing (no long-lived token): after the crate
     name exists on crates.io (first publish may need a token — see
     fallback), go to *crates.io → termlens → Settings → Trusted
     Publishing → Add* and enter repository `vyncint/termlens`, workflow
     `release.yml`, environment blank. `release.yml` already requests the
     OIDC token and prefers it.
   - Fallback — token: create an API token at
     <https://crates.io/settings/tokens> (scope: `publish-new`,
     `publish-update`), then
     `gh secret set CARGO_REGISTRY_TOKEN --repo vyncint/termlens`.
2. **Commit email + signing.** History is authored as
   `Vyncint Ng <vyncint@icloud.com>`. Add **and verify** that address on
   your GitHub account (*Settings → Emails*) — it does two things: links
   the commits to your profile, **and makes GitHub keep it as the author
   email on future squash merges** (today GitHub substitutes your account
   noreply address, which is why the DCO check needed a web-flow
   exemption — see §4.12). For *Verified* badges, also add a signing key,
   e.g. SSH signing:

   ```sh
   git config --global gpg.format ssh
   git config --global user.signingkey ~/.ssh/id_ed25519.pub
   git config --global commit.gpgsign true
   # then: GitHub → Settings → SSH and GPG keys → New SSH key → type "Signing key"
   ```

3. **Git remote / SSH identity on this machine.** Your SSH key
   authenticates as a different GitHub account with no access to this
   repo, so the remote was switched to HTTPS using the `gh` credential
   helper (active account `vyncint`). Keep it, or set up a per-account
   SSH alias if you prefer SSH.
4. **Ruleset — applied AND enforcing** (the brief's plan caveat turned out
   obsolete: rulesets now enforce on Free-plan private repos). Ruleset
   `protect-main` (id 20601249): PR required before merge (0 approvals
   while solo), dismiss stale approvals, required checks `required-green` +
   `commit-policy`, linear history, no force pushes, no deletions.
   **Consequence: direct pushes to `main` are already blocked** — a test
   push was rejected, and the final commits landed through PRs. Manage it
   at <https://github.com/vyncint/termlens/settings/rules>.
5. **Dependabot PRs under the bot-author policy.** Squash-merging a
   Dependabot PR would put a bot-authored commit on `main`, which our own
   `commit-policy` (push trigger) flags. Pattern used for PR #1 and
   recommended henceforth: apply the same bump in a human-authored,
   signed-off PR, reference/close the Dependabot PR. Dependabot stays
   valuable as the notifier; a human stays the author of record.

## 3. Go-public checklist (when you flip the switch)

```sh
gh repo edit vyncint/termlens --visibility public --accept-visibility-change-consequences
```

Then:

- [ ] Verify the ruleset still enforces (it does today, but re-check):
      try a direct push to `main`; it must be rejected.
- [ ] Bump required approvals to 1:
      edit ruleset 20601249 → `pull_request.required_approving_review_count: 1`.
- [ ] Enable secret scanning + push protection:
      `gh api -X PATCH repos/vyncint/termlens -f 'security_and_analysis[secret_scanning][status]=enabled' -f 'security_and_analysis[secret_scanning_push_protection][status]=enabled'`
- [ ] Enable Private Vulnerability Reporting (Settings → Code security), and
      delete the "once public" caveat in SECURITY.md.
- [ ] Decide on Discussions (`gh repo edit --enable-discussions`); the brief asked for them off.
- [ ] docs.rs + crates.io badges go live after the first publish
      (`docs/RELEASING.md` end-to-end).
- [ ] Announce: r/rust, This Week in Rust (PR to `rust-lang/this-week-in-rust`),
      awesome-ratatui PR, and the ratatui forum/Discord testing channel.

**Outcome (2026-08-19).** The repository is public and the crate is
published, so the badges are live. The ruleset still enforces, secret
scanning and push protection are on, and Discussions stayed off. Two items
resolved differently from the plan: required approvals are deliberately
still **0**, because a solo maintainer cannot approve their own pull
request and requiring one approval would block every release; and the
announcements are still in flight, with the draft in
`docs/announce-r-rust.md`.

## 4. Deviations from the build brief, and why

1. **MSRV 1.85, not portable-pty/vt100's ~1.70** — the default `insta`
   feature's tree (insta → tempfile → getrandom 0.4) hard-requires 1.85;
   we declare only what CI actually verifies (`--locked`).
2. **`unicode-width` is a dev-dependency** — vt100 already reports wide
   cells; the lib never needed it, and the dep tree stays lean (the brief
   listed it among the runtime dependencies).
3. **Builder has `arg`/`args`** — the brief's API sketch showed only
   `spawn(program)`;
   argv-less spawning can't drive `sh -c` or any real CLI.
4. **`cursor: hidden`** in the snapshot header when the app hides the
   cursor — the brief's sample output only showed the visible-cursor form;
   hiding is
   deliberate TUI behavior worth asserting on.
5. **`with_styles()` is a documented plan (DESIGN.md §3), not a stub
   function** — shipping a callable API whose output will change in v0.2 is
   a semver trap; a documented reservation is not.
6. **`env_clear()` order-independence** (unlike `std::process::Command`) —
   the brief's own example calls `.env("TERM", …)` *before* `.env_clear()`
   and expects TERM to survive; documented on the method.
7. **Internal `Emulator` trait gained `set_size`** — resize must reach the
   grid; trait is `pub(crate)`, no API impact.
8. **Negative-test commit uses a generic "Example Bot" identity and
   "Generated with SomeAgent"** instead of naming a real AI vendor — the
   zero-AI-attribution rule applies everywhere, test artifacts included; the
   generic forms still trip every pattern class (see the red run above).
9. **Ruleset JSON fallback not committed** — the brief expected rulesets
   not to enforce on Free-plan private repos and asked for the ruleset
   JSON to be committed only if the API refused it. GitHub now enforces
   rulesets on such repos and the API accepted, so by the brief's own
   condition there was nothing to save.
10. **`stress.yml` accepts an `iterations` input** — the brief's fixed
    100 iterations remain the default; one-off deeper or shallower runs
    become possible.
11. **`checkout@v7`** rather than v4 — v4 targets end-of-life Node 20 and
    annotates every job; bumped to current (via Dependabot's own proposal,
    landed human-authored per §2.5).
12. **`check-dco.sh` exempts GitHub web-flow merge commits** (committer
    `noreply@github.com`) from the sign-off==author-email match — GitHub
    rewrites squash-commit author emails, so the strict match fails by
    construction; a sign-off must still be present, and the real commits
    were already checked by the required PR run. Found live on the first
    squash merge; the brief's DCO design predates this GitHub behavior.
13. **Stress workflow prebuilds `--all-targets`** (not `--tests`) and the
    fixture tests carry 30s deadlines — the first stress run proved that
    compiling fixture bins mid-iteration on a cold 2-vCPU runner blows a
    10s deadline. Generous deadlines cost nothing when green.

## 5. Where to start reading

`README.md` → `docs/DESIGN.md` → `crates/termlens/src/terminal.rs` (the
whole runtime is ~450 lines) → `tests/fixtures.rs` for what using it feels
like. `cargo run --example inspect -- <your-app>` shows you any program
through termlens's eyes.
