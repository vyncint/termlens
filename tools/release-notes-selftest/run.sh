#!/usr/bin/env bash
# Proves the release-notes extraction carries the links a section uses,
# before a release is cut from it (#658).
#
# release.yml builds a GitHub Release's notes from one CHANGELOG.md section,
# and Keep a Changelog defines reference links once, at the foot of the
# file. The section was copied without them, so the v0.11.0 notes showed
# `[Unsupported]` in brackets, linked to nothing, and v0.11.1's showed
# `[Keep a Changelog]`. A fixture changelog checks the rules one by one:
#
#   - a section gets the foot definitions it names, matched as CommonMark
#     matches a label (case-insensitively), and no others
#   - a definition inside a section is not printed in the middle of it, nor
#     does it end the section, as the first definition of the foot once did
#   - a `## [` line in a code fence is not a section heading
#   - the oldest section stops above the foot; an empty [Unreleased] prints
#     nothing and passes; a missing or empty numbered section fails
#   - a `[label]` only inside a code span, of one backtick or more and
#     across a line break, or escaped, or only inside a fenced block, names
#     nothing and appends nothing (#674): the #658 entry quoted
#     `[Keep a Changelog]` and `` [`Unsupported`] `` as code, and the 0.11.8
#     notes ended with both definitions
#   - a fence closes only on a run of its character at least as long: a
#     shorter one inside, and a `## [` line after that, are still text
#   - a real reference whose text holds code, [`Thing`] or
#     [`the guide`][guide], still appends its definition. The script before
#     #674 passes this case, since it appended every mention; it is here
#     against the opposite mistake, deleting code spans whole, which loses
#     these labels and fails it
#   - a line that starts with `[label]:` under a line of text continues
#     that text, as in CommonMark, where a definition cannot interrupt a
#     paragraph. It stays in its notes and defines nothing, whether the
#     text is a list item or a paragraph, and whether or not the line above
#     it is shaped the same way; right under a heading or a closed fence it
#     is a definition (#717). Such a line vanished from its notes and stood
#     in for the real definition in every other section's
#
# and then every section of the real CHANGELOG.md, both ways: the
# definitions in its notes are exactly the ones its text names outside code,
# out of those the file defines. That is computed here in Python, by
# regex, rather than by the script's awk, so the two do not share a mistake.
# v0.11.0's `Unsupported` is asserted by name.
#
# Usage: tools/release-notes-selftest/run.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
root="$(cd ../.. && pwd)"
extract="$root/.github/scripts/extract-changelog.sh"
status=0
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

cat > "$scratch/CHANGELOG.md" <<'EOF'
# Changelog

Follows [Keep a Changelog].

## [Unreleased]

## [0.6.0] - 2026-06-01

- Quotes `[plain]`, ``[`Thing`]`` and `` [`Thing`] `` as code, and
  `[Keep a
  Changelog]` across a line, and \[guide] escaped: none of it is a link.

## [0.5.0] - 2026-05-01

- A fenced example, indented under its item:

  ```markdown
  See [plain] and [`Thing`].
  ```

## [0.4.0] - 2026-04-01

- Real links whose text holds code: [`Thing`], [`the guide`][guide],
  and [Keep a
  Changelog] across a line.

## [0.3.0] - 2026-03-01

- Example with a fence:

  ```text
  ## [0.0.9] - not a heading
  [fake]: https://example.com/not-a-definition
  ```

- Uses [`Thing`] and [the guide][GUIDE], and a [local] one.

[local]: https://example.com/local

- After the local definition, still 0.3.0.

## [0.2.0] - 2026-02-01

- Uses nothing.

## [0.1.0] - 2026-01-01

- First, linking [Keep a Changelog].

## [0.0.1] - 2025-12-01

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[`Thing`]: https://docs.rs/thing
[guide]: https://example.com/guide
[plain]: https://example.com/plain
[unused]: https://example.com/unused

[Unreleased]: https://example.com/compare/v0.3.0...HEAD
[0.3.0]: https://example.com/compare/v0.2.0...v0.3.0
EOF

# `expect <label> <version> <zero|nonzero> [expected output file]`, against
# the changelog named by $fixture.
fixture="$scratch/CHANGELOG.md"
expect() {
  label=$1
  version=$2
  want=$3
  got=0
  "$extract" "$version" "$fixture" > "$scratch/out.md" 2> "$scratch/err.log" || got=$?
  case "$want:$got" in
    zero:0 | nonzero:[1-9]*) ;;
    *)
      printf '  FAIL  %-58s exit %s, expected %s\n' "$label" "$got" "$want" >&2
      sed 's/^/        /' "$scratch/err.log" "$scratch/out.md" >&2
      status=1
      return ;;
  esac
  if [ $# -ge 4 ] && ! diff "$4" "$scratch/out.md" > "$scratch/diff.log"; then
    printf '  FAIL  %-58s output differs (< expected, > got)\n' "$label" >&2
    sed 's/^/        /' "$scratch/diff.log" >&2
    status=1
    return
  fi
  printf '  ok    %-58s exit %s\n' "$label" "$got"
}

