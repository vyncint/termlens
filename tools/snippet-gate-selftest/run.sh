#!/usr/bin/env bash
# Proves the snippet gate reads every Rust block it is meant to, before a
# green run of it is trusted (#657, #660).
#
# The gate compiles what it extracts, so a block it never extracts is a
# block it never checks: it matched only an unindented, bare ```rust, and a
# fake skill with a broken ```rust,no_run block and a broken block indented
# under a list item passed. Each case below runs the gate with
# SNIPPETS_EXTRACT_ONLY=1, which stops after the extraction, so no
# toolchain is needed; what it would then compile is asserted directly.
#
#   - ```rust,no_run, ```rust,should_panic and ```rs fail by name: a tag is
#     skipped only by saying ,ignore, so nothing is hidden by a variant
#   - a ```rust block indented under a list item is extracted, with the
#     list's indentation removed and nothing else
#   - ```rust,ignore is skipped, and a ```rust line inside a ````text
#     example opens nothing
#   - a fence left open fails rather than swallowing the rest of the file
#   - a README block without #[test] is wrapped as the body of a function
#     with `t` and `screen` in scope; one with #[test] is left whole
#   - the repository's own SKILL.md and README.md extract cleanly
#
# Usage: tools/snippet-gate-selftest/run.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
root="$(cd ../.. && pwd)"
gate="$root/.github/scripts/check-skill-snippets.sh"
status=0
scratch="$(mktemp -d)"
gate_tmp=""
trap 'rm -rf "$scratch" $gate_tmp' EXIT

good_readme="$scratch/good-README.md"
printf '%s\n' '```rust' 'let x = 1;' '```' > "$good_readme"

# `run_gate <skill> <readme>`: the gate in extraction-only mode; sets `got`
# to its exit status and `snippets` to the directory it extracted into.
run_gate() {
  got=0
  SNIPPETS_EXTRACT_ONLY=1 "$gate" "$1" "$2" > "$scratch/out.log" 2>&1 || got=$?
  snippets="$(sed -n 's/^snippets under //p' "$scratch/out.log")"
  [ -n "$snippets" ] && gate_tmp="$gate_tmp $snippets"
  return 0
}

fail() {
  printf '  FAIL  %-58s %s\n' "$1" "$2" >&2
  sed 's/^/        /' "$scratch/out.log" >&2
  status=1
}

ok() { printf '  ok    %-58s exit %s\n' "$1" "$got"; }

# `refuses <label> <needle> <skill body line ...>`: the gate exits non-zero
# on a SKILL.md made of these lines, and says <needle>.
refuses() {
  label=$1
  needle=$2
  shift 2
  printf '%s\n' "$@" > "$scratch/SKILL.md"
  run_gate "$scratch/SKILL.md" "$good_readme"
  if [ "$got" -eq 0 ]; then
    fail "$label" "exit 0, expected non-zero"
  elif ! grep -Fq -- "$needle" "$scratch/out.log"; then
    fail "$label" "output lacks: $needle"
  else
    ok "$label"
  fi
}

refuses "a \`\`\`rust,no_run block fails by name" '```rust,no_run is neither' \
  '# skill' '```rust,no_run' 'fn broken() -> u8 { "not a u8" }' '```'
refuses "a \`\`\`rust,should_panic block fails by name" '```rust,should_panic is neither' \
  '```rust,should_panic' '#[test] fn t() { panic!() }' '```'
refuses "a \`\`\`rs block fails by name" '```rs is neither' \
  '```rs' 'fn f() {}' '```'
refuses "a fence left open fails" 'never closed' \
  '```rust' '#[test] fn t() {}' '```' '' '```text' 'the rest of the file'

# `extracts <label> <file> <expected file …>`: the last run extracted
# exactly these files, each with the expected content.
extracts() {
  label=$1
  dir=$2
  shift 2
  if [ "$got" -ne 0 ]; then
    fail "$label" "exit $got, expected 0"
    return
  fi
  want_n=$#
  have_n="$(find "$dir" -name 'snippet_*.rs' | wc -l | tr -d ' ')"
  if [ "$have_n" -ne "$want_n" ]; then
    fail "$label" "extracted $have_n block(s), expected $want_n"
    return
  fi
  i=0
  for expected in "$@"; do
    i=$((i + 1))
    file="$dir/$(printf 'snippet_%02d.rs' "$i")"
    if ! printf '%s\n' "$expected" | diff - "$file" > "$scratch/diff.log"; then
      cat "$scratch/diff.log" >> "$scratch/out.log"
      fail "$label" "$(basename "$file") differs from what was written"
      return
    fi
  done
  ok "$label"
}

printf '%s\n' \
  '1. A step in a list:' \
  '' \
  '   ```rust' \
  '   #[test]' \
  '   fn indented() {' \
  '       let broken: u8 = "not a u8";' \
  '   }' \
  '   ```' \
  '' \
  '```rust,ignore' \
  'a fragment that is never compiled' \
  '```' \
  '' \
  '````text' \
  '```rust' \
  'an example of markdown, not Rust' \
  '```' \
  '````' > "$scratch/SKILL.md"
run_gate "$scratch/SKILL.md" "$good_readme"
extracts "an indented block is extracted; ignore and text are not" "$snippets/skill/tests" \
  "$(printf '%s\n' '#[test]' 'fn indented() {' '    let broken: u8 = "not a u8";' '}')"

printf '%s\n' \
  '```rust' '#[test]' 'fn whole() {}' '```' \
  '```rust' 'let s = screen.text();' '```' > "$scratch/README.md"
printf '%s\n' '```rust' '#[test] fn t() {}' '```' > "$scratch/SKILL.md"
run_gate "$scratch/SKILL.md" "$scratch/README.md"
extracts "a README fragment is wrapped; a whole test is not" "$snippets/readme/tests" \
  "$(printf '%s\n' '#[test]' 'fn whole() {}')" \
  "$(printf '%s\n' '#![allow(unused)]' 'use termlens::{Screen, Terminal};' \
    'fn fragment(mut t: Terminal, screen: Screen) -> termlens::Result<()> {' \
    'let s = screen.text();' '    Ok(())' '}')"

run_gate "$root/skills/termlens/SKILL.md" "$root/README.md"
if [ "$got" -ne 0 ]; then
  fail "this repository's SKILL.md and README.md extract" "exit $got, expected 0"
else
  ok "this repository's SKILL.md and README.md extract"
fi

if [ "$status" -eq 0 ]; then
  echo "snippet gate selftest: the gate reads every Rust block and refuses the rest"
fi
exit "$status"
