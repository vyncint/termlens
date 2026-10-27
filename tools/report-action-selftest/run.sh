#!/usr/bin/env bash
# Proves the report action's render step survives the workspaces it meets,
# before a green run of it is trusted (#597).
#
# The step's `run:` block is read out of .github/actions/report/action.yml
# and run as the runner runs it, under bash with `set -euo pipefail`, in a
# scratch workspace. termlens-cli is a stub that echoes its arguments: what
# is under test is the shell around the CLI, not the CLI.
#
#   - a directory nobody can read beside a .snap.new: `find` exits 1 on it,
#     and that status ended the step before the summary had a line (#597);
#     the .snap.new must still reach the summary
#   - the same inside the artifact directory, which the step searches with
#     a second `find`
#   - nothing to show, with a .snap.new under target/ that must stay out:
#     the summary says so and names INSTA_UPDATE
#
# Root reads a chmod 000 directory anyway, so as root the first two cases
# cannot fail and are skipped by name rather than passed.
#
# Usage: tools/report-action-selftest/run.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
root="$(cd ../.. && pwd)"
action="$root/.github/actions/report/action.yml"
status=0

body="$(awk '
  /^    - name: Render the screens into the summary$/ { step = 1; next }
  step && /^      run: [|]$/ { run = 1; next }
  run && /^$/ { print ""; next }
  run && /^        / { print substr($0, 9); next }
  run { exit }
' "$action")"
if [ -z "$body" ]; then
  echo "::error::no \"Render the screens into the summary\" step with a run: block in $action" >&2
  exit 1
fi

scratch="$(mktemp -d)"
locked=""
trap 'for d in $locked; do chmod 755 "$d"; done; rm -rf "$scratch"' EXIT

# `workspace <case>`: a fresh workspace, artifact directory and RUNNER_TEMP
# for one case, with the stub CLI where the install step puts the real one.
workspace() {
  ws="$scratch/$1/workspace"
  artifacts="$scratch/$1/artifacts"
  rt="$scratch/$1/runner-temp"
  mkdir -p "$ws" "$artifacts" "$rt/termlens-cli/bin"
  printf '#!/bin/sh\necho "stub termlens $*"\n' > "$rt/termlens-cli/bin/termlens"
  chmod +x "$rt/termlens-cli/bin/termlens"
}

unreadable() {
  mkdir -p "$1"
  chmod 000 "$1"
  locked="$locked $1"
}

# `expect <label> <grep pattern …>`: the step, run in the current case's
# workspace, exits 0 and its summary holds every pattern.
expect() {
  label=$1
  shift
  : > "$rt/summary.md"
  got=0
  (cd "$ws" && RUNNER_TEMP="$rt" GITHUB_STEP_SUMMARY="$rt/summary.md" \
    TERMLENS_ARTIFACT_DIR="" ARTIFACT_DIR_INPUT="$artifacts" \
    ARTIFACT_NAME=termlens-report bash -c "$body") > "$rt/out.log" 2>&1 || got=$?
  if [ "$got" -ne 0 ]; then
    printf '  FAIL  %-58s exit %s, expected 0\n' "$label" "$got" >&2
    sed 's/^/        /' "$rt/out.log" "$rt/summary.md" >&2
    status=1
    return
  fi
  for pattern in "$@"; do
    if ! grep -Fq -- "$pattern" "$rt/summary.md"; then
      printf '  FAIL  %-58s summary lacks %s\n' "$label" "$pattern" >&2
      sed 's/^/        /' "$rt/out.log" "$rt/summary.md" >&2
      status=1
      return
    fi
  done
  printf '  ok    %-58s exit %s\n' "$label" "$got"
}

# Succeeds, saying so, when the directory just made unreadable is not.
readable_anyway() {
  if ls "$1" > /dev/null 2>&1; then
    printf '  skip  %-58s %s\n' "$2" "a chmod 000 directory is readable here (root?)"
    return 0
  fi
  return 1
}

workspace beside-snap
mkdir -p "$ws/tests/snapshots"
printf 'a screen\n' > "$ws/tests/snapshots/app__main.snap.new"
unreadable "$ws/locked"
label="an unreadable directory beside a .snap.new"
readable_anyway "$ws/locked" "$label" \
  || expect "$label" "<code>./tests/snapshots/app__main.snap.new</code>"

workspace in-artifacts
printf 'a screen\n' > "$artifacts/app-1.screen.txt"
unreadable "$artifacts/locked"
label="an unreadable directory inside the artifact directory"
readable_anyway "$artifacts/locked" "$label" \
  || expect "$label" "<code>$artifacts/app-1.screen.txt</code>"

workspace nothing
mkdir -p "$ws/target/debug"
printf 'a screen\n' > "$ws/target/debug/build.snap.new"
expect "nothing to show names INSTA_UPDATE, and target/ stays out" \
  "No \`.snap.new\` files" "INSTA_UPDATE: new" ".pending-snap"

if [ "$status" -eq 0 ]; then
  echo "report action selftest: the render step writes its summary in every case"
fi
exit "$status"
