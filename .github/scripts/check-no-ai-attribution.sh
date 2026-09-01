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
# One carve-out, and only from the identity rules: a named dependency bot.
# The policy exists so that a human is not displaced as the author of record,
# and a dependency bump has no human author to displace — Dependabot is not an
# assistant that helped somebody write something, it is the whole author. The
# message rules still apply to its commits in full, so a bot cannot carry an
# AI co-author trailer or a "Generated with" watermark in past this.
#
# It is a hygiene guard, not a security boundary: anyone can set an author
# email locally. What stops a forged one is that it still has to survive
# review and a merge, which is where attribution is actually judged.
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
#   # and the dependency-bot carve-out, which must pass on identity but still
#   # fail on a watermark in the message:
#   git -c user.name='dependabot[bot]' \
#       -c user.email='49699333+dependabot[bot]@users.noreply.github.com' \
#       commit --allow-empty -m "build(deps): bump x" -m "Signed-off-by: dependabot[bot] <support@github.com>"
#   .github/scripts/check-no-ai-attribution.sh main..HEAD  # still just the one failure
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

# Automation identities exempt from the *identity* rules above (never from the
# message rules). Anchored at both ends and matched against the whole
# lowercased address: a substring rule here would be a hole a crafted local
# part could walk through. Matched by name rather than by GitHub's numeric user
# id, so the exemption survives an id change; add another bot by naming it.
trusted_bot_emails='^([0-9]+\+)?(dependabot|dependabot-preview)\[bot\]@users\.noreply\.github\.com$'

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
    # Skip the identity rules for a named dependency bot in this role. The
    # message rules ran above and applied to this commit like any other.
    if printf '%s\n' "$email" | tr '[:upper:]' '[:lower:]' | grep -Eq "$trusted_bot_emails"; then
      continue
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
