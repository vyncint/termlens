#!/usr/bin/env bash
# check-no-ai-attribution.sh — reject AI/bot attribution in commits.
#
# Policy (one line): AI assistance is welcome; AI attribution is not. Remove
# the trailer and recommit — you are the author of record.
#
# For every commit in the range this fails on any of:
#   * message (subject + body + trailers, case-insensitive):
#       - Co-authored-by trailers naming an AI/agent/bot vendor
#       - "Generated with/by" watermarks
#       - the robot emoji watermark
#       - noreply.anthropic.com anywhere
#   * author/committer identity:
#       - *[bot]@users.noreply.github.com, *@noreply.anthropic.com,
#         actions@github.com emails
#       - names containing claude/copilot/devin/aider/codex/gemini
#
# Usage:
#   .github/scripts/check-no-ai-attribution.sh <base>..<head>
#
# tests: exercise locally on a scratch branch before trusting it in CI —
#   git checkout -b scratch/attribution
#   git commit --allow-empty -s -m "test: clean commit"
#   git commit --allow-empty -s -m "test: bad commit" \
#     -m "Co-Authored-By: Example Bot <example[bot]@users.noreply.github.com>"
#   .github/scripts/check-no-ai-attribution.sh main..HEAD  # must fail once
#   git checkout - && git branch -D scratch/attribution
set -euo pipefail

range="${1:?usage: check-no-ai-attribution.sh <range>}"

POLICY="AI assistance is welcome; AI attribution is not. Remove the trailer and recommit — you are the author of record."

# Message patterns, matched case-insensitively (ERE).
msg_patterns=(
  'co-authored-by:[[:space:]]*.*\b(claude|anthropic|copilot|chatgpt|gpt|openai|cursor|devin|aider|codex|gemini|windsurf|jetbrains ai|amazon q|sweep|bot)\b'
  'generated (with|by)\b'
  'noreply\.anthropic\.com'
)

# Identity patterns (ERE). Emails are lowercased before matching.
email_pattern='(\[bot\]@users\.noreply\.github\.com$|@noreply\.anthropic\.com$|^actions@github\.com$)'
name_pattern='\b(claude|copilot|devin|aider|codex|gemini)\b'

fail=0
count=0
while IFS= read -r sha; do
  count=$((count + 1))
  msg="$(git log -1 --format='%B' "$sha")"

  for pat in "${msg_patterns[@]}"; do
    if printf '%s\n' "$msg" | grep -Eiq "$pat"; then
      echo "::error::Commit ${sha} message matches banned pattern: ${pat}"
      fail=1
    fi
  done
  if printf '%s\n' "$msg" | grep -Fq '🤖'; then
    echo "::error::Commit ${sha} message contains a robot-emoji watermark."
    fail=1
  fi

  for role in author committer; do
    if [ "$role" = author ]; then
      name="$(git log -1 --format='%an' "$sha")"
      email="$(git log -1 --format='%ae' "$sha")"
    else
      name="$(git log -1 --format='%cn' "$sha")"
      email="$(git log -1 --format='%ce' "$sha")"
    fi
    if printf '%s\n' "$email" | tr '[:upper:]' '[:lower:]' | grep -Eq "$email_pattern"; then
      echo "::error::Commit ${sha} ${role} email '${email}' is a bot/vendor identity."
      fail=1
    fi
    if printf '%s\n' "$name" | grep -Eiq "$name_pattern"; then
      echo "::error::Commit ${sha} ${role} name '${name}' is an AI/agent identity."
      fail=1
    fi
  done
done < <(git rev-list "$range")

echo "check-no-ai-attribution: inspected ${count} commit(s) in ${range}"
if [ "$fail" -ne 0 ]; then
  echo "::error::${POLICY}"
  exit 1
fi
echo "check-no-ai-attribution: OK"
