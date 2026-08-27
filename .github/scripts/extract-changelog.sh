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
#
# An empty [Unreleased] section is valid and prints nothing with exit status 0.
# An empty numbered release section is an error, as is a missing section.
set -euo pipefail

version="${1:?usage: extract-changelog.sh <version|vX.Y.Z|Unreleased>}"
version="${version#v}"

result="$(awk -v ver="$version" '
  # Section headers look like "## [0.1.0] - 2026-01-31" or "## [Unreleased]".
  /^## \[/ {
    if (found) exit
    if (index($0, "[" ver "]") > 0) { found = 1; next }
  }
  found { lines[++n] = $0 }
  END {
    start = 1; while (start <= n && lines[start] ~ /^[[:space:]]*$/) start++
    end = n;   while (end >= start && lines[end]   ~ /^[[:space:]]*$/) end--
    if (!found) { print "__EXTRACT_CHANGELOG_MISSING__"; exit }
    if (start > end) { print "__EXTRACT_CHANGELOG_EMPTY__"; exit }
    print "__EXTRACT_CHANGELOG_CONTENT__"
    for (i = start; i <= end; i++) print lines[i]
  }
' CHANGELOG.md)"

case "$result" in
__EXTRACT_CHANGELOG_MISSING__)
  echo "::error::No CHANGELOG.md section found for version '${version}'." >&2
  exit 1
  ;;
__EXTRACT_CHANGELOG_EMPTY__)
  if [ "$version" != "Unreleased" ]; then
    echo "::error::CHANGELOG.md section for version '${version}' is empty." >&2
    exit 1
  fi
  ;;
__EXTRACT_CHANGELOG_CONTENT__*)
  out="${result#*$'\n'}"
  printf '%s\n' "$out"
  ;;
esac
