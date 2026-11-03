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
# case-insensitively and across a line break.
#
# A `[label]` inside an inline code span or a fenced code block is text and
# names nothing, so it appends nothing: the 0.11.8 notes ended with the two
# definitions the #658 entry quotes as code, and re-extracting 0.11.1 added
# today's `[Unreleased]` compare link to it (#674). A reference whose text
# holds code, such as [`Unsupported`] in 0.11.0, still names its label: a
# code span loses its brackets and keeps the rest. Still counted, and
# harmless because an unused definition renders as nothing: the text half
# of `[text](url)` or `[text][label]`, when a definition is named `text`.
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
  function run(t, c,   n) { n = 0; while (substr(t, n + 1, 1) == c) n++; return n }
  # 1 when the line opens, closes or sits inside a fenced code block. A
  # fence is three or more backticks or tildes at any indentation, since a
  # list item indents its blocks; it closes on a run of the same character
  # at least as long, with nothing after it.
  function fence(line,   t, c, n) {
    t = line; sub(/^[ \t]*/, "", t); c = substr(t, 1, 1)
    if (fchar != "") {
      if (c == fchar) {
        n = run(t, c)
        if (n >= flen && substr(t, n + 1) ~ /^[ \t]*$/) fchar = ""
      }
      return 1
    }
    if (c != "`" && c != "~") return 0
    n = run(t, c)
    if (n < 3 || (c == "`" && index(substr(t, n + 1), "`") > 0)) return 0
    fchar = c; flen = n
    return 1
  }
  # The text of one block with every bracket inside an inline code span
  # replaced, the rest of the span kept: `[Unreleased]` then names nothing,
  # and [`Unsupported`] still names `unsupported` in backticks. A span opens
  # on a run of backticks and closes on the next run exactly as long, which
  # may be on a later line of the block; a run with no such closer is
  # literal. Outside a span, a backslash makes a backtick or a bracket
  # literal. No regex intervals or gensub: this runs under BSD awk too.
  function mask(s,   out, i, j, k, m, n, r, c, cl, span) {
    out = ""; n = length(s); i = 1
    while (i <= n) {
      c = substr(s, i, 1)
      if (c == "\\") {
        c = substr(s, i + 1, 1)
        if (c == "`" || c == "[" || c == "]") { out = out "\001\001"; i += 2 }
        else { out = out "\\"; i++ }
        continue
      }
      if (c != "`") { out = out c; i++; continue }
      j = i; while (substr(s, j, 1) == "`") j++
      r = j - i
      k = j; cl = 0
      while (k <= n) {
        if (substr(s, k, 1) != "`") { k++; continue }
        m = k; while (substr(s, m, 1) == "`") m++
        if (m - k == r) { cl = k; break }
        k = m
      }
      if (!cl) { out = out substr(s, i, r); i = j; continue }
      span = substr(s, i, cl + r - i)
      gsub(/\[/, "\001", span); gsub(/\]/, "\001", span)
      out = out span; i = cl + r
    }
    return out
  }
  FNR == 1 { fchar = "" }
  { infence = fence($0) }
  # A line in a code fence is never a definition or a heading.
  !infence && match($0, /^ ? ? ?\[[^]]+\]:/) {
    d = $0; sub(/^ */, "", d)
    l = label(substr(d, 2, index(d, "]:") - 2))
    if (FNR == NR) {
      if (!(l in def)) { def[l] = $0; order[++ndefs] = l }
    }
    next
  }
  FNR == NR { next }
  # Section headers look like "## [0.1.0] - 2026-01-31" or "## [Unreleased]".
  !infence && /^## \[/ {
    if (found) { done = 1 }
    else if (index($0, "[" ver "]") > 0) { found = 1; next }
  }
  found && !done { lines[++n] = $0; code[n] = infence }
  END {
    start = 1; while (start <= n && lines[start] ~ /^[[:space:]]*$/) start++
    end = n;   while (end >= start && lines[end]   ~ /^[[:space:]]*$/) end--
    if (!found) { print "__EXTRACT_CHANGELOG_MISSING__"; exit }
    if (start > end) { print "__EXTRACT_CHANGELOG_EMPTY__"; exit }
    print "__EXTRACT_CHANGELOG_CONTENT__"
    # What the section names is read block by block, so a code span cannot
    # run from one list item into the next: a blank line, a fence or a list
    # item starts a new block, a heading or a table row is a block of its
    # own, and a fenced block contributes nothing.
    body = ""; block = ""
    for (i = start; i <= end; i++) {
      print lines[i]
      t = lines[i]
      own = (t ~ /^[ \t]*#+([ \t]|$)/ || t ~ /^[ \t]*[|]/)
      if (code[i] || own || t ~ /^[ \t]*$/ || t ~ /^[ \t]*([-*+]|[0-9]+[.)])([ \t]|$)/) {
        body = body "\n" mask(block); block = ""
      }
      if (code[i]) continue
      sub(/^[ \t]+/, "", t)
      block = (block == "" ? t : block " " t)
      if (own) { body = body "\n" mask(block); block = "" }
    }
    body = body "\n" mask(block)
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
