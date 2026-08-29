# Releasing termlens

One page, copy-pasteable. Maintainers only.

## Prerequisites (already satisfied)

- Publishing auth is **crates.io Trusted Publishing** (linked
  2026-08-09): crates.io → termlens → Settings → Trusted Publishing →
  GitHub, repository `vyncint/termlens`, workflow `release.yml`. No
  token or secret is stored anywhere.
- The publish job runs in the **`release` GitHub environment**, which
  only deploys from `v*` tags — an OIDC publish token can never be
  minted from a branch. (Optionally set the environment name `release`
  on the crates.io side too, for the server-side binding.)

## Cutting vX.Y.Z

```sh
# 0. Green main + no flakes: run the stress workflow and wait for it.
gh workflow run stress.yml --ref main
gh run watch                                  # both OSes must pass

# 1. Bump the version (workspace.package.version in root Cargo.toml).
$EDITOR Cargo.toml                            # version = "X.Y.Z"
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
4. `cargo publish -p termlens` via Trusted Publishing (OIDC) — the
   repository stores no tokens,
5. creates the GitHub Release with notes extracted from the CHANGELOG
   section for that version (`.github/scripts/extract-changelog.sh`), and
6. runs the registry-consumer check against that published version on Linux
   and macOS.

## If something fails mid-release

- **Before publish**: fix, delete the tag (`git push --delete origin
  vX.Y.Z`), re-tag. Nothing was published; the world never saw it.
- **After publish**: crates.io is immutable — ship `X.Y.Z+1`. Never yank
  unless the release is actively harmful (yanked crates still break
  downstream lockfiles).
