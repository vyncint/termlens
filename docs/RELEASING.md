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

## Before a 1.0 tag

`v1.0.0` requires all three sections of [STABILITY.md](STABILITY.md) —
Windows, backend, styled history — to be filled in with their decision
and the measurement behind it, and the "What the promise covers" list to
match `lib.rs`. A 1.0 with an open section is a 0.x with a bigger number.

## Cutting vX.Y.Z

```sh
# 0. Green main + no flakes: run the stress workflow and wait for it.
gh workflow run stress.yml --ref main
gh run watch                                  # both OSes must pass

# 1. Bump the version (workspace.package.version in root Cargo.toml), and
#    the same number in crates/termlens-cli/Cargo.toml's `termlens = { version
#    = … }` — a path dependency publishes by its version.
$EDITOR Cargo.toml crates/termlens-cli/Cargo.toml
cargo check --workspace                       # refreshes Cargo.lock

# 2. Move the CHANGELOG section.
$EDITOR CHANGELOG.md                          # [Unreleased] -> [X.Y.Z] - YYYY-MM-DD
                                              # add a fresh empty [Unreleased] above

# 3. Land it.
git checkout -b chore/release-vX.Y.Z
git commit -s -am "chore: release vX.Y.Z"
gh pr create --fill && gh pr merge --squash --auto

# 4. Tag the squash-merged commit on main.
git checkout main && git pull
git tag vX.Y.Z
git push origin vX.Y.Z
```

Pushing the tag runs `release.yml`, which:

1. fails unless tag == `crates/termlens` version,
2. re-runs the full CI gates (`workflow_call` into ci.yml),
3. runs `cargo-semver-checks` against the last published release
   (skipped gracefully on the first release),
4. `cargo publish -p termlens` then `cargo publish -p termlens-cli`, both
   via Trusted Publishing (OIDC); each crate needs its own publisher
   link, and the step says so if one is missing,
5. creates the GitHub Release with notes extracted from the CHANGELOG
   section for that version (`.github/scripts/extract-changelog.sh`), and
6. runs the registry-consumer check against that published version on Linux
   and macOS.

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
