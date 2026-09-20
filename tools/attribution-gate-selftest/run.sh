#!/usr/bin/env bash
# Proves the AI-attribution gate can fail, before it is trusted to pass (#470).
#
# Builds a scratch history in a temporary directory and exercises each pattern
# and failure mode against .github/scripts/check-no-ai-attribution.sh:
#
#   - a clean signed commit passes
#   - an AI co-author trailer fails
#   - a "Generated with" watermark fails
#   - a robot emoji watermark fails
#   - a bot author identity fails
#   - the Dependabot identity carve-out passes
#   - the Dependabot squash-merge Co-authored-by trailer passes
#   - Dependabot carrying a watermark in its message still fails
#   - an unresolvable commit range fails (#214)
#   - an empty commit range fails (#214)
#
# Usage: tools/attribution-gate-selftest/run.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
root="$(cd ../.. && pwd)"
gate="$root/.github/scripts/check-no-ai-attribution.sh"
status=0

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

git init -q -b main "$scratch"
git -C "$scratch" config user.name "Author"
git -C "$scratch" config user.email "author@example.com"
git -C "$scratch" commit -q --allow-empty -m "initial"

# 1. Clean signed commit
git -C "$scratch" checkout -q -b case-clean main
git -C "$scratch" commit -q --allow-empty -s -m "feat: clean commit"

# 2. AI co-author trailer
git -C "$scratch" checkout -q -b case-ai-coauthor main
git -C "$scratch" commit -q --allow-empty -m "feat: commit with bot trailer" \
  -m "Co-Authored-By: Example Bot <example[bot]@users.noreply.github.com>"

# 3. Generated with watermark
git -C "$scratch" checkout -q -b case-generated-with main
git -C "$scratch" commit -q --allow-empty -m "feat: commit with watermark" \
  -m "Generated with Claude Code"

# 4. Robot emoji watermark
git -C "$scratch" checkout -q -b case-robot-emoji main
git -C "$scratch" commit -q --allow-empty -m "feat: commit with robot 🤖 emoji"

# 5. Bot author identity
git -C "$scratch" checkout -q -b case-bot-identity main
git -C "$scratch" -c user.name="Example Bot" \
  -c user.email="example[bot]@users.noreply.github.com" \
  commit -q --allow-empty -m "feat: bot author identity"

# 6. Dependabot identity carve-out
git -C "$scratch" checkout -q -b case-dependabot-clean main
git -C "$scratch" -c user.name="dependabot[bot]" \
  -c user.email="49699333+dependabot[bot]@users.noreply.github.com" \
  commit -q --allow-empty -m "build(deps): bump x" \
  -m "Signed-off-by: dependabot[bot] <support@github.com>"

# 7. Dependabot squash-merge co-author trailer
git -C "$scratch" checkout -q -b case-dependabot-squash-coauthor main
git -C "$scratch" commit -q --allow-empty -m "build(deps): bump x" \
  -m "Co-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>"

# 8. Dependabot with watermark in message
git -C "$scratch" checkout -q -b case-dependabot-watermark main
git -C "$scratch" -c user.name="dependabot[bot]" \
  -c user.email="49699333+dependabot[bot]@users.noreply.github.com" \
  commit -q --allow-empty -m "build(deps): bump x" \
  -m "Generated with SomeBot" \
  -m "Signed-off-by: dependabot[bot] <support@github.com>"

# `<label> <expected: zero|nonzero> <range>`
expect() {
  label=$1
  want=$2
  range=$3
  got=0
  (cd "$scratch" && "$gate" "$range") > "$scratch/out.log" 2>&1 || got=$?
  case "$want:$got" in
    zero:0 | nonzero:[1-9]*)
      printf '  ok    %-58s exit %s\n' "$label" "$got" ;;
    *)
      printf '  FAIL  %-58s exit %s, expected %s\n' "$label" "$got" "$want" >&2
      sed 's/^/        /' "$scratch/out.log" >&2
      status=1 ;;
  esac
}

expect "a clean signed commit passes" zero main..case-clean
expect "an AI co-author trailer fails" nonzero main..case-ai-coauthor
expect "a 'Generated with' watermark fails" nonzero main..case-generated-with
expect "a robot emoji watermark fails" nonzero main..case-robot-emoji
expect "a bot author identity fails" nonzero main..case-bot-identity
expect "the Dependabot identity carve-out passes" zero main..case-dependabot-clean
expect "the Dependabot squash co-author trailer passes" zero main..case-dependabot-squash-coauthor
expect "Dependabot with a watermark in message fails" nonzero main..case-dependabot-watermark
expect "an unresolvable range fails (#214)" nonzero main..deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
expect "an empty range fails (#214)" nonzero main..main

if [ "$status" -eq 0 ]; then
  echo "attribution gate selftest: 10 expectation(s), the gate fails when it should"
fi
exit "$status"