cat > "$scratch/0.3.0.md" <<'EOF'
- Example with a fence:

  ```text
  ## [0.0.9] - not a heading
  [fake]: https://example.com/not-a-definition
  ```

- Uses [`Thing`] and [the guide][GUIDE], and a [local] one.


- After the local definition, still 0.3.0.

[local]: https://example.com/local
[`Thing`]: https://docs.rs/thing
[guide]: https://example.com/guide
EOF
expect "a section carries the definitions it names, and only those" 0.3.0 zero "$scratch/0.3.0.md"
cat > "$scratch/0.6.0.md" <<'EOF'
- Quotes `[plain]`, ``[`Thing`]`` and `` [`Thing`] `` as code, and
  `[Keep a
  Changelog]` across a line, and \[guide] escaped: none of it is a link.
EOF
expect "a label only in code spans or escaped appends nothing (#674)" 0.6.0 zero "$scratch/0.6.0.md"
cat > "$scratch/0.5.0.md" <<'EOF'
- A fenced example, indented under its item:

  ```markdown
  See [plain] and [`Thing`].
  ```
EOF
expect "a label only in a fenced block appends nothing (#674)" 0.5.0 zero "$scratch/0.5.0.md"

# A fence closes only on a run at least as long as the one that opened it:
# a shorter one inside is text, and so is a heading after it. Its own file,
# since a reader that closed the fence early misreads everything below.
mkdir -p "$scratch/nested"
cat > "$scratch/nested/CHANGELOG.md" <<'EOF'
# Changelog

## [1.0.0] - 2027-01-01

- An example of markdown, which shows a shorter fence:

  ````markdown
  ```
  ## [0.0.8] - not a heading
  [guide] is not a link here
  ````

- After the example, a real [plain] link.

## [0.9.0] - 2026-12-01

- Older.

[guide]: https://example.com/guide
[plain]: https://example.com/plain
EOF
cat > "$scratch/1.0.0.md" <<'EOF'
- An example of markdown, which shows a shorter fence:

  ````markdown
  ```
  ## [0.0.8] - not a heading
  [guide] is not a link here
  ````

- After the example, a real [plain] link.

[plain]: https://example.com/plain
EOF
fixture="$scratch/nested/CHANGELOG.md"
expect "a shorter fence inside a fence closes nothing" 1.0.0 zero "$scratch/1.0.0.md"

# A line that starts with `[label]:` is a definition only where a paragraph
# could start. Its own file: a reader that takes a wrapped line for one
# gives every section naming the label that line in place of the real one.
mkdir -p "$scratch/wrapped"
cat > "$scratch/wrapped/CHANGELOG.md" <<'EOF'
# Changelog

## [2.2.0] - 2027-04-01

A paragraph can wrap the same way, for instance onto
[guide]: read it first. The line is still text.

### Added
[local]: https://example.com/local

- Uses the [local] one, defined right under the heading.

  ```text
  a fence
  ```
[fenced]: https://example.com/fenced

- And the [fenced] one, defined right under the fence.

## [2.1.0] - 2027-03-01

- The type every test reads is now
  [`Thing`]: a view of what the emulator dropped, and it
  replaces the old list.
- Two such lines, the second under a line shaped like a definition:
  [guide]: the first, and
  [plain]: the second.

## [2.0.0] - 2027-02-01

- First, adds [`Thing`] and the [guide].

[`Thing`]: https://docs.rs/thing
[guide]: https://example.com/guide
[plain]: https://example.com/plain
EOF
cat > "$scratch/2.1.0.md" <<'EOF'
- The type every test reads is now
  [`Thing`]: a view of what the emulator dropped, and it
  replaces the old list.
- Two such lines, the second under a line shaped like a definition:
  [guide]: the first, and
  [plain]: the second.

[`Thing`]: https://docs.rs/thing
[guide]: https://example.com/guide
[plain]: https://example.com/plain
EOF
cat > "$scratch/2.0.0.md" <<'EOF'
- First, adds [`Thing`] and the [guide].

[`Thing`]: https://docs.rs/thing
[guide]: https://example.com/guide
EOF
cat > "$scratch/2.2.0.md" <<'EOF'
A paragraph can wrap the same way, for instance onto
[guide]: read it first. The line is still text.

### Added

- Uses the [local] one, defined right under the heading.

  ```text
  a fence
  ```

- And the [fenced] one, defined right under the fence.

[local]: https://example.com/local
[fenced]: https://example.com/fenced
[guide]: https://example.com/guide
EOF
fixture="$scratch/wrapped/CHANGELOG.md"
expect "a wrapped line that starts [label]: stays in its notes" 2.1.0 zero "$scratch/2.1.0.md"
expect "and does not stand in for the real definition (#717)" 2.0.0 zero "$scratch/2.0.0.md"
expect "under a heading or a fence, the same line is a definition" 2.2.0 zero "$scratch/2.2.0.md"
fixture="$scratch/CHANGELOG.md"
cat > "$scratch/0.4.0.md" <<'EOF'
- Real links whose text holds code: [`Thing`], [`the guide`][guide],
  and [Keep a
  Changelog] across a line.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[`Thing`]: https://docs.rs/thing
