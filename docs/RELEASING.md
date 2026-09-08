# Releasing termlens

One page, copy-pasteable. Maintainers only.

## Prerequisites (already satisfied)

- Publishing auth is **crates.io Trusted Publishing**, configured **per
  crate**: crates.io → *crate* → Settings → Trusted Publishing → GitHub,
  repository `vyncint/termlens`, workflow `release.yml`.
  - `termlens` — linked 2026-08-09.
  - `termlens-cli` — **not linked yet**. The crate was first published on
    2026-09-08 by the one-shot `publish-cli.yml` bootstrap (a new name has
    no crate to configure Trusted Publishing against, which is the whole
    chicken-and-egg). Until it is linked, `release.yml`'s termlens-cli
    step will fail on authentication *after* `termlens` has already gone
    out — so link it before the next tag. Then delete the
    `CARGO_REGISTRY_TOKEN` secret and `.github/workflows/publish-cli.yml`,
    and the repository is back to storing no secret at all.
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
exists, so the *first* publish of a new name cannot use it.
`.github/workflows/publish-cli.yml` is that one-shot path — dispatch-only,
a typed confirmation, a tag rather than a branch, the same
tag-matches-version guard, `--locked` — run once against
`CARGO_REGISTRY_TOKEN`. It published `termlens-cli` 0.10.0 on 2026-09-08.

Afterwards, always: link Trusted Publishing for the new crate, revoke the
token, delete the secret, delete the workflow. A stored publish token is a
standing risk that this repository otherwise does not carry.

## If something fails mid-release

- **Before publish**: fix, delete the tag (`git push --delete origin
  vX.Y.Z`), re-tag. Nothing was published; the world never saw it.
- **After publish**: crates.io is immutable — ship `X.Y.Z+1`. Never yank
  unless the release is actively harmful (yanked crates still break
  downstream lockfiles).
