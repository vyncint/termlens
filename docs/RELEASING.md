# Releasing termlens

One page, copy-pasteable. Maintainers only.

## Prerequisites (once)

- A crates.io account (log in with GitHub) with a **verified email** —
  publishing is refused without one.
- **The first publish must use an API token** — crates.io has no
  "pending publisher" feature (RFC 3691), so Trusted Publishing can only
  be configured after the crate exists:
  1. crates.io → Account Settings → API Tokens → New token: scopes
     `publish-new` + `publish-update`, crate pattern `termlens`, short
     expiry (it is needed exactly once).
  2. `gh secret set CARGO_REGISTRY_TOKEN --repo vyncint/termlens`
  `release.yml` tries Trusted Publishing first and falls back to this
  secret automatically.
- **After the first publish, switch to Trusted Publishing** (tokenless):
  crates.io → termlens → Settings → Trusted Publishing → GitHub:
  repository `vyncint/termlens`, workflow `release.yml`, environment
  *(none)*. Then revoke the token and
  `gh secret delete CARGO_REGISTRY_TOKEN --repo vyncint/termlens`.

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
4. `cargo publish -p termlens` via Trusted Publishing (OIDC) or the
   token fallback,
5. creates the GitHub Release with notes extracted from the CHANGELOG
   section for that version (`.github/scripts/extract-changelog.sh`).

## If something fails mid-release

- **Before publish**: fix, delete the tag (`git push --delete origin
  vX.Y.Z`), re-tag. Nothing was published; the world never saw it.
- **After publish**: crates.io is immutable — ship `X.Y.Z+1`. Never yank
  unless the release is actively harmful (yanked crates still break
  downstream lockfiles).
