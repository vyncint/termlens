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
#   tools/release-notes-selftest/run.sh               # the cases below, checked
#
# An empty [Unreleased] section is valid and prints nothing with exit status 0.
# An empty numbered release section is an error, as is a missing section.
#
# A section is printed with the link reference definitions it uses. Keep a
# Changelog puts them at the foot of the file, below every section, so a
# section copied alone kept its `` [`Unsupported`] `` and lost what it
# pointed at, and the v0.11.0 release notes showed the brackets (#658).
# Definitions are never part of a section's body, wherever they sit: they
# are collected from the whole file and the ones the section names are
# appended, in file order. Naming is matched as CommonMark matches a label,
# case-insensitively; a definition the section merely mentions in passing
# is appended too, which renders as nothing.
#
# Usage: extract-changelog.sh <version|vX.Y.Z|Unreleased> [CHANGELOG.md]
set -euo pipefail

version="${1:?usage: extract-changelog.sh <version|vX.Y.Z|Unreleased> [CHANGELOG.md]}"
version="${version#v}"
changelog="${2:-CHANGELOG.md}"

result="$(awk -v ver="$version" '
  function label(s) {
    s = tolower(s); gsub(/[ \t]+/, " ", s); sub(/^ /, "", s); sub(/ $/, "", s)
    return s
  }
  # A line in a code fence is never a definition or a heading.
  FNR == 1 { fenced = 0 }
  /^ ? ? ?(```|~~~)/ { fenced = !fenced }
  !fenced && match($0, /^ ? ? ?\[[^]]+\]:/) {
    d = $0; sub(/^ */, "", d)
    l = label(substr(d, 2, index(d, "]:") - 2))
    if (FNR == NR) {
      if (!(l in def)) { def[l] = $0; order[++ndefs] = l }
    }
    next
  }
  FNR == NR { next }
  # Section headers look like "## [0.1.0] - 2026-01-31" or "## [Unreleased]".
  !fenced && /^## \[/ {
    if (found) { done = 1 }
    else if (index($0, "[" ver "]") > 0) { found = 1; next }
  }
  found && !done { lines[++n] = $0 }
  END {
    start = 1; while (start <= n && lines[start] ~ /^[[:space:]]*$/) start++
    end = n;   while (end >= start && lines[end]   ~ /^[[:space:]]*$/) end--
    if (!found) { print "__EXTRACT_CHANGELOG_MISSING__"; exit }
    if (start > end) { print "__EXTRACT_CHANGELOG_EMPTY__"; exit }
    print "__EXTRACT_CHANGELOG_CONTENT__"
    body = ""
    for (i = start; i <= end; i++) { print lines[i]; body = body " " lines[i] }
    body = tolower(body); gsub(/[ \t]+/, " ", body)
    sep = 0
    for (i = 1; i <= ndefs; i++) {
      if (index(body, "[" order[i] "]") == 0) continue
      if (!sep) { print ""; sep = 1 }
      print def[order[i]]
    }
  }
' "$changelog" "$changelog")"

case "$result" in
__EXTRACT_CHANGELOG_MISSING__)
  echo "::error::No ${changelog} section found for version '${version}'." >&2
  exit 1
  ;;
__EXTRACT_CHANGELOG_EMPTY__)
  if [ "$version" != "Unreleased" ]; then
    echo "::error::${changelog} section for version '${version}' is empty." >&2
    exit 1
  fi
  ;;
__EXTRACT_CHANGELOG_CONTENT__*)
  out="${result#*$'\n'}"
  printf '%s\n' "$out"
  ;;
esac
