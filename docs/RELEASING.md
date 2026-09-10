# Releasing termlens

One page, copy-pasteable. Maintainers only.

## Prerequisites (already satisfied)

- Publishing auth is **crates.io Trusted Publishing**, configured **per
  crate**: crates.io → *crate* → Settings → Trusted Publishing → GitHub,
  repository `vyncint/termlens`, workflow `release.yml`.
  - `termlens` — linked 2026-08-09.
  - `termlens-cli` — linked 2026-09-08, and first exercised by v0.10.1.
    The crate's own first publish could not use it (a new name has no
    crate to configure Trusted Publishing against) and went out through a
    one-shot token workflow, since deleted.

  No token or secret is stored for publishing.
- The publish job runs in the **`release` GitHub environment**, which
  only deploys from `v*` tags — an OIDC publish token can never be
  minted from a branch. (Optionally set the environment name `release`
  on the crates.io side too, for the server-side binding.)

## The stability candidate, and what may break

0.11.0 is the **stability candidate** ([STABILITY.md](STABILITY.md), "What
the promise covers"). From it, no promised item changes incompatibly
before 1.0. What that means for a release:

- **A patch or minor release breaks nothing.** The `semver` job holds it:
  every pull request is checked against the last published release with
  the release type forced to `patch`, in three feature views. A red
  `semver` on a PR is the answer, not an obstacle.
- **A necessary break is a new candidate** — `0.12.0`, never a patch — and
  restarts the observation window in
  [#335](https://github.com/vyncint/termlens/issues/335). Its pull request
  carries the **`breaking` label** (a maintainer applies it; the gate then
  runs with `--release-type major`), its description pastes the
  diagnostics the *forced* run produced (run
  `.github/scripts/check-semver.sh <baseline>` without the label to get
  them), and its CHANGELOG entry is a bullet beginning **`- **Breaking:**`**
  under **Changed** or **Removed** whose text matches those diagnostics and
  carries a migration table. The marker is what keeps `main` green after
  the merge, where no label exists, and what lets the release tag pass the
  same gate — so a break without the marker is a break the gate refuses,
  on the PR and again on the tag.
- **Release candidates for 1.0** are tagged `v1.0.0-rc.N` and publish to
  crates.io as pre-releases; a consumer opts in with an exact requirement
  (`termlens = "=1.0.0-rc.1"`), and Cargo never resolves a pre-release
  from `"0.11"` or `"1"`. The `v1.0.0` tag follows the readiness criteria
  in #335 — an external pilot, eight weeks of stable use from its first
  green run, every maintained consumer on the candidate, the daily
  fresh-install evidence — and **a date is never one of them**.

## Before a 1.0 tag

`v1.0.0` requires all three sections of [STABILITY.md](STABILITY.md) —
Windows, backend, styled history — to be filled in with their decision
and the measurement behind it, the "What the promise covers" section to
match `lib.rs`, and every criterion in #335 to be recorded as met, with
its evidence. A 1.0 with an open section is a 0.x with a bigger number.

## Cutting vX.Y.Z

```sh
# 0. Green main + no flakes: run the stress workflow on the exact commit
#    that will be tagged — not an older main — and wait for it. All three
#    OSes, all five shards, must pass. A release PR that lands after the
#    stress run is a different tree; run it again on the merged commit
#    before tagging.
gh workflow run stress.yml --ref main
gh run watch                                  # ubuntu, macos, windows × 5 shards

# 1. Bump the version (workspace.package.version in root Cargo.toml), and
#    the same number in crates/termlens-cli/Cargo.toml's `termlens = { version
#    = … }` — a path dependency publishes by its version.
$EDITOR Cargo.toml crates/termlens-cli/Cargo.toml
cargo check --workspace                       # refreshes Cargo.lock

# 2. Move the CHANGELOG section. A `- **Breaking:**` bullet moves with it,
#    which is what lets the tag's semver run pass (see above).
$EDITOR CHANGELOG.md                          # [Unreleased] -> [X.Y.Z] - YYYY-MM-DD
                                              # add a fresh empty [Unreleased] above

# 2b. Freeze this release's saved-screen shapes into the compatibility
#     corpus (crates/termlens/tests/compat/README.md), from this tree:
TERMLENS_WRITE_CORPUS=X.Y.Z cargo test -p termlens --features serde \
    --test compat write_corpus -- --ignored
cargo test -p termlens --all-features --test compat   # the new directory passes

# 3. Land it.
git checkout -b chore/release-vX.Y.Z
git commit -s -am "chore: release vX.Y.Z"
gh pr create --fill && gh pr merge --squash --auto

# 4. Tag the squash-merged commit on main.
git checkout main && git pull
git tag vX.Y.Z
git push origin vX.Y.Z

# 5. Once release.yml has published: move the semver gate's baseline to the
#    version that is now on crates.io. One literal, in one place — the
#    `.github/scripts/check-semver.sh X.Y.Z` step of ci.yml's `semver` job
#    (and the same line in CONTRIBUTING.md §1, or `gates-listed` fails).
#    After the release PR and not in it: a version that is not on crates.io
#    yet cannot be a baseline, and left at the old release the gate would
#    compare every later PR against a version nobody installs any more.
git checkout -b ci/semver-baseline-vX.Y.Z
$EDITOR .github/workflows/ci.yml CONTRIBUTING.md
git commit -s -am "ci: move the semver baseline to vX.Y.Z"
gh pr create --fill && gh pr merge --squash --auto
```

Between the release merge and step 5, `main` compares `X.Y.Z` against the
previous release. That is green when the release broke nothing, and green
for a release whose CHANGELOG section records its break as a `- **Breaking`
bullet — the gate reads that marker (`.github/scripts/check-semver.sh`), so
a break that was declared stays declared after the label on its pull
request is out of sight.

Pushing the tag runs `release.yml`, which:

1. fails unless tag == `crates/termlens` version,
2. re-runs the full CI gates (`workflow_call` into ci.yml) — the semver
   gate among them, against the last published release with the release
   type forced (`.github/scripts/check-semver.sh`),
3. `cargo publish -p termlens` then `cargo publish -p termlens-cli`, both
   via Trusted Publishing (OIDC); each crate needs its own publisher
   link, and the step says so if one is missing,
4. creates the GitHub Release with notes extracted from the CHANGELOG
   section for that version (`.github/scripts/extract-changelog.sh`), and
5. runs the registry-consumer check — the library as `cargo add termlens`
   and the CLI as `cargo install termlens-cli`, held to its exit-code
   contract — against that published version on Linux and macOS.

## Bootstrapping a brand-new crate

Trusted Publishing is configured per crate, on a crate that already
exists, so the *first* publish of a new name cannot use it. The recipe,
used once for `termlens-cli` on 2026-09-08 (see `publish-cli.yml` in that
commit range for the exact shape):

1. A dispatch-only workflow with a typed confirmation, checking out the
   **tag** rather than a branch, carrying `release.yml`'s
   tag-matches-version guard, publishing with `--locked` against a
   `CARGO_REGISTRY_TOKEN` repository secret.
2. Link Trusted Publishing for the new crate on crates.io.
3. Revoke the token, delete the secret, delete the workflow.

Step 3 is not optional and comes *after* a release has proved step 2: a
stored publish token is a standing risk this repository otherwise does not
carry, and deleting the only fallback before the mechanism is exercised is
the wrong order.

## If something fails mid-release

- **Before publish**: fix, delete the tag (`git push --delete origin
  vX.Y.Z`), re-tag. Nothing was published; the world never saw it.
- **After publish**: crates.io is immutable — ship `X.Y.Z+1`. Never yank
  unless the release is actively harmful (yanked crates still break
  downstream lockfiles).
