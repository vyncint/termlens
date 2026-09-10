#!/usr/bin/env bash
# Every link in the packaged README must be absolute, and must point at a
# file that exists.
#
# `crates/termlens/Cargo.toml` sets `readme = "../../README.md"`, so this
# file is shipped as the crate's README and crates.io renders it there.
# crates.io rewrites a *relative* link against the crate's directory in the
# repository, not the repository root -- so `docs/DESIGN.md` became
# `…/blob/HEAD/crates/termlens/docs/DESIGN.md`, and every one of the ten
# relative links on the 0.10.2 page was a 404. Nothing in this repository
# could see it: the same file renders correctly on GitHub, where it *is* at
# the root.
#
# So: absolute links only, and the path each one names must exist here.
# The second half is the more useful one -- it is offline, deterministic,
# and catches a renamed or deleted target, which a link checker that only
# asks GitHub would report as a 200 for the wrong reason or not at all.
#
# Usage: check-readme-links.sh [README.md]
set -euo pipefail

readme="${1:-README.md}"
root=$(cd "$(dirname "$0")/../.." && pwd)
base="https://github.com/vyncint/termlens/blob/main/"
status=0

# `[text](target)` with a target that is neither absolute nor a fragment.
relative=$(grep -oE '\]\([^)]+\)' "$readme" \
  | sed -E 's/^\]\(//; s/\)$//' \
  | grep -vE '^(https?:|#|mailto:)' || true)
if [ -n "$relative" ]; then
  echo "$relative" | while IFS= read -r link; do
    echo "README LINK: relative target \"$link\" — crates.io rewrites it against" >&2
    echo "  crates/termlens/, where it does not exist. Use ${base}$link" >&2
  done
  status=1
fi

# Every in-repo absolute link names a path that is really here.
checked=0
missing=0
for url in $(grep -oE "${base}[^)]+" "$readme" | sort -u); do
  path=${url#"$base"}
  path=${path%%#*}
  checked=$((checked + 1))
  if [ ! -e "$root/$path" ] && [ ! -d "$root/$path" ]; then
    echo "README LINK: \"$path\" is linked but not in the repository" >&2
    missing=$((missing + 1))
  fi
done
if [ "$missing" -gt 0 ]; then
  status=1
fi

if [ "$status" -eq 0 ]; then
  echo "readme links: $checked absolute link(s), every target present, none relative"
fi
exit "$status"