[guide]: https://example.com/guide
EOF
expect "a real reference whose text holds code still counts" 0.4.0 zero "$scratch/0.4.0.md"
printf -- '- Uses nothing.\n' > "$scratch/0.2.0.md"
expect "a section that names none gets none" v0.2.0 zero "$scratch/0.2.0.md"
printf -- '- First, linking [Keep a Changelog].\n\n[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/\n' > "$scratch/0.1.0.md"
expect "a section above the foot gets a foot definition" 0.1.0 zero "$scratch/0.1.0.md"
: > "$scratch/empty.md"
expect "an empty [Unreleased] prints nothing" Unreleased zero "$scratch/empty.md"
expect "an empty numbered section fails" 0.0.1 nonzero
expect "a missing section fails" 9.9.9 nonzero

# The real file, both ways: the definitions a section's notes end with are
# exactly the labels its text names outside code that the file defines.
cd "$root"
checked=0
real_ok=1
for version in $(sed -n 's/^## \[\([^]]*\)\].*/\1/p' CHANGELOG.md); do
  if ! "$extract" "$version" > "$scratch/notes.md" 2> "$scratch/err.log"; then
    printf '  FAIL  %-58s %s\n' "CHANGELOG.md $version extracts" "$(cat "$scratch/err.log")" >&2
    status=1
    real_ok=0
    continue
  fi
  checked=$((checked + 1))
  if ! python3 - "$scratch/notes.md" CHANGELOG.md > "$scratch/check.log" 2>&1 <<'PY'
import re, sys

DEF = re.compile(r" {0,3}\[([^\]]+)\]:")
HEADING = re.compile(r" {0,3}#{1,6}(\s|$)")
norm = lambda s: " ".join(s.lower().split())

def read(path):
    """Each line of a file with what it is: "code" for a fence and the
    lines inside it, "def" for a link reference definition, else "text".
    A definition cannot interrupt a paragraph, so a line shaped like one
    is text when the line above it is: text, not blank and not a heading."""
    out, fence, para = [], None, False
    for line in open(path).read().split("\n"):
        t = line.strip()
        f = re.match(r"(`{3,}|~{3,})", t)
        if fence:
            if f and f.group(1)[0] == fence[0] and len(f.group(1)) >= len(fence) and t == f.group(1):
                fence = None
            kind = "code"
        # A backtick fence's info string holds no backtick: `` `x` `` is a span.
        elif f and not (f.group(1)[0] == "`" and "`" in t[len(f.group(1)):]):
            fence, kind = f.group(1), "code"
        elif DEF.match(line) and not para:
            kind = "def"
        else:
            kind = "text"
        para = kind == "text" and bool(t) and not HEADING.match(line)
        out.append((line, kind))
    return out

notes = read(sys.argv[1])
defined = {norm(DEF.match(l).group(1)) for l, kind in read(sys.argv[2]) if kind == "def"}
appended = {norm(DEF.match(l).group(1)) for l, kind in notes if kind == "def"}

# Blocks of text outside fences: a blank line, a fence or a list item
# starts a new one, so a code span cannot pair backticks across them.
blocks, block = [], []
for line, kind in notes:
    if kind == "def":
        continue
    t = line.strip()
    if kind == "code" or not t or re.match(r"([-*+]|\d+[.)])(\s|$)", t):
        blocks.append(" ".join(block)); block = []
    if t and kind != "code":
        block.append(t)
blocks.append(" ".join(block))

named = set()
for b in blocks:
    b = re.sub(r"\\[][`]", "", b)  # escaped: literal text
    # A code span: a run of backticks to the next run exactly as long.
    b = re.sub(r"(?<!`)(`+)(?!`)(.*?)(?<!`)\1(?!`)",
               lambda m: m.group(0).replace("[", "").replace("]", ""), b)
    named |= {norm(x) for x in re.findall(r"\[([^\]]+)\]", b)}

want = named & defined
if want != appended:
    print("missing: %s; not named: %s" % (sorted(want - appended), sorted(appended - want)))
    sys.exit(1)
PY
  then
    printf '  FAIL  %-58s %s\n' "CHANGELOG.md $version" "$(cat "$scratch/check.log")" >&2
    status=1
    real_ok=0
  fi
done
if [ "$real_ok" -eq 1 ]; then
  printf '  ok    %-58s %s sections\n' "every section's notes define exactly what it names" "$checked"
fi
if "$extract" 0.11.0 | grep -Fxq '[`Unsupported`]: https://docs.rs/termlens/0.11.0/termlens/struct.Unsupported.html'; then
  printf '  ok    %-58s\n' "v0.11.0's notes define [\`Unsupported\`]"
else
  printf '  FAIL  %-58s\n' "v0.11.0's notes define [\`Unsupported\`]" >&2
  status=1
fi

if [ "$status" -eq 0 ]; then
  echo "release notes selftest: every section carries the links it uses"
fi
exit "$status"
