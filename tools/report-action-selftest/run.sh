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
#   - six screens of about 200 KB each, 1.2 MB in all: the summary stops
#     before GitHub's 1 MiB limit, which drops a step's whole summary, says
#     how many more screens the artifact holds, and the artifact holds all
#     six (#710)
#   - a `.snap.new` and its `.snap` that are no screens — an insta value
#     snapshot beside a screen one: the pair's entry says termlens could
#     compare nothing, with no empty code block under "Against", and
#     neither the count nor the artifact line claims it was rendered (#783)
#   - `.snap.new` names near the 255 bytes a file name may hold, as a long
#     test name gives: each is shown whole, and its renderings, counter and
#     suffix included, fit in 255 bytes, cut and hashed past that, so the
#     artifact holds them and the count has them (#808)
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
# A file holding `not a screen` is one termlens cannot read: every command
# naming it exits 2, as the real CLI does, before writing anything.
for arg in "$@"; do
  if [ -f "$arg" ] && grep -q 'not a screen' "$arg"; then
    echo "stub termlens: $arg: not a termlens Screen" >&2
    exit 2
  fi
done
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

# GitHub drops a step's whole summary past 1 MiB, small entries included,
# and the step never counted what it wrote (#710). Four of these screens
# fit under the step's 900 KiB budget and a fifth would not: the summary
# shows four, names the other two, and the artifact holds all six.
workspace big
rm -rf "$scratch/blocks"; mkdir -p "$scratch/blocks"
for i in 1 2 3 4 5 6; do
  awk -v i="$i" 'BEGIN { for (r = 0; r < 2000; r++) printf "screen %d row %091d\n", i, r }' \
    > "$artifacts/big-$i.screen.txt"
done
expect "screens past 1 MiB stop before the limit and name the rest" \
  "<code>$artifacts/big-4.screen.txt</code>" \
  "2 more screens are not shown here" "termlens-report"
size=$(($(wc -c < "$rt/summary.md")))
label="  and the summary is under 900 KiB"
if [ "$size" -le 921600 ]; then
  printf '  ok    %-58s %s bytes\n' "$label" "$size"
else
  printf '  FAIL  %-58s %s bytes\n' "$label" "$size" >&2
  status=1
fi
verbatim "  holding the first four screens verbatim" 4 \
  "$artifacts/big-1.screen.txt" "$artifacts/big-2.screen.txt" \
  "$artifacts/big-3.screen.txt" "$artifacts/big-4.screen.txt"
label="  and the artifact holds all six"
if [ "$(ls "$rt/termlens-report" | grep -c '[.]svg$')" -eq 6 ]; then
  printf '  ok    %-58s\n' "$label"
else
  printf '  FAIL  %-58s artifact holds: %s\n' "$label" "$(ls "$rt/termlens-report")" >&2
  status=1
fi

# A crate that also snapshots values with insta has `.snap`/`.snap.new`
# pairs that are no screens. `diff`'s exit 2 was dropped with `|| true`, so
# the entry showed "Against" over an empty code block, which reads as no
# difference, and the step counted the pair as rendered and claimed SVG and
# HTML for it (#783).
workspace unreadable-pair
rm -rf "$scratch/blocks"; mkdir -p "$scratch/blocks"
mkdir -p "$ws/tests/snapshots"
printf 'a screen\n' > "$ws/tests/snapshots/a__one.snap.new"
printf 'the old screen\n' > "$ws/tests/snapshots/a__one.snap"
printf 'not a screen: {"answer": 42}\n' > "$ws/tests/snapshots/b__json.snap.new"
printf 'not a screen: {"answer": 41}\n' > "$ws/tests/snapshots/b__json.snap"
expect "a pair termlens cannot read says so, not an empty block" \
  "<p>Against <code>./tests/snapshots/b__json.snap</code>:</p>" \
  "(termlens could not compare these files)" \
  "SVG and HTML renderings of the 1 screen termlens could read are in the \`termlens-report\` artifact."
label="  and no code block in the summary is empty"
if awk '/^`{3,}$/ { if (open && !lines) bad = 1; open = !open; lines = 0; next } open { lines++ } END { exit bad }' "$rt/summary.md"; then
  printf '  ok    %-58s\n' "$label"
else
  printf '  FAIL  %-58s\n' "$label" >&2
  sed 's/^/        /' "$rt/summary.md" >&2
  status=1
fi
label="  and the step counts the one screen it rendered"
if grep -Fq "rendered 1 screen(s), 1 of them into the step summary; termlens could not read 1 file(s)" "$rt/out.log"; then
  printf '  ok    %-58s\n' "$label"
else
  printf '  FAIL  %-58s %s\n' "$label" "$(tail -1 "$rt/out.log")" >&2
  status=1
fi

# insta names a snapshot after its module and test, so a long test name
# makes a `.snap.new` name near the 255 bytes a file name may hold. With the
# counter ahead of it and `.html` after, its renderings' names were longer
# than that: `termlens render --out` could not create them, the artifact
# held no rendering, and the count left the screen out (#808). A name one
# byte past what `.html` fits, and one past what even `.svg` fits, are cut
# to their first 200 bytes and a hash of the whole, which keeps the two
# apart; one that fits exactly is kept whole.
workspace long-name
mkdir -p "$ws/tests/snapshots"
x231="$(printf '%0231d' 0 | tr 0 x)"
fits="app__$(printf '%0234d' 0 | tr 0 x)"
for snap in "app__${x231}_one" "app__${x231}_three" "$fits"; do
  printf 'a screen\n' > "$ws/tests/snapshots/${snap}.snap.new"
done
expect "a name near 255 bytes is shown whole" \
  "<code>./tests/snapshots/app__${x231}_one.snap.new</code>" \
  "<code>./tests/snapshots/app__${x231}_three.snap.new</code>" \
  "<code>./tests/snapshots/${fits}.snap.new</code>" \
  "SVG and HTML renderings of each are in the \`termlens-report\` artifact."
label="  and its renderings fit, cut and apart past 255 bytes"
cut="app__$(printf '%0195d' 0 | tr 0 x)"
names="$(ls "$rt/termlens-report")"
too_long="$(printf '%s\n' "$names" | awk 'length($0) > 255')"
if [ "$(printf '%s\n' "$names" | grep -c .)" -eq 6 ] && [ -z "$too_long" ] \
  && [ "$(printf '%s\n' "$names" | grep -Ec "^[0-9]-${cut}-[0-9a-f]{8}[.]svg\$")" -eq 2 ] \
  && [ "$(printf '%s\n' "$names" | grep -Ec "^[0-9]-${cut}-[0-9a-f]{8}[.]html\$")" -eq 2 ] \
  && [ "$(printf '%s\n' "$names" | grep -E "^[0-9]-${cut}-" | sed 's/^[0-9]-//' | sort -u | wc -l)" -eq 4 ] \
  && printf '%s\n' "$names" | grep -Fqx "3-${fits}.snap.new.html"; then
  printf '  ok    %-58s\n' "$label"
else
  printf '  FAIL  %-58s artifact holds: %s\n' "$label" "$names" >&2
  status=1
fi
label="  and the step counts all three"
if grep -Fq "rendered 3 screen(s), 3 of them into the step summary" "$rt/out.log"; then
  printf '  ok    %-58s\n' "$label"
else
  printf '  FAIL  %-58s %s\n' "$label" "$(tail -1 "$rt/out.log")" >&2
  status=1
fi

if [ "$status" -eq 0 ]; then
  echo "report action selftest: the render step writes its summary in every case"
fi
exit "$status"
