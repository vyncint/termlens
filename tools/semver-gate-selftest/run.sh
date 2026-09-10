#!/usr/bin/env bash
# Proves the semver gate can fail, before it is trusted to pass (#323).
#
# Three one-file crates model the two ways a real break slips past a naive
# `cargo semver-checks` run:
#
#   baseline/  0.10.1 with `kept`, `removed`, `to_svg` and an empty feature
#   bumped/    0.11.0 with `removed` deleted — a break *and* a version bump
#   gated/     0.10.1 with `to_svg` moved behind the off-by-default feature
#
# Measured with cargo-semver-checks 0.50.0: a 0.10 -> 0.11 bump is inferred as
# a major release and every check is skipped, so the deleted function passes;
# and `--all-features` never loses `to_svg`, so the gating is invisible in
# that view. Both are asserted below *as the holes they are*, so that a
# checker version which closes one makes this script fail and the forcing in
# `.github/scripts/check-semver.sh` gets reconsidered in the open rather than
# left as cargo cult.
#
# Usage: tools/semver-gate-selftest/run.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
status=0

# `<label> <expected: zero|nonzero> <crate> <cargo-semver-checks args…>`
expect() {
  label=$1
  want=$2
  crate=$3
  shift 3
  got=0
  cargo semver-checks --manifest-path "$crate/Cargo.toml" --baseline-root baseline "$@" \
    > out.log 2>&1 || got=$?
  case "$want:$got" in
    zero:0 | nonzero:[1-9]*)
      printf '  ok    %-58s exit %s\n' "$label" "$got" ;;
    *)
      printf '  FAIL  %-58s exit %s, expected %s\n' "$label" "$got" "$want" >&2
      sed 's/^/        /' out.log >&2
      status=1 ;;
  esac
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
# default and explicit-only views can, which is why the gate runs all three.
expect "gated, forced patch, all-features (the hole)" zero gated --release-type patch --all-features
expect "gated, forced patch, default-features" nonzero gated --release-type patch --default-features
expect "gated, forced patch, only-explicit-features" nonzero gated --release-type patch --only-explicit-features

rm -f out.log
if [ "$status" -eq 0 ]; then
  echo "semver gate self-test: PASS — the gate fails where it must and passes where it must"
fi
exit "$status"
