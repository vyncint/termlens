#!/usr/bin/env bash
# The stability-candidate statement is made in three places — README.md,
# the CHANGELOG header and docs/STABILITY.md — and #328/#334 ask that they
# say the same thing in the same words. Three hand-maintained copies of one
# promise drift; this makes the promise checked rather than maintained.
# The canonical text is the paragraph in docs/STABILITY.md that opens with
# the bold "0.11.0 is the stability candidate" and ends at the blank line.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

statement="$(awk '
  /^\*\*0\.11\.0 is the stability candidate\*\*/ { on = 1 }
  on && /^$/ { exit }
  on { print }
' docs/STABILITY.md)"

if [ -z "$statement" ]; then
  echo "::error::docs/STABILITY.md has no stability-candidate statement"
  exit 1
fi

status=0
for doc in README.md CHANGELOG.md; do
  if python3 - "$doc" "$statement" <<'PY'
import sys
doc, statement = sys.argv[1], sys.argv[2]
sys.exit(0 if statement in open(doc, encoding="utf-8").read() else 1)
PY
  then
    echo "candidate statement: $doc carries it verbatim ($(printf '%s\n' "$statement" | wc -l | tr -d ' ') lines)"
  else
    echo "::error::$doc does not carry the stability-candidate statement in the same words as docs/STABILITY.md:"
    printf '%s\n' "$statement" | sed 's/^/  /'
    status=1
  fi
done
exit "$status"
