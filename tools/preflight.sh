#!/usr/bin/env bash
# tools/preflight.sh — run CONTRIBUTING §1's gates, one command to type
# before pushing (#368).
#
# Each gate runs in the order §1 lists it; one line per gate says ok, FAIL
# or skip. A failure does not stop the run, so one broken gate cannot hide
# the rest, and the exit status is non-zero if any gate failed.
#
# The list is read from CONTRIBUTING §1 through .github/scripts/
# extract-gates.sh — the same extraction check-ci-gates-listed.sh compares
# against ci.yml — so this runner cannot drift from the documented list.
# §1's fences also document setup and snapshot-review steps (clone, cd,
# `cargo install cargo-insta`, `cargo insta review`); those are instructions
# for the reader, not gates, and are skipped below.
#
# A gate whose optional tooling is not installed is skipped by name, never
# failed: cargo-deny, cargo-semver-checks, pipx, the 1.85 toolchain, the
# Windows target. `--fast` runs only the fmt, clippy and test gates, by
# matching the documented command, so a fmt/clippy/test line added to §1 is
# picked up without touching this script.
#
# Usage: tools/preflight.sh [--fast] [-h|--help]
#   PREFLIGHT_DOC   the document to read; tools/preflight-selftest/run.sh
#                   points it at a fixture. Default CONTRIBUTING.md.
# Exit status: 0 when no gate failed, 1 when at least one did, 2 on misuse.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

doc="${PREFLIGHT_DOC:-CONTRIBUTING.md}"
fast=0
while [ $# -gt 0 ]; do
  case "$1" in
    --fast) fast=1 ;;
    -h | --help)
      printf 'usage: tools/preflight.sh [--fast] [-h|--help]\n'
      printf '%s\n' '--fast runs only formatting, clippy and tests.'
      exit 0
      ;;
    *)
      echo "usage: tools/preflight.sh [--fast] [-h|--help]" >&2
      exit 2
      ;;
  esac
  shift
done

have() { command -v "$1" >/dev/null 2>&1; }

# `rustup toolchain list` names a version either `1.85-<host>` or
# `1.85.0-<host>`; matching the separator too keeps 1.850 out.
rustup_toolchain_has() {
  rustup toolchain list 2>/dev/null | grep -Eq "^$1([.-]|$)"
}

rustup_target_has() {
  rustup target list --installed 2>/dev/null | grep -qx -- "$1"
}

# The doc cannot say which gates need tooling that may not be installed — it
# has to stay a plain command list for gates-listed — so the condition lives
# here, matched on the documented command. Prints a reason and succeeds when
# the gate should be skipped; prints nothing and fails otherwise.
skip_reason() {
  case "$1" in
    "cargo deny "*)
      have cargo-deny || {
        echo "cargo-deny is not installed (cargo install cargo-deny)"
        return 0
      }
      ;;
    "pipx "*)
      have pipx || {
        echo "pipx is not installed"
        return 0
      }
      ;;
    "cargo +1.85 "*)
      rustup_toolchain_has 1.85 || {
        echo "toolchain 1.85 is not installed (rustup toolchain install 1.85)"
        return 0
      }
      ;;
    *"--target x86_64-pc-windows-msvc"*)
      rustup_target_has x86_64-pc-windows-msvc || {
        echo "target x86_64-pc-windows-msvc is not installed (rustup target add x86_64-pc-windows-msvc)"
        return 0
      }
      ;;
    "tools/semver-gate-selftest/run.sh" | ".github/scripts/check-semver.sh "*)
      have cargo-semver-checks || {
        echo "cargo-semver-checks is not installed (cargo install cargo-semver-checks)"
        return 0
      }
      ;;
  esac
  return 1
}

fast_runs() {
  case "$1" in
    "cargo fmt "* | "cargo clippy "* | "cargo test "*) return 0 ;;
  esac
  return 1
}

cmds="$(.github/scripts/extract-gates.sh "$doc")"
ok=0
skipped=0
failed=0
total=0
failed_cmds=""

while IFS= read -r cmd; do
  [ -z "$cmd" ] && continue
  case "$cmd" in
    "git clone "* | "cd "* | "cargo install "* | "cargo insta "*) continue ;;
  esac
  if [ "$fast" -eq 1 ] && ! fast_runs "$cmd"; then
    continue
  fi
  total=$((total + 1))
  if reason="$(skip_reason "$cmd")"; then
    skipped=$((skipped + 1))
    printf '  skip  %-58s %s\n' "$cmd" "$reason"
    continue
  fi
  code=0
  sh -c "$cmd" || code=$?
  if [ "$code" -eq 0 ]; then
    ok=$((ok + 1))
    printf '  ok    %-58s exit %s\n' "$cmd" "$code"
  else
    failed=$((failed + 1))
    failed_cmds="${failed_cmds}${cmd}"$'\n'
    printf '  FAIL  %-58s exit %s\n' "$cmd" "$code" >&2
  fi
done <<<"$cmds"

printf 'preflight: %s ok, %s skipped, %s failed of %s gate(s) in %ss\n' \
  "$ok" "$skipped" "$failed" "$total" "$SECONDS"

if [ "$failed" -gt 0 ]; then
  echo "failed:"
  while IFS= read -r cmd; do
    [ -n "$cmd" ] && printf '  %s\n' "$cmd"
  done <<<"$failed_cmds"
  exit 1
fi
