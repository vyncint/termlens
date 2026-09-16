#!/usr/bin/env bash
# check-emit-steps.sh — the emit fixture's documented steps against the ones
# it implements.
#
# `fixtures/emit` is the fixture almost every integration test drives, and
# its step language is the reference every test author reads. Since #304 the
# language lives in exactly one file, `fixtures/emit/src/steps.txt`: the
# module header includes it and `emit --help` prints it, so those two cannot
# disagree. What they still could disagree with is the `match` that parses
# the steps — a step added to one side only, or renamed on one, is invisible
# until someone wastes an afternoon on it (#318).
#
# This repository solves that class of drift twice already, with
# check-ci-gates-listed.sh and check-skill-snippets.sh; this is the third.
#
# A step is documented by a line in steps.txt beginning `--name` in column 1,
# and implemented by a match arm `"--name" =>` in main.rs. `-h`/`--help` is
# neither: it is handled before any step is parsed and never becomes a Step,
# and its arm (`"-h" | "--help" =>`) does not have the shape below.
#
# Portability: macOS ships bash 3.2 and BSD sed. No `declare -A`, no
# associative arrays, no GNU-only flags, no process substitution.
#
# Usage: check-emit-steps.sh [steps.txt] [main.rs]
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
steps="${1:-$root/fixtures/emit/src/steps.txt}"
main="${2:-$root/fixtures/emit/src/main.rs}"
for file in "$steps" "$main"; do
  [ -f "$file" ] || { echo "::error::$file does not exist"; exit 1; }
done

work="$(mktemp -d 2>/dev/null || mktemp -d -t termlens-emit-steps)"
trap 'rm -rf "$work"' EXIT

grep -oE '^--[a-z][a-z-]*' "$steps" | sort -u > "$work/documented"
grep -oE '^[[:space:]]+"--[a-z][a-z-]*" =>' "$main" \
  | grep -oE -- '--[a-z][a-z-]*' | sort -u > "$work/implemented"

documented="$(grep -c . "$work/documented" || true)"
implemented="$(grep -c . "$work/implemented" || true)"
if [ "$documented" -eq 0 ] || [ "$implemented" -eq 0 ]; then
  echo "::error::found $documented documented and $implemented implemented steps;" \
       "one of the two patterns has stopped matching, which would make this gate pass on anything"
  exit 1
fi

status=0
# -23: documented only. -13: implemented only. Named separately because the
# two are different mistakes with different fixes. Through files rather than
# a process substitution, so the loop runs in this shell and its `status=1`
# is the one that gets read.
comm -23 "$work/documented" "$work/implemented" > "$work/undone"
comm -13 "$work/documented" "$work/implemented" > "$work/unwritten"
while IFS= read -r step; do
  [ -z "$step" ] && continue
  echo "::error::\`$step\` is documented in ${steps#"$root"/} but no \`\"$step\" =>\` arm implements it"
  status=1
done < "$work/undone"
while IFS= read -r step; do
  [ -z "$step" ] && continue
  echo "::error::\`$step\` is implemented in ${main#"$root"/} but not documented in ${steps#"$root"/}"
  status=1
done < "$work/unwritten"

if [ "$status" -eq 0 ]; then
  echo "emit steps: $documented documented, $implemented implemented, the same set"
fi
exit "$status"
