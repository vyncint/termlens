#!/usr/bin/env bash
# Proves the semver gate can fail, before it is trusted to pass (#323, #598).
#
# Four one-file crates model the ways a real break slips past a naive
# `cargo semver-checks` run:
#
#   baseline/  0.10.1 with `kept`, `removed`, `to_svg`, and `find` behind
#              the `regex` feature
#   bumped/    0.11.0 with `removed` deleted — a break *and* a version bump
#   gated/     0.10.1 with `to_svg` moved behind the off-by-default `render`
#   moved/     0.10.1 with `find` moved behind `regex` *and* `serde`
#
# Measured with cargo-semver-checks 0.50.0: a 0.10 -> 0.11 bump is inferred as
# a major release and every check is skipped, so the deleted function passes;
# `--all-features` never loses `to_svg`, so the gating is invisible in that
# view; and of the default, explicit-only and all-features views none loses
# `find`, which only a `--features regex` build does. Each is asserted below
# *as the hole it is*, so that a checker version which closes one makes this
# script fail and the forcing in `.github/scripts/check-semver.sh` gets
# reconsidered in the open rather than left as cargo cult.
#
# The second half runs the gate itself, end to end, in a scratch repository
# whose history models a declared break and what can follow it (#598). A
# second, undeclared removal must fail once a break is declared, and the
# moved `find` must fail; the declared break, an addition beside it and the
# release that ships it must pass. The gate's copy sits at the same path
# there, so it takes the scratch tree for the one to check, and
# SEMVER_GATE_BASELINE_ROOT stands baseline/ in for the crates.io release.
#
# Usage: tools/semver-gate-selftest/run.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
root="$(cd ../.. && pwd)"
status=0

# `report <label> <expected: zero|nonzero> <got> [pattern]`: one line per
# expectation, and the log of any miss — an exit status, or a pattern the
# log should hold and does not.
report() {
  label=$1
  want=$2
  got=$3
  pattern=${4:-}
  case "$want:$got" in
    zero:0 | nonzero:[1-9]*)
      if [ -z "$pattern" ] || grep -Fq -- "$pattern" out.log; then
        printf '  ok    %-58s exit %s\n' "$label" "$got"
        return
      fi
      printf '  FAIL  %-58s exit %s, but the log lacks "%s"\n' "$label" "$got" "$pattern" >&2 ;;
    *)
      printf '  FAIL  %-58s exit %s, expected %s\n' "$label" "$got" "$want" >&2 ;;
  esac
  sed 's/^/        /' out.log >&2
  status=1
}

# `<label> <expected: zero|nonzero> <crate> <cargo-semver-checks args…>`
expect() {
  label=$1
  want=$2
  crate=$3
  shift 3
  got=0
  cargo semver-checks --manifest-path "$crate/Cargo.toml" --baseline-root baseline "$@" \
    > out.log 2>&1 || got=$?
  report "$label" "$want" "$got"
}

echo "semver gate self-test: $(cargo semver-checks --version)"

# A crate against itself passes under the strictest type: the harness works.
expect "baseline against itself, forced patch" zero baseline --release-type patch --default-features

# Hole 1: the version number decides how much is checked. Inferred, the
# bump to 0.11.0 reads as a major release and nothing is checked at all.
expect "bumped, inferred release type (the hole)" zero bumped --all-features
expect "bumped, forced patch" nonzero bumped --release-type patch --all-features
# The exception path a `breaking` label switches on. `minor` is not enough:
# on a 0.x crate cargo-semver-checks still treats a removed item as major.
expect "bumped, forced minor (still fails on 0.x)" nonzero bumped --release-type minor --all-features
expect "bumped, forced major (the exception path)" zero bumped --release-type major --all-features

# Hole 2: `--all-features` cannot see an item moved behind a feature. The
# default and explicit-only views can, which is why the gate runs them too.
expect "gated, forced patch, all-features (the hole)" zero gated --release-type patch --all-features
expect "gated, forced patch, default-features" nonzero gated --release-type patch --default-features
expect "gated, forced patch, only-explicit-features" nonzero gated --release-type patch --only-explicit-features

# Hole 3: an item moved from one feature to two is invisible to all three of
# those views. One feature alone sees it, which is why the gate runs each.
expect "moved, forced patch, default-features (the hole)" zero moved --release-type patch --default-features
expect "moved, forced patch, only-explicit-features (the hole)" zero moved --release-type patch --only-explicit-features
expect "moved, forced patch, all-features (the hole)" zero moved --release-type patch --all-features
expect "moved, forced patch, regex alone" nonzero moved --release-type patch --only-explicit-features --features regex

