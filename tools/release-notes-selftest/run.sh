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
#
# and then every section of the real CHANGELOG.md: each `[label]` it names
# whose definition is in the file must have that definition in its notes.
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
[unused]: https://example.com/unused

[Unreleased]: https://example.com/compare/v0.3.0...HEAD
[0.3.0]: https://example.com/compare/v0.2.0...v0.3.0
EOF

# `expect <label> <version> <zero|nonzero> [expected output file]`
expect() {
  label=$1
  version=$2
  want=$3
  got=0
  "$extract" "$version" "$scratch/CHANGELOG.md" > "$scratch/out.md" 2> "$scratch/err.log" || got=$?
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
printf -- '- Uses nothing.\n' > "$scratch/0.2.0.md"
expect "a section that names none gets none" v0.2.0 zero "$scratch/0.2.0.md"
printf -- '- First, linking [Keep a Changelog].\n\n[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/\n' > "$scratch/0.1.0.md"
expect "a section above the foot gets a foot definition" 0.1.0 zero "$scratch/0.1.0.md"
: > "$scratch/empty.md"
expect "an empty [Unreleased] prints nothing" Unreleased zero "$scratch/empty.md"
expect "an empty numbered section fails" 0.0.1 nonzero
expect "a missing section fails" 9.9.9 nonzero

# The real file: every label a section names, if the file defines it, is
# defined in that section's notes. Code spans are left out first, since a
# `[label]` in one is text. Checked by grep rather than by the script's
# own awk, so the two do not share a mistake.
cd "$root"
grep -E '^ {0,3}\[[^]]+\]:' CHANGELOG.md | sed -E 's/^ *\[([^]]+)\]:.*/\1/' \
  | tr '[:upper:]' '[:lower:]' | sort -u > "$scratch/defined"
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
  grep -vE '^ {0,3}\[[^]]+\]:' "$scratch/notes.md" | sed -E 's/`[^`]*`//g' \
    | grep -oE '\[[^]]+\]' | tr -d '[]' | tr '[:upper:]' '[:lower:]' | sort -u > "$scratch/named" || true
  # `[`Unsupported`]` loses its code span above; the definition keeps it.
  grep -oE '\[`[^]`]+`\]' "$scratch/notes.md" | tr -d '[]' | tr '[:upper:]' '[:lower:]' \
    | sort -u >> "$scratch/named" || true
  while IFS= read -r name; do
    [ -z "$name" ] && continue
    grep -Fxq -- "$name" "$scratch/defined" || continue
    if ! grep -E '^ {0,3}\[[^]]+\]:' "$scratch/notes.md" | sed -E 's/^ *\[([^]]+)\]:.*/\1/' \
      | tr '[:upper:]' '[:lower:]' | grep -Fxq -- "$name"; then
      printf '  FAIL  %-58s [%s] has no definition in its notes\n' "CHANGELOG.md $version" "$name" >&2
      status=1
      real_ok=0
    fi
  done < "$scratch/named"
done
if [ "$real_ok" -eq 1 ]; then
  printf '  ok    %-58s %s sections\n' "every CHANGELOG.md section's references resolve" "$checked"
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
