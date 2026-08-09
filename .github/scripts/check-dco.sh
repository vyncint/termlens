#!/usr/bin/env bash
# check-dco.sh — require a DCO sign-off on every commit in a range.
#
# Every non-merge commit must contain a "Signed-off-by: Name <email>" line
# whose email matches the commit AUTHOR email. Merge commits are exempt
# (the repo is squash-merge only, so there should be none anyway).
#
# Usage:
#   .github/scripts/check-dco.sh <base>..<head>
#   .github/scripts/check-dco.sh <sha>          # single commit and ancestors
#
# tests: exercise locally on a scratch branch before trusting it in CI —
#   git checkout -b scratch/dco
#   git commit --allow-empty -m "test: signed"  -s
#   git commit --allow-empty -m "test: unsigned"
#   .github/scripts/check-dco.sh main..HEAD   # must fail on the 2nd commit
#   git checkout - && git branch -D scratch/dco
set -euo pipefail

range="${1:?usage: check-dco.sh <range>}"

fail=0
count=0
while IFS= read -r sha; do
  count=$((count + 1))
  author_email="$(git log -1 --format='%ae' "$sha")"
  committer_email="$(git log -1 --format='%ce' "$sha")"

  # GitHub web-flow commits (squash/merge performed by github.com itself,
  # committer noreply@github.com) rewrite the author email to the merging
  # account's GitHub address, so an exact sign-off==author match is
  # impossible by construction. The underlying PR commits were already
  # DCO-checked by this workflow's required pull_request run; for the
  # resulting merge commit we require a sign-off to be present but skip
  # the email match.
  if [ "$committer_email" = "noreply@github.com" ]; then
    if ! git log -1 --format='%B' "$sha" | grep -qi '^signed-off-by:'; then
      echo "::error::Merge/squash commit ${sha} carries no Signed-off-by at all."
      echo "  subject: $(git log -1 --format='%s' "$sha")"
      fail=1
    fi
    continue
  fi

  # The sign-off may sit anywhere in the message body: after a squash merge,
  # GitHub concatenates commit messages, which moves trailers out of the
  # strict trailer block. Match any "Signed-off-by:" line instead.
  if ! git log -1 --format='%B' "$sha" \
      | grep -i '^signed-off-by:' \
      | grep -qF "<${author_email}>"; then
    echo "::error::Commit ${sha} has no Signed-off-by matching its author <${author_email}>."
    echo "  subject: $(git log -1 --format='%s' "$sha")"
    echo "  fix: 'git commit --amend -s' for the last commit, or" \
         "'git rebase --signoff' for a branch, then force-push."
    fail=1
  fi
done < <(git rev-list --no-merges "$range")

echo "check-dco: inspected ${count} commit(s) in ${range}"
if [ "$fail" -ne 0 ]; then
  echo "::error::DCO check failed. Every commit must be signed off (git commit -s)." \
       "See CONTRIBUTING.md §5 and https://developercertificate.org"
  exit 1
fi
echo "check-dco: OK"
