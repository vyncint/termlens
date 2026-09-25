#!/usr/bin/env bash
# Proves tools/preflight.sh accounts for every gate it runs, failures
# included (#368), before a green preflight is trusted:
#
#   failing/   one gate fails, one passes — both are asserted to be
#              reported, and the runner to exit non-zero
#   passing/   both pass — the runner is asserted to exit zero
#
# and that `-h` and `--help` print the usage to stdout alone and exit zero
# (#518).
#
# Every case, the help ones included, reads a fixture through the runner's
# PREFLIGHT_DOC override, so the repository's own CONTRIBUTING.md is never
# touched. It lists this self-test as a gate: a runner that read it would
# run the whole suite, this script included.
#
# Usage: tools/preflight-selftest/run.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
root="$(cd ../.. && pwd)"
runner="$root/tools/preflight.sh"
status=0

# `<doc> <expected: zero|nonzero> <label> [grep pattern ...]`
# The patterns are what make "reports each gate" and "keeps going after a
# failure" assertions rather than exit-status coincidences.
expect() {
  doc=$1
  want=$2
  label=$3
  shift 3
  got=0
  PREFLIGHT_DOC="$PWD/$doc" "$runner" > out.log 2>&1 || got=$?
  case "$want:$got" in
    zero:0 | nonzero:[1-9]*) ;;
    *)
      printf '  FAIL  %-58s exit %s, expected %s\n' "$label" "$got" "$want" >&2
      sed 's/^/        /' out.log >&2
      status=1
      return
      ;;
  esac
  for pattern in "$@"; do
    if ! grep -Eq -- "$pattern" out.log; then
      printf '  FAIL  %-58s missing %s\n' "$label" "$pattern" >&2
      sed 's/^/        /' out.log >&2
      status=1
      return
    fi
  done
  printf '  ok    %-58s exit %s\n' "$label" "$got"
}

expect_help() {
  flag=$1
  label=$2
  got=0
  # The passing fixture, not the repository's CONTRIBUTING.md: a help flag
  # that stopped exiting early would otherwise run every real gate — this
  # self-test among them — instead of failing here in a second.
  PREFLIGHT_DOC="$PWD/passing/CONTRIBUTING.md" "$runner" "$flag" > out.log 2> err.log || got=$?
  if [ "$got" -ne 0 ]; then
    printf '  FAIL  %-58s exit %s, expected 0\n' "$label" "$got" >&2
    sed 's/^/        /' out.log err.log >&2
    status=1
    return
  fi
  if [ -s err.log ] || [ "$(wc -l < out.log | tr -d ' ')" -ne 2 ] || \
    ! grep -Fqx 'usage: tools/preflight.sh [--fast] [-h|--help]' out.log || \
    ! grep -Fqx -- '--fast runs only formatting, clippy and tests.' out.log; then
    printf '  FAIL  %-58s unexpected help output\n' "$label" >&2
    sed 's/^/        /' out.log err.log >&2
    status=1
    return
  fi
  printf '  ok    %-58s exit %s\n' "$label" "$got"
}

expect failing/CONTRIBUTING.md nonzero "a failing gate is reported and the run continues" \
  '^  FAIL  false' '^  ok    true'
expect passing/CONTRIBUTING.md zero "an all-passing fixture exits zero" \
  '^  ok    true'
expect_help -h "-h prints usage to stdout and exits zero"
expect_help --help "--help prints usage to stdout and exits zero"

rm -f out.log err.log
if [ "$status" -eq 0 ]; then
  echo "preflight selftest: 4 expectation(s), the runner accounts for failures and help"
fi
exit "$status"
