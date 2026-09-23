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
expect "render --json"             0 "$BIN" render --json "$WORK/a.snap"
"$BIN" render --svg "$WORK/a.snap" > "$WORK/out.svg" 2>/dev/null || true
if head -c 4 "$WORK/out.svg" | grep -q '<svg'; then
  printf '  ok    %-46s starts with <svg\n' "render --svg"
else
  printf '  FAIL  %-46s does not start with <svg\n' "render --svg" >&2
  status=1
fi

# --- the JSON is a promised format too: the document says which one, and
# render reads back what render wrote (#373).
"$BIN" render --json "$WORK/a.snap" > "$WORK/out.json" 2>/dev/null || true
contains "render --json, format 1" '"format": 1' "$WORK/out.json"
expect "render --json, read back"  0 "$BIN" render --text "$WORK/out.json"

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
# `render`'s one operand is the whole input, and the last of several used to
# win silently: with --out the wrong screen was written and nothing printed
# to show it (#364). Refused with the usage, and no file is created.
got=0
"$BIN" render --text "$WORK/a.snap" "$WORK/b.snap" > "$WORK/out" 2> "$WORK/err" || got=$?
if [ "$got" = 2 ] && grep -qF -- 'usage: termlens render' "$WORK/err"; then
  printf '  ok    %-46s exit 2, prints the usage\n' "render, two operands"
else
  printf '  FAIL  %-46s exit %s, expected 2 with the usage\n' "render, two operands" "$got" >&2
  sed 's/^/        /' "$WORK/err" >&2 || true
  status=1
fi
got=0
"$BIN" render --svg --out "$WORK/never.svg" "$WORK/a.snap" "$WORK/b.snap" \
  > "$WORK/out" 2> "$WORK/err" || got=$?
if [ "$got" = 2 ] && [ ! -e "$WORK/never.svg" ]; then
  printf '  ok    %-46s exit 2, no file\n' "render --out, two operands"
else
  printf '  FAIL  %-46s exit %s, or left a file behind\n' \
    "render --out, two operands" "$got" >&2
  status=1
fi
expect "inspect --cwd, missing directory" 2 "$BIN" inspect --cwd "$WORK/no-such-dir" true
"$BIN" inspect --size 200x3 --cwd "$WORK" sh -c pwd > "$WORK/cwd.snap" 2>/dev/null || true
contains "inspect --cwd runs the program there" "$(basename "$WORK")" "$WORK/cwd.snap"

# The child environment is bare by default; --env sets a value in it and
# --inherit-env hands the caller's over instead. Both decide which screen
# the test sees, and a regression in either is silent (#367).
expect "inspect --env, malformed pair" 2 "$BIN" inspect --env NO_EQUALS true
# The empty key is the one case where splitting on the first `=` and on the
# last disagree about whether to refuse, so this line holds the published
# binary to the first-= parse in a way the value assertions cannot (#367).
expect "inspect --env, empty key" 2 "$BIN" inspect --env '=a=b' true
"$BIN" inspect --size 60x3 --env TERMLENS_CONTRACT=zzz sh -c 'echo "[$TERMLENS_CONTRACT]"' > "$WORK/env.snap" 2>/dev/null || true
contains "inspect --env sets a variable"       "[zzz]" "$WORK/env.snap"
TERMLENS_CONTRACT=zzz "$BIN" inspect --size 60x3 sh -c 'echo "[$TERMLENS_CONTRACT]"' > "$WORK/cleared.snap" 2>/dev/null || true
contains "inspect clears the env by default"   "[]"    "$WORK/cleared.snap"
TERMLENS_CONTRACT=zzz "$BIN" inspect --size 60x3 --inherit-env sh -c 'echo "[$TERMLENS_CONTRACT]"' > "$WORK/inherited.snap" 2>/dev/null || true
contains "inspect --inherit-env keeps the caller's" "[zzz]" "$WORK/inherited.snap"

# --- `--flag=value`, for every inspect flag that takes one (#366, #457).
# Covered in tests/cli.rs against the tree; here against what users
# install, which is where #310 was found. Each `=` spelling must produce the
# same screen as the spelled-out form, not merely exit 0.
same_as_spelled_out() {
  label=$1
  eq=$2
  spelled=$3
  shift 3
  got=0
  "$BIN" inspect $eq "$@" > "$WORK/eq.snap" 2>/dev/null || got=$?
  "$BIN" inspect $spelled "$@" > "$WORK/sp.snap" 2>/dev/null || true
  if [ "$got" = 0 ] && cmp -s "$WORK/eq.snap" "$WORK/sp.snap"; then
    printf '  ok    %-46s same screen as the spelled-out form\n' "$label"
  else
    printf '  FAIL  %-46s exit %s, or differs from %s\n' "$label" "$got" "$spelled" >&2
    status=1
  fi
}
same_as_spelled_out "inspect --size="    "--size=30x3"   "--size 30x3"   sh -c 'printf hi'
same_as_spelled_out "inspect --timeout=" "--timeout=2"   "--timeout 2"   sh -c 'printf hi'
same_as_spelled_out "inspect --idle="    "--idle=300"    "--idle 300"    sh -c 'printf hi'
"$BIN" inspect --size 200x3 --cwd="$WORK" sh -c pwd > "$WORK/cwd-eq.snap" 2>/dev/null || true
contains "inspect --cwd= runs the program there" "$(basename "$WORK")" "$WORK/cwd-eq.snap"
"$BIN" inspect --size 60x3 --env=TERMLENS_CONTRACT=eq sh -c 'echo "[$TERMLENS_CONTRACT]"' > "$WORK/env-eq.snap" 2>/dev/null || true
contains "inspect --env= sets a variable"      "[eq]"  "$WORK/env-eq.snap"
# The flag's `=` is the first one; the pair then splits on *its* first. A
# last-`=` parse would read `--env=A=b=c` as a flag called `--env=A=b` and
# refuse it, so this is the line that tells the two apart.
"$BIN" inspect --size 60x3 --env=TERMLENS_CONTRACT=b=c sh -c 'echo "[$TERMLENS_CONTRACT]"' > "$WORK/env-eq2.snap" 2>/dev/null || true
contains "inspect --env=K=a=b splits on the first =" "[b=c]" "$WORK/env-eq2.snap"
expect "inspect --inherit-env=, a value on a bare flag" 2 "$BIN" inspect --inherit-env=nonsense true

