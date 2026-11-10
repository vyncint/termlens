#!/usr/bin/env bash
# Proves the report action's render step survives the workspaces it meets,
# before a green run of it is trusted (#597).
#
# The step's `run:` block is read out of .github/actions/report/action.yml
# and run as the runner runs it, under bash with `set -euo pipefail`, in a
# scratch workspace. termlens-cli is a stub: `render --text` prints the file
# as it is, `--out` creates the file it names, and anything else echoes its
# arguments. What is under test is the shell around the CLI, not the CLI.
#
#   - a directory nobody can read beside a .snap.new: `find` exits 1 on it,
#     and that status ended the step before the summary had a line (#597);
#     the .snap.new must still reach the summary
#   - the same inside the artifact directory, which the step searches with
#     a second `find`
#   - nothing to show, with a .snap.new under target/ that must stay out:
#     the summary says so and names INSTA_UPDATE
#   - a screen with a row of three backticks and a `# heading` row, beside
#     its .snap: the screen and the diff each sit verbatim in one code block
#     that nothing inside closes, and nothing leaks out as Markdown (#648)
#   - a file name holding a newline and markup: one entry, not two, with the
#     name escaped, and an artifact file name upload-artifact accepts (#648)
#   - a file name holding carriage returns around a `# heading`: one line,
#     each CR a character reference, none left raw (#708)
#   - a relative artifact directory, as `artifact-dir` or as
#     TERMLENS_ARTIFACT_DIR: cargo put the screens in a crate's directory,
#     so the step warns and its summary says why it found none (#712); an
#     absolute one, Windows drive paths included, draws no warning
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
# The step is given that directory as `artifact-dir` (`input`), with
# TERMLENS_ARTIFACT_DIR unset (`env_dir`), and must not warn (`warns`); a
# case may change any of the three before it runs the step.
workspace() {
  ws="$scratch/$1/workspace"
  artifacts="$scratch/$1/artifacts"
  rt="$scratch/$1/runner-temp"
  input="$artifacts"
  env_dir=""
  warns=0
  mkdir -p "$ws" "$artifacts" "$rt/termlens-cli/bin"
  cat > "$rt/termlens-cli/bin/termlens" <<'STUB'
#!/bin/sh
case "$1 $2" in
  "render --text") exec cat "$3" ;;
  "render --svg" | "render --html") : > "$4" ;;
esac
echo "stub termlens $*"
STUB
  chmod +x "$rt/termlens-cli/bin/termlens"
}

unreadable() {
  mkdir -p "$1"
  chmod 000 "$1"
  locked="$locked $1"
}

