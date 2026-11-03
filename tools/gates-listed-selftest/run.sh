#!/usr/bin/env bash
# Proves the gates-listed gate can fail, before a green run of it is
# trusted (#657).
#
# Each case appends one step to a copy of this repository's ci.yml and runs
# the gate against it and the real CONTRIBUTING.md. The gate read only lines
# spelled `- run: cargo …`, and every shape below that must fail passed:
#
#   - a gate under a `- name:` key, and one in double or single quotes
#   - a gate in a folded `run: >-` block, and one in a plain scalar that
#     starts on the line after `run:`
#   - a single-line command that is neither a gate nor named plumbing
#
# and the shapes that must still pass: a listed gate that is named, quoted
# or carries a trailing comment, and the repository's own ci.yml, whose
# multi-line blocks are the named plumbing. Dropping a listed line from a
# copy of CONTRIBUTING.md must fail, as it always did.
#
# Usage: tools/gates-listed-selftest/run.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
root="$(cd ../.. && pwd)"
gate="$root/.github/scripts/check-ci-gates-listed.sh"
ci="$root/.github/workflows/ci.yml"
doc="$root/CONTRIBUTING.md"
status=0
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

# `expect <label> <zero|nonzero> <ci> <doc>`
expect() {
  label=$1
  want=$2
  got=0
  "$gate" "$3" "$4" > "$scratch/out.log" 2>&1 || got=$?
  case "$want:$got" in
    zero:0 | nonzero:[1-9]*)
      printf '  ok    %-58s exit %s\n' "$label" "$got" ;;
    *)
      printf '  FAIL  %-58s exit %s, expected %s\n' "$label" "$got" "$want" >&2
      sed 's/^/        /' "$scratch/out.log" >&2
      status=1 ;;
  esac
}

# `with_step <label> <zero|nonzero> <step line ...>`: ci.yml with one more
# step at the end, at the steps' indentation, against the real doc.
with_step() {
  label=$1
  want=$2
  shift 2
  cp "$ci" "$scratch/ci.yml"
  printf '      %s\n' "$@" >> "$scratch/ci.yml"
  expect "$label" "$want" "$scratch/ci.yml" "$doc"
}

with_step "a named, unlisted gate fails" nonzero \
  '- name: A named gate' '  run: cargo nextest run --workspace'
with_step "a double-quoted, unlisted gate fails" nonzero \
  '- run: "cargo udeps"'
with_step "a single-quoted, unlisted gate fails" nonzero \
  "- run: 'cargo udeps'"
with_step "a gate in a folded run: >- block fails" nonzero \
  '- run: >-' '    cargo nextest run' '    --workspace'
with_step "a literal run: | block that is not named plumbing fails" nonzero \
  '- name: Not on the list' '  run: |' '    cargo fmt --all --check'
with_step "a gate on the line after run: fails" nonzero \
  '- run:' '    cargo udeps'
with_step "a command that is neither gate nor plumbing fails" nonzero \
  '- run: make check'
with_step "a listed gate that is named passes" zero \
  '- name: Format' '  run: cargo fmt --all --check'
with_step "a listed gate in quotes, with a comment, passes" zero \
  '- run: "cargo fmt --all --check"' "- run: cargo fmt   --all --check  # spaced"

grep -Fv 'cargo test -p termlens --no-default-features --features regex' "$doc" > "$scratch/CONTRIBUTING.md"
expect "a gate CONTRIBUTING.md stops listing fails" nonzero "$ci" "$scratch/CONTRIBUTING.md"
expect "this repository's ci.yml and CONTRIBUTING.md pass" zero "$ci" "$doc"

if [ "$status" -eq 0 ]; then
  echo "gates-listed selftest: the gate fails on every shape it exists to catch"
fi
exit "$status"
