#!/usr/bin/env bash
# extract-changelog.sh — print the CHANGELOG.md section for one version.
#
# Used by release.yml to build GitHub Release notes; never hand-write notes.
#
# Usage:
#   .github/scripts/extract-changelog.sh 0.1.0        # bare version
#   .github/scripts/extract-changelog.sh v0.1.0       # tag form also fine
#
# tests:
#   .github/scripts/extract-changelog.sh Unreleased   # prints current section
set -euo pipefail

version="${1:?usage: extract-changelog.sh <version|vX.Y.Z|Unreleased>}"
version="${version#v}"

out="$(awk -v ver="$version" '
  # Section headers look like "## [0.1.0] - 2026-01-31" or "## [Unreleased]".
  /^## \[/ {
    if (found) exit
    if (index($0, "[" ver "]") > 0) { found = 1; next }
  }
  found { lines[++n] = $0 }
  END {
    start = 1; while (start <= n && lines[start] ~ /^[[:space:]]*$/) start++
    end = n;   while (end >= start && lines[end]   ~ /^[[:space:]]*$/) end--
    for (i = start; i <= end; i++) print lines[i]
  }
' CHANGELOG.md)"

if [ -z "$out" ]; then
  echo "::error::No CHANGELOG.md section found for version '${version}'." >&2
  exit 1
fi
printf '%s\n' "$out"