echo "semver gate self-test, the gate itself: .github/scripts/check-semver.sh"

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
git init -q -b main "$scratch"
git -C "$scratch" config user.name "Selftest"
git -C "$scratch" config user.email "selftest@example.com"
# A signing key in the caller's global config is not what is under test.
git -C "$scratch" config commit.gpgsign false
mkdir -p "$scratch/.github/scripts" "$scratch/src"
cp "$root/.github/scripts/check-semver.sh" "$scratch/.github/scripts/"
printf 'target/\nCargo.lock\n' > "$scratch/.gitignore"

# `state <tag> <version> <section|-> <item …>`: a commit of the model at
# that version with those items, and a CHANGELOG with one `- **Breaking:**`
# bullet under `## [<section>]`, or none for `-`. Tagged <tag>.
state() {
  tag=$1
  ver=$2
  section=$3
  shift 3
  sed "s/^version = .*/version = \"$ver\"/" baseline/Cargo.toml > "$scratch/Cargo.toml"
  printf '%s\n' "$@" > "$scratch/src/lib.rs"
  {
    printf '# Changelog\n\n## [Unreleased]\n'
    if [ "$section" != "-" ]; then
      if [ "$section" != Unreleased ]; then
        printf '\n## [%s] - 2026-02-01\n' "$section"
      fi
      printf '\n### Removed\n\n- **Breaking:** `removed` is gone.\n'
    fi
    printf '\n## [0.10.1] - 2026-01-01\n\n- The published model.\n'
  } > "$scratch/CHANGELOG.md"
  git -C "$scratch" add -A
  git -C "$scratch" commit -q -m "$tag"
  git -C "$scratch" tag "$tag"
}

kept='pub fn kept() {}'
removed='pub fn removed() {}'
to_svg='pub fn to_svg() {}'
find_regex='#[cfg(feature = "regex")] pub fn find() {}'

# main: published, then the declared break, then a second removal on top.
state published 0.10.1 - "$kept" "$removed" "$to_svg" "$find_regex"
state declared 0.10.1 Unreleased "$kept" "$to_svg" "$find_regex"
state second 0.10.1 Unreleased "$to_svg" "$find_regex"
# Beside the second removal, on the declared break: an addition, and the
# release that moves the bullet under its own version.
git -C "$scratch" checkout -q declared
state additive 0.10.1 Unreleased "$kept" "$to_svg" "$find_regex" 'pub fn added() {}'
git -C "$scratch" checkout -q declared
state release 0.11.0 0.11.0 "$kept" "$to_svg" "$find_regex"
# And from published, `find` moved behind a second feature, undeclared.
git -C "$scratch" checkout -q published
state moved 0.10.1 - "$kept" "$removed" "$to_svg" \
  '#[cfg(all(feature = "regex", feature = "serde"))] pub fn find() {}'

published="$(git -C "$scratch" rev-parse published)"
declared="$(git -C "$scratch" rev-parse declared)"

# `gate <label> <expected> <tag> <pattern|-> [VAR=value …]`: the gate at that
# commit, with baseline/ standing for the published 0.10.1 and the
# workflow's variables cleared unless given.
gate() {
  label=$1
  want=$2
  at=$3
  pattern=$4
  shift 4
  if [ "$pattern" = "-" ]; then
    pattern=""
  fi
  git -C "$scratch" checkout -q "$at"
  got=0
  env BREAKING_LABEL= BASE_REV= GITHUB_STEP_SUMMARY= \
    SEMVER_GATE_BASELINE_ROOT="$PWD/baseline" "$@" \
    "$scratch/.github/scripts/check-semver.sh" 0.10.1 > out.log 2>&1 || got=$?
  report "$label" "$want" "$got" "$pattern"
}

gate "the declared break, which adds its own bullet" zero declared \
  "adds the declaring bullet" BASE_REV="$published"
gate "a second removal once a break is declared" nonzero second \
  "not patch-compatible with $declared" BASE_REV="$declared"
gate "the same, with the breaking label" zero second \
  "the label declares" BASE_REV="$declared" BREAKING_LABEL=true
gate "the same, with no base revision (the hole, said)" zero second \
  "BASE_REV names none" BASE_REV=
gate "the same, with a base of forty zeros" zero second \
  "BASE_REV names none" BASE_REV=0000000000000000000000000000000000000000
gate "an addition beside a declared break" zero additive \
  "patch-compatible with $declared" BASE_REV="$declared"
gate "the release, its bullet moved under [0.11.0]" zero release \
  "patch-compatible with $declared" BASE_REV="$declared"
gate "find moved from regex to regex and serde" nonzero moved \
  "the regex view of" BASE_REV="$published"

rm -f out.log
if [ "$status" -eq 0 ]; then
  echo "semver gate self-test: PASS — the gate fails where it must and passes where it must"
fi
exit "$status"
