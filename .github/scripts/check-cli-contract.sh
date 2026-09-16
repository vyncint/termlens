#!/usr/bin/env bash
# The published CLI's documented contract, as exit-code assertions.
#
# `termlens --help` states three exit codes -- 0 ran, 1 diff found a
# difference, 2 termlens itself could not run -- and STABILITY.md promises
# them. Nothing checked them against an *installed* binary until #326: the
# `install` workflow verified `cargo add termlens` and never `cargo install
# termlens-cli`, so #310 (`termlens inspect --version` exiting 2) shipped in
# 0.10.0 and again in 0.10.1 and was found by a user of the published binary.
#
# Takes the binary to exercise, so the same assertions run against a release
# from crates.io in CI and against `target/debug/termlens` locally -- which
# is how you check that the assertions can fail at all:
#
#   cargo build -p termlens-cli
#   .github/scripts/check-cli-contract.sh target/debug/termlens
#
# Portability: macOS ships bash 3.2 and BSD sed. No `declare -A`, no GNU-only
# sed flags, no process substitution in the assertions.
set -euo pipefail

BIN=${1:?usage: check-cli-contract.sh <path-to-termlens>}
command -v "$BIN" >/dev/null 2>&1 || [ -x "$BIN" ] || {
  echo "check-cli-contract: $BIN is not executable" >&2
  exit 2
}

WORK=$(mktemp -d 2>/dev/null || mktemp -d -t termlens-cli-contract)
trap 'rm -rf "$WORK"' EXIT
status=0

# `<label> <expected-exit> <command…>`; runs it, compares, records.
expect() {
  label=$1
  want=$2
  shift 2
  got=0
  "$@" > "$WORK/out" 2> "$WORK/err" || got=$?
  if [ "$got" = "$want" ]; then
    printf '  ok    %-46s exit %s\n' "$label" "$got"
  else
    printf '  FAIL  %-46s exit %s, expected %s\n' "$label" "$got" "$want" >&2
    sed 's/^/        /' "$WORK/err" >&2 || true
    status=1
  fi
}

contains() {
  label=$1
  needle=$2
  file=$3
  if grep -qF -- "$needle" "$file"; then
    printf '  ok    %-46s contains %s\n' "$label" "$needle"
  else
    printf '  FAIL  %-46s does not contain %s\n' "$label" "$needle" >&2
    status=1
  fi
}

echo "cli contract: $BIN"

# --- the version, in every position. #310 was exactly this.
expect "--version"                 0 "$BIN" --version
top=$("$BIN" --version 2>/dev/null || echo "<failed>")
for sub in inspect diff render; do
  expect "$sub --version"          0 "$BIN" "$sub" --version
  sub_version=$("$BIN" "$sub" --version 2>/dev/null || echo "<failed>")
  if [ "$sub_version" = "$top" ]; then
    printf '  ok    %-46s same string as top level\n' "$sub --version"
  else
    printf '  FAIL  %-46s said %s, top level said %s\n' \
      "$sub --version" "$sub_version" "$top" >&2
    status=1
  fi
done

# --- inspect drives a real PTY: the screen on stdout, the trailer on stderr.
expect "inspect a program"         0 "$BIN" inspect sh -c 'printf hi'
"$BIN" inspect sh -c 'printf hi' > "$WORK/a.snap" 2> "$WORK/a.err" || true
contains "inspect prints the screen"          "hi"            "$WORK/a.snap"
contains "inspect prints the header"          "size: "        "$WORK/a.snap"
contains "inspect's trailer goes to stderr"   "--- exited: "  "$WORK/a.err"
if grep -q '^--- ' "$WORK/a.snap"; then
  printf '  FAIL  %-46s stdout carries a trailer line\n' "inspect > file is a screen" >&2
  status=1
else
  printf '  ok    %-46s no trailer on stdout\n' "inspect > file is a screen"
fi

# `inspect … > file` is a saved screen: what stdout carried, unedited, is
# what render and diff read below (#340). Before 0.11 the trailer followed
# the screen on stdout and nothing the CLI wrote was a file it would read.
"$BIN" inspect sh -c 'printf bye' > "$WORK/b.snap" 2>/dev/null || true
printf 'not a saved screen at all\n' > "$WORK/junk.txt"
# A file saved by a 0.10 inspect, trailer and all, still reads.
{ cat "$WORK/a.snap"; echo "--- exited: exit code 0 ---"; } > "$WORK/old.snap"
expect "a 0.10 inspect file, trailer included" 0 "$BIN" render --text "$WORK/old.snap"
expect "diff, inspect output against itself" 0 "$BIN" diff "$WORK/a.snap" "$WORK/a.snap"

