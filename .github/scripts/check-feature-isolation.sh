#!/usr/bin/env bash
# The minimal library build must be minimal (#324).
#
# Cargo unifies features across every package selected in one invocation,
# and `termlens-cli` depends on the library with `features = ["serde"]` — so
# `cargo test --workspace --no-default-features` tested a library that had
# `serde` on, and a `cfg(feature = "serde")` mistake in the library could
# not be caught here. It was not hypothetical: `tests/record.rs` used
# `serde_json` unconditionally and `cargo test -p termlens
# --no-default-features` did not build.
#
# The `features` job now selects the library alone for every reduced
# configuration; this asserts that selection really is isolated, by asking
# Cargo for the library's own dependency tree — normal edges only, since the
# dev-dependencies (insta) legitimately pull serde in for the tests — and
# refusing any mention of serde in it.
#
# Usage: check-feature-isolation.sh
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

tree="$(cargo tree -p termlens --no-default-features -e normal,features)"
if printf '%s\n' "$tree" | grep -q 'serde'; then
  echo "::error::the minimal termlens build (-p termlens --no-default-features) still resolves serde:"
  printf '%s\n' "$tree" | grep -n 'serde' | sed 's/^/  /'
  exit 1
fi
echo "feature isolation: the minimal termlens tree ($(printf '%s\n' "$tree" | wc -l | tr -d ' ') lines) mentions no serde"
