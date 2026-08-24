#!/usr/bin/env bash
# check-dco.sh — require a DCO sign-off on every commit in a range.
#
# Every non-merge commit must contain a "Signed-off-by: Name <email>" line
# whose email matches the commit AUTHOR email. Merge commits are exempt
# (the repo is squash-merge only, so there should be none anyway).
#
# Usage:
#   .github/scripts/check-dco.sh <base>..<head> [composed-sha]
#   .github/scripts/check-dco.sh <sha>          # single commit and ancestors
#
# The optional second argument names the one commit GitHub may have just
# composed itself by squash-merging a pull request — the tip of a push to
# main. See the web-flow block below for why that commit is treated
# differently, and why naming it explicitly is what keeps the check honest.
#
# tests: exercise locally on a scratch branch before trusting it in CI —
#   git checkout -b scratch/dco
#   git commit --allow-empty -m "test: signed"  -s
#   git commit --allow-empty -m "test: unsigned"
#   .github/scripts/check-dco.sh main..HEAD   # must fail on the 2nd commit
#   # and the composed-squash path, which must pass only when named:
#   git -c user.email=noreply@github.com commit --allow-empty \
#       -m "test: composed (#1)"
#   .github/scripts/check-dco.sh main..HEAD            # must fail
#   .github/scripts/check-dco.sh main..HEAD "$(git rev-parse HEAD)"  # passes
#   git checkout - && git branch -D scratch/dco
set -euo pipefail

range="${1:?usage: check-dco.sh <range> [composed-sha]}"
composed="${2:-}"
# Resolve it once, so the comparison below is sha-to-sha rather than
# text-to-text. Empty (pull_request runs) or unresolvable leaves it empty,
# which exempts nothing.
if [ -n "$composed" ]; then
  composed="$(git rev-parse --verify --quiet "${composed}^{commit}" || true)"
fi

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
    if git log -1 --format='%B' "$sha" | grep -qi '^signed-off-by:'; then
      continue
    fi

    # ...except that GitHub composes the squash message itself, and drops
    # the trailers of the commits it squashed whenever the branch contained
    # a merge commit — someone pressing "Update branch" is enough. A pull
    # request in which every commit was signed off then lands on main
    # carrying no sign-off at all, and it cannot be repaired: main is linear
    # history, non-fast-forward, and the ruleset has no bypass actors. The
    # branch is red forever over a policy that was, in fact, met.
    #
    # It was met verifiably: `commit-policy` is a required status check on
    # main, main accepts nothing except through a pull request, and the
    # pull_request run checks every commit strictly — sign-off email against
    # author email. So exempt this commit, and only this commit: the tip of
    # the push, named by the caller, subject ending in the "(#123)" GitHub
    # appends. A contributor cannot forge their way in here, because they
    # cannot be the tip of a push to main without a pull request first.
    if [ -n "$composed" ] && [ "$sha" = "$composed" ] \
       && git log -1 --format='%s' "$sha" | grep -Eq '\(#[0-9]+\)$'; then
      echo "::notice::${sha} is a squash-merge whose message GitHub composed" \
           "without the trailers of the commits it replaced. Those commits" \
           "were checked on the pull request."
      echo "  subject: $(git log -1 --format='%s' "$sha")"
      continue
    fi

    echo "::error::Merge/squash commit ${sha} carries no Signed-off-by at all."
    echo "  subject: $(git log -1 --format='%s' "$sha")"
    fail=1
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
