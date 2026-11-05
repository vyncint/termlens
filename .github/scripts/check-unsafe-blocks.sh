#!/usr/bin/env bash
# .github/scripts/check-unsafe-blocks.sh — assert that the count of `unsafe {`
# blocks in the codebase matches the number documented in SECURITY.md (#706).
#
# SECURITY.md promises that unsafe is confined to a specific, audited count
# of blocks. This check ensures the documentation and implementation cannot
# drift when unsafe blocks are added or removed.
#
# Usage: .github/scripts/check-unsafe-blocks.sh [SECURITY.md]
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

doc="${1:-SECURITY.md}"

if [ ! -f "$doc" ]; then
  echo "::error::document not found: $doc" >&2
  exit 1
fi

documented="$(python3 - "$doc" <<'PY'
import sys, re

doc = sys.argv[1]
text = open(doc, encoding="utf-8").read()

m = re.search(r"\*\*`unsafe` is confined to ([a-zA-Z0-9]+) audited blocks", text)
if not m:
    sys.exit(1)

val = m.group(1).lower()
words = {
    "zero": 0, "one": 1, "two": 2, "three": 3, "four": 4,
    "five": 5, "six": 6, "seven": 7, "eight": 8, "nine": 9, "ten": 10,
    "eleven": 11, "twelve": 12, "thirteen": 13, "fourteen": 14,
    "fifteen": 15, "sixteen": 16, "seventeen": 17, "eighteen": 18,
    "nineteen": 19, "twenty": 20
}
count = words.get(val, int(val) if val.isdigit() else -1)
if count < 0:
    sys.exit(1)
print(count)
PY
)" || {
  echo "::error::$doc does not carry the audited unsafe block count statement (expected pattern: '**\`unsafe\` is confined to <N> audited blocks')" >&2
  exit 1
}

actual="$(grep -rn 'unsafe {' crates/*/src | wc -l | tr -d ' ')"

if [ "$actual" -ne "$documented" ]; then
  echo "::error::unsafe block count mismatch: $doc documents $documented, but crates/*/src contains $actual:" >&2
  grep -rn 'unsafe {' crates/*/src | sed 's/^/  /' >&2
  exit 1
fi

echo "unsafe blocks: $doc documents $documented, matching $actual in crates/*/src"