# `expect <label> <grep pattern …>`: the step, run in the current case's
# workspace, exits 0, prints as many `::warning` commands as `warns` says,
# and its summary holds every pattern.
expect() {
  label=$1
  shift
  : > "$rt/summary.md"
  got=0
  (cd "$ws" && RUNNER_TEMP="$rt" GITHUB_STEP_SUMMARY="$rt/summary.md" \
    TERMLENS_ARTIFACT_DIR="$env_dir" ARTIFACT_DIR_INPUT="$input" \
    ARTIFACT_NAME=termlens-report bash -c "$body") > "$rt/out.log" 2>&1 || got=$?
  if [ "$got" -ne 0 ]; then
    printf '  FAIL  %-58s exit %s, expected 0\n' "$label" "$got" >&2
    sed 's/^/        /' "$rt/out.log" "$rt/summary.md" >&2
    status=1
    return
  fi
  if [ "$(grep -c '^::warning' "$rt/out.log")" -ne "$warns" ]; then
    printf '  FAIL  %-58s %s warning(s), expected %s\n' "$label" "$(grep -c '^::warning' "$rt/out.log")" "$warns" >&2
    sed 's/^/        /' "$rt/out.log" >&2
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

# `verbatim <label> <n> <file …>`: the summary of the last case holds the
# files' contents, in this order, each as the whole of one fenced block,
# read the way CommonMark reads one — a fence closes only on a run of
# backticks at least as long as the one that opened it, with nothing after
# — and has exactly <n> entries, and no line outside a block that Markdown
# would turn into a heading.
verbatim() {
  label=$1
  entries=$2
  shift 2
  awk -v dir="$scratch/blocks" '
    function ticks(s,   n) { n = 0; while (substr(s, n + 1, 1) == "`") n++; return n }
    fence == 0 && ticks($0) >= 3 { fence = ticks($0); f = sprintf("%s/%d", dir, ++b); printf "" > f; next }
    fence > 0 && ticks($0) >= fence && $0 ~ /^`+$/ { fence = 0; close(f); next }
    fence > 0 { print > f; next }
    /^<details>/ { d++ }
    /^#/ && $0 != "## termlens: screens from the failing tests" { print "heading outside a block: " $0 > "/dev/stderr"; bad = 1 }
    END { if (fence) { print "a block is never closed" > "/dev/stderr"; bad = 1 }
          print d + 0 > (dir "/entries"); exit bad }
  ' dir="$scratch/blocks" "$rt/summary.md" 2> "$rt/verbatim.log" || {
    printf '  FAIL  %-58s %s\n' "$label" "$(cat "$rt/verbatim.log")" >&2
    sed 's/^/        /' "$rt/summary.md" >&2
    status=1
    return
  }
  if [ "$(cat "$scratch/blocks/entries")" -ne "$entries" ]; then
    printf '  FAIL  %-58s %s entries, expected %s\n' "$label" "$(cat "$scratch/blocks/entries")" "$entries" >&2
    sed 's/^/        /' "$rt/summary.md" >&2
    status=1
    return
  fi
  i=0
  for want in "$@"; do
    i=$((i + 1))
    if ! diff "$want" "$scratch/blocks/$i" > "$rt/verbatim.log" 2>&1; then
      printf '  FAIL  %-58s block %s is not %s verbatim\n' "$label" "$i" "$want" >&2
      sed 's/^/        /' "$rt/verbatim.log" "$rt/summary.md" >&2
      status=1
      return
    fi
  done
  printf '  ok    %-58s %s entries, verbatim\n' "$label" "$entries"
}

workspace fence
mkdir -p "$ws/tests/snapshots" "$scratch/blocks"
printf '%s\n' 'a screen' '```' '# not a heading' '````' 'the last row' > "$ws/tests/snapshots/app__fence.snap.new"
printf 'the old screen\n' > "$ws/tests/snapshots/app__fence.snap"
printf 'stub termlens diff --color never ./tests/snapshots/app__fence.snap ./tests/snapshots/app__fence.snap.new\n' > "$scratch/diff.want"
expect "a row of backticks stays inside its code block" "<code>./tests/snapshots/app__fence.snap.new</code>" \
  "<p>Against <code>./tests/snapshots/app__fence.snap</code>:</p>"
verbatim "  and the screen and its diff render verbatim" 1 \
  "$ws/tests/snapshots/app__fence.snap.new" "$scratch/diff.want"

workspace newline-name
rm -rf "$scratch/blocks"; mkdir -p "$scratch/blocks"
odd="$artifacts/weird
<b>&\"name.txt"
printf 'one screen\n' > "$odd"
expect "a file name with a newline is one entry, escaped" \
  "<code>$artifacts/weird&#10;&lt;b&gt;&amp;&quot;name.txt</code>"
verbatim "  holding its screen verbatim" 1 "$odd"
label="  and its artifact file has a name upload-artifact accepts"
if [ -f "$rt/termlens-report/1-weird__b_&_name.txt.svg" ]; then
  printf '  ok    %-58s\n' "$label"
else
  printf '  FAIL  %-58s artifact holds: %s\n' "$label" "$(ls "$rt/termlens-report")" >&2
  status=1
fi
if ! grep -Fq "rendered 1 screen(s)" "$rt/out.log"; then
  printf '  FAIL  %-58s %s\n' "  and the step counts one screen" "$(tail -1 "$rt/out.log")" >&2
  status=1
fi

# CommonMark ends a line at a lone CR as at an LF, so a raw `\r\r` in a
# name ended the HTML block it sits in and rendered the rest as Markdown: a
# real heading here (#708). Every CR becomes `&#13;`, and none is left.
workspace cr-name
rm -rf "$scratch/blocks"; mkdir -p "$scratch/blocks"
cr=$(printf '\r')
odd="$artifacts/x${cr}${cr}# Injected **bold**${cr}${cr}y.txt"
printf 'one screen\n' > "$odd"
expect "a file name with a carriage return is one line, escaped" \
  "<code>$artifacts/x&#13;&#13;# Injected **bold**&#13;&#13;y.txt</code>"
verbatim "  holding its screen verbatim" 1 "$odd"
label="  and no carriage return is left in the summary"
if LC_ALL=C grep -q "$cr" "$rt/summary.md"; then
  printf '  FAIL  %-58s\n' "$label" >&2
  status=1
else
  printf '  ok    %-58s\n' "$label"
fi

# cargo runs each test binary in its package's directory, so a relative
# TERMLENS_ARTIFACT_DIR puts the screens under each crate, and the step,
# which reads it from the workspace root, found none and said the failures
# left none (#712). It warns, and the summary says the directory is relative.
workspace relative
mkdir -p "$ws/crates/app/rel-art"
printf 'a screen\n' > "$ws/crates/app/rel-art/smoke-1.screen.txt"
input=rel-art
warns=1
expect "a relative artifact-dir warns, and the summary says why" \
  "\`rel-art\` is a relative path" "each package's own \`rel-art\`" "INSTA_UPDATE: new"
label="  and no longer says the failures left no screen"
if grep -Fq "left no screen behind" "$rt/summary.md"; then
  printf '  FAIL  %-58s\n' "$label" >&2
  sed 's/^/        /' "$rt/summary.md" >&2
  status=1
else
  printf '  ok    %-58s\n' "$label"
fi
input=""
env_dir=rel-art
expect "  and so does a relative TERMLENS_ARTIFACT_DIR" \
  "\`rel-art\` is a relative path"
for abs in 'D:\a\_temp\termlens' 'D:/a/_temp/termlens' '\\server\share\termlens'; do
  workspace "absolute-$(printf '%s' "$abs" | tr -c 'A-Za-z0-9' _)"
  input="$abs"
  expect "  and ${abs} is absolute, with no warning"
done

if [ "$status" -eq 0 ]; then
  echo "report action selftest: the render step writes its summary in every case"
fi
exit "$status"