# --- a program that outlives the wait (#374, #479). The wait ends two ways,
# so a still-running child has two trailers, and both must be stripped from
# a saved screen that carries one -- `strip_inspect_trailer` names them,
# and nothing else ran that list against an installed binary.
#
# The sleeps are in the *program under inspection*: it has to outlive the
# wait for the trailer to exist at all. The deadlines are generous against
# what each run measures: the first ends on 300ms of silence well inside a
# 20s ceiling; the second prints every second, so a 3s silence window can
# never close and only the 2s deadline can end it.
got=0
"$BIN" inspect --size 20x2 --idle 300 --timeout 20 sh -c 'printf hi; sleep 30' \
  > "$WORK/idle.snap" 2> "$WORK/idle.err" || got=$?
if [ "$got" = 0 ] && grep -qF -- '--- still running (killed on exit) ---' "$WORK/idle.err"; then
  printf '  ok    %-46s exit 0, trailer on stderr\n' "inspect, silence ends the wait"
else
  printf '  FAIL  %-46s exit %s, or no still-running trailer\n' "inspect, silence ends the wait" "$got" >&2
  sed 's/^/        /' "$WORK/idle.err" >&2 || true
  status=1
fi
got=0
"$BIN" inspect --size 20x2 --idle 3000 --timeout 2 sh -c 'while :; do printf .; sleep 1; done' \
  > "$WORK/deadline.snap" 2> "$WORK/deadline.err" || got=$?
if [ "$got" = 0 ] && grep -qF -- '--- still running at the deadline (killed on exit) ---' "$WORK/deadline.err"; then
  printf '  ok    %-46s exit 0, the at-the-deadline form\n' "inspect, the deadline ends the wait"
else
  printf '  FAIL  %-46s exit %s, or not the deadline trailer\n' "inspect, the deadline ends the wait" "$got" >&2
  sed 's/^/        /' "$WORK/deadline.err" >&2 || true
  status=1
fi
for run in idle deadline; do
  if grep -q '^--- ' "$WORK/$run.snap"; then
    printf '  FAIL  %-46s stdout carries a trailer line\n' "inspect, $run: stdout is a screen" >&2
    status=1
  else
    printf '  ok    %-46s no trailer on stdout\n' "inspect, $run: stdout is a screen"
  fi
  # A screen saved with its trailer attached -- `2>&1 > file`, or a log --
  # is what exercises the strip, since stdout alone never carries it.
  cat "$WORK/$run.snap" "$WORK/$run.err" > "$WORK/$run.with-trailer.snap"
  expect "a screen saved with the $run trailer reads" 0 \
    "$BIN" render --text "$WORK/$run.with-trailer.snap"
done
# And the strip is specific: an arbitrary `---` line is not a trailer, so a
# check that accepted anything would be no check at all.
{ cat "$WORK/idle.snap"; echo "--- something else entirely ---"; } > "$WORK/not-a-trailer.snap"
expect "a --- line that is no trailer is refused" 2 "$BIN" render --text "$WORK/not-a-trailer.snap"

# --- `--` ends inspect's options (#453): the one way to run a program
# whose name begins with `-`, and nothing in the help said so.
expect "inspect -- <program>"      0 "$BIN" inspect -- sh -c 'printf hi'
got=0
"$BIN" inspect -- -termlens-contract-no-such-program > "$WORK/out" 2> "$WORK/err" || got=$?
if [ "$got" = 2 ] && grep -qF -- 'failed to spawn' "$WORK/err" && ! grep -qF -- 'unknown option' "$WORK/err"; then
  printf '  ok    %-46s a flag-shaped name is a program\n' "inspect -- -name"
else
  printf '  FAIL  %-46s exit %s, or read as an option\n' "inspect -- -name" "$got" >&2
  sed 's/^/        /' "$WORK/err" >&2 || true
  status=1
fi

# --- one format and one --out (#450), the flag-position twin of the
# two-operand refusal above.
got=0
"$BIN" render --svg --html "$WORK/a.snap" > "$WORK/out" 2> "$WORK/err" || got=$?
if [ "$got" = 2 ] && [ ! -s "$WORK/out" ] && grep -qF -- 'usage: termlens render' "$WORK/err"; then
  printf '  ok    %-46s exit 2, nothing rendered\n' "render, two formats"
else
  printf '  FAIL  %-46s exit %s, expected 2 and no output\n' "render, two formats" "$got" >&2
  status=1
fi
got=0
"$BIN" render --svg --out "$WORK/one.svg" --out="$WORK/two.svg" "$WORK/a.snap" \
  > "$WORK/out" 2> "$WORK/err" || got=$?
if [ "$got" = 2 ] && [ ! -e "$WORK/one.svg" ] && [ ! -e "$WORK/two.svg" ]; then
  printf '  ok    %-46s exit 2, neither file\n' "render, two --out"
else
  printf '  FAIL  %-46s exit %s, or wrote a file\n' "render, two --out" "$got" >&2
  status=1
fi

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