# --- render, every format, from a saved screen.
expect "render --text"             0 "$BIN" render --text "$WORK/a.snap"
expect "render --svg"              0 "$BIN" render --svg  "$WORK/a.snap"
expect "render --html"             0 "$BIN" render --html "$WORK/a.snap"
expect "render --ansi"             0 "$BIN" render --ansi "$WORK/a.snap"
"$BIN" render --svg "$WORK/a.snap" > "$WORK/out.svg" 2>/dev/null || true
if head -c 4 "$WORK/out.svg" | grep -q '<svg'; then
  printf '  ok    %-46s starts with <svg\n' "render --svg"
else
  printf '  FAIL  %-46s does not start with <svg\n' "render --svg" >&2
  status=1
fi

# --- the three flags 0.11.1 added (#312, #313, #317). Here and not only in
# tests/cli.rs because this script runs against the *published* binary.
expect "render --out"              0 "$BIN" render --svg --out "$WORK/out2.svg" "$WORK/a.snap"
if cmp -s "$WORK/out.svg" "$WORK/out2.svg"; then
  printf '  ok    %-46s same bytes as stdout\n' "render --out"
else
  printf '  FAIL  %-46s differs from the stdout rendering\n' "render --out" >&2
  status=1
fi
expect "render --out, unreadable input" 2 "$BIN" render --svg --out "$WORK/never.svg" "$WORK/junk.txt"
if [ -e "$WORK/never.svg" ]; then
  printf '  FAIL  %-46s left a file behind\n' "render --out, unreadable input" >&2
  status=1
else
  printf '  ok    %-46s no partial file\n' "render --out, unreadable input"
fi
got=0
"$BIN" render --text - < "$WORK/a.snap" > "$WORK/stdin.txt" 2> "$WORK/err" || got=$?
"$BIN" render --text "$WORK/a.snap" > "$WORK/path.txt" 2>/dev/null || true
if [ "$got" = 0 ] && cmp -s "$WORK/stdin.txt" "$WORK/path.txt"; then
  printf '  ok    %-46s exit 0, same as the path\n' "render reads - as stdin"
else
  printf '  FAIL  %-46s exit %s, or differs from the path\n' "render reads - as stdin" "$got" >&2
  status=1
fi
got=0
"$BIN" diff - - < "$WORK/a.snap" > "$WORK/out" 2> "$WORK/err" || got=$?
if [ "$got" = 2 ] && grep -qF -- 'stdin is read once' "$WORK/err"; then
  printf '  ok    %-46s exit 2, says why\n' "diff - -"
else
  printf '  FAIL  %-46s exit %s, expected 2 naming stdin\n' "diff - -" "$got" >&2
  status=1
fi
expect "inspect --cwd, missing directory" 2 "$BIN" inspect --cwd "$WORK/no-such-dir" true
"$BIN" inspect --size 200x3 --cwd "$WORK" sh -c pwd > "$WORK/cwd.snap" 2>/dev/null || true
contains "inspect --cwd runs the program there" "$(basename "$WORK")" "$WORK/cwd.snap"

# --- diff, which is the one command with three meaningful exit codes.
expect "diff, same picture"        0 "$BIN" diff "$WORK/a.snap" "$WORK/a.snap"
expect "diff, different pictures"  1 "$BIN" diff "$WORK/a.snap" "$WORK/b.snap"
expect "diff, unreadable input"    2 "$BIN" diff "$WORK/junk.txt" "$WORK/a.snap"
expect "render, unreadable input"  2 "$BIN" render --text "$WORK/junk.txt"
expect "diff, missing file"        2 "$BIN" diff "$WORK/nope.snap" "$WORK/a.snap"
expect "an unknown subcommand"     2 "$BIN" nonesuch
expect "--help"                    0 "$BIN" --help

if [ "$status" -eq 0 ]; then
  echo "cli contract: PASS — every documented exit code holds"
fi
exit "$status"
