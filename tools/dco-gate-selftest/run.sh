#!/usr/bin/env bash
# Proves the DCO gate can fail, before it is trusted to pass (#471).
#
# Builds a scratch history in a temporary directory and exercises each rule
# and failure mode of .github/scripts/check-dco.sh:
#
#   - a signed commit passes
#   - an unsigned commit fails
#   - a sign-off whose email is not the author's fails
#   - a merge commit is exempt, and the commits it merged are still read
#   - a web-flow commit (committer noreply@github.com) with a sign-off passes
#     without the email match on the push run, which names the commit GitHub
#     composed: GitHub rewrites its author there
#   - on a pull request the committer proves nothing, since anyone can set
#     it: a forged web-flow committer with someone else's sign-off fails
#     (#718), and only Dependabot's own commit, its author and its sign-off
#     both Dependabot's, is let through without the match
#   - the squash GitHub composed without trailers fails unless it is named,
#     and passes when it is
#   - the exemption is one commit wide: naming the tip exempts no other
#     commit, an unresolvable name exempts nothing, a name whose subject has
#     no "(#N)" exempts nothing, and naming a commit GitHub did not commit
#     exempts nothing
#   - an unresolvable commit range fails (#214)
#   - an empty commit range fails (#214)
#
# The composed-squash exemption is a deliberate hole in the gate, and the
# half of these that matters most: one that silently widened would let
# unsigned commits onto main and look green doing it.
#
# Usage: tools/dco-gate-selftest/run.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
root="$(cd ../.. && pwd)"
gate="$root/.github/scripts/check-dco.sh"
status=0

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

git init -q -b main "$scratch"
git -C "$scratch" config user.name "Author"
git -C "$scratch" config user.email "author@example.com"
# A signing key in the caller's global config is not what is under test.
git -C "$scratch" config commit.gpgsign false
git -C "$scratch" commit -q --allow-empty -m "initial"

# What GitHub writes when it squash-merges a pull request: the merging
# account's address as the author, itself as the committer.
web_flow() {
  GIT_AUTHOR_NAME="Author" \
  GIT_AUTHOR_EMAIL="12345+author@users.noreply.github.com" \
  GIT_COMMITTER_NAME="GitHub" \
  GIT_COMMITTER_EMAIL="noreply@github.com" \
    git -C "$scratch" commit -q --allow-empty "$@"
}

# 1. Signed
git -C "$scratch" checkout -q -b case-signed main
git -C "$scratch" commit -q --allow-empty -s -m "feat: signed"

# 2. Unsigned
git -C "$scratch" checkout -q -b case-unsigned main
git -C "$scratch" commit -q --allow-empty -m "feat: unsigned"

# 3. Signed off by an address that is not the author's
git -C "$scratch" checkout -q -b case-mismatched main
git -C "$scratch" commit -q --allow-empty -m "feat: signed as someone else" \
  -m "Signed-off-by: Author <someone-else@example.com>"

# 4. An unsigned merge of two signed commits
git -C "$scratch" checkout -q -b case-merge-side main
git -C "$scratch" commit -q --allow-empty -s -m "feat: side"
git -C "$scratch" checkout -q -b case-merge main
git -C "$scratch" commit -q --allow-empty -s -m "feat: mainline"
git -C "$scratch" merge -q --no-ff --no-edit -m "Merge side" case-merge-side

# 5. An unsigned merge that brings in an unsigned commit
git -C "$scratch" checkout -q -b case-merge-unsigned-side main
git -C "$scratch" commit -q --allow-empty -m "feat: side, unsigned"
git -C "$scratch" checkout -q -b case-merge-unsigned main
git -C "$scratch" commit -q --allow-empty -s -m "feat: mainline"
git -C "$scratch" merge -q --no-ff --no-edit -m "Merge side" case-merge-unsigned-side

# 6. A web-flow squash that kept the sign-off of the commit it replaced
git -C "$scratch" checkout -q -b case-web-flow-signed main
web_flow -m "feat: squashed (#1)" \
  -m "Signed-off-by: Author <author@example.com>"

# 7. A web-flow squash whose message GitHub composed without trailers
git -C "$scratch" checkout -q -b case-composed main
web_flow -m "feat: composed (#2)"

# 7b. A squash that kept its sign-off, then the composed squash at the tip:
#     the history a first push to main walks
git -C "$scratch" checkout -q -b case-composed-over-web-flow main
web_flow -m "feat: squashed (#7)" \
  -m "Signed-off-by: Author <author@example.com>"
web_flow -m "feat: composed (#8)"

# 7c. Two squashes in one push, each signed off by its contributor and
#     neither by the address GitHub wrote as its author: a push to main
#     carries several when queued runs were dropped (#656), and only the
#     tip can be named
git -C "$scratch" checkout -q -b case-two-signed-squashes main
web_flow -m "feat: one contributor's change (#9)" \
  -m "Signed-off-by: Author <author@example.com>"
web_flow -m "fix: another contributor's change (#10)" \
  -m "Signed-off-by: Other Person <other@example.org>"

# 8. An unsigned commit, then the composed squash at the tip
git -C "$scratch" checkout -q -b case-composed-over-unsigned main
git -C "$scratch" commit -q --allow-empty -m "feat: unsigned"
web_flow -m "feat: composed (#3)"

# 9. Two composed squashes: only the tip can be named
git -C "$scratch" checkout -q -b case-two-composed main
web_flow -m "feat: composed (#4)"
web_flow -m "feat: composed (#5)"

# 10. A web-flow commit without a sign-off whose subject names no pull request
git -C "$scratch" checkout -q -b case-composed-no-pr main
web_flow -m "feat: composed without a pull request number"

# 11. An unsigned commit that looks composed but was committed by a person
git -C "$scratch" checkout -q -b case-not-web-flow main
git -C "$scratch" commit -q --allow-empty -m "feat: looks composed (#6)"

# 12. A pull request commit whose committer was forged as GitHub's, signed
#     off by someone other than its author (#718)
git -C "$scratch" checkout -q -b case-forged-committer main
GIT_COMMITTER_EMAIL="noreply@github.com" git -C "$scratch" commit -q --allow-empty \
  -m "feat: forged committer" \
  -m "Signed-off-by: Someone Else <someone@else.example>"

# What Dependabot pushes to its pull request branch: GitHub commits it, and
# its sign-off is support@github.com, never its author address.
dependabot() {
  GIT_AUTHOR_NAME="dependabot[bot]" \
  GIT_AUTHOR_EMAIL="49699333+dependabot[bot]@users.noreply.github.com" \
  GIT_COMMITTER_NAME="GitHub" \
  GIT_COMMITTER_EMAIL="noreply@github.com" \
    git -C "$scratch" commit -q --allow-empty "$@"
}

# 13. Dependabot's own commit
git -C "$scratch" checkout -q -b case-dependabot main
dependabot -m "chore(deps): bump a crate" \
  -m "Signed-off-by: dependabot[bot] <support@github.com>"

# 14. Dependabot as the author, a person's sign-off
git -C "$scratch" checkout -q -b case-dependabot-signed-by-someone main
dependabot -m "chore(deps): bump a crate" \
  -m "Signed-off-by: Someone Else <someone@else.example>"

tip() { git -C "$scratch" rev-parse "$1"; }

# `<label> <expected: zero|nonzero> <needle> <range> [composed-sha]`
# The needle is a line the gate must print, so a failure is a failure for
# the reason the case is about: its two #214 guards back each other up, and
# the gate also exits non-zero when it crashes, so an exit code alone would
# stay green with either guard deleted.
expect() {
  label=$1
  want=$2
  needle=$3
  shift 3
  got=0
  (cd "$scratch" && "$gate" "$@") > "$scratch/out.log" 2>&1 || got=$?
  case "$want:$got" in
    zero:0 | nonzero:[1-9]*)
      if grep -qF -- "$needle" "$scratch/out.log"; then
        printf '  ok    %-62s exit %s\n' "$label" "$got"
        return
      fi
      printf '  FAIL  %-62s exit %s, but never said: %s\n' "$label" "$got" "$needle" >&2 ;;
    *)
      printf '  FAIL  %-62s exit %s, expected %s\n' "$label" "$got" "$want" >&2 ;;
  esac
  sed 's/^/        /' "$scratch/out.log" >&2
  status=1
}

# What the gate says about one commit it refused.
unsigned() { echo "Commit $(tip "$1") has no Signed-off-by matching its author"; }
unexempt() { echo "Merge/squash commit $(tip "$1") carries no Signed-off-by at all"; }
ok="check-dco: OK"

expect "a signed commit passes" zero "$ok" main..case-signed
expect "an unsigned commit fails" nonzero "$(unsigned case-unsigned)" \
  main..case-unsigned
expect "a sign-off with another email than the author's fails" nonzero \
  "$(unsigned case-mismatched)" main..case-mismatched
expect "an unsigned merge commit is exempt" zero "$ok" main..case-merge
expect "an unsigned commit behind a merge still fails" nonzero \
  "$(unsigned case-merge-unsigned-side)" main..case-merge-unsigned
expect "a web-flow commit with a sign-off passes on the push run" zero "$ok" \
  main..case-web-flow-signed "$(tip case-web-flow-signed)"
expect "...and under the named tip, as a first push walks all of main" zero \
  "$ok" main..case-composed-over-web-flow "$(tip case-composed-over-web-flow)"
expect "two signed squashes in one push both pass (#656)" zero "$ok" \
  main..case-two-signed-squashes "$(tip case-two-signed-squashes)"
expect "on a pull request, a web-flow commit's sign-off must match" \
  nonzero "$(unsigned case-web-flow-signed)" main..case-web-flow-signed
expect "a forged web-flow committer, another's sign-off: fails (#718)" \
  nonzero "$(unsigned case-forged-committer)" main..case-forged-committer
expect "...but passes on the push run, reached only through a PR" zero \
  "$ok" main..case-forged-committer "$(tip case-forged-committer)"
expect "Dependabot's own commit passes on a pull request" zero "$ok" \
  main..case-dependabot
expect "Dependabot as author, a person's sign-off: fails on a PR" \
  nonzero "$(unsigned case-dependabot-signed-by-someone)" \
  main..case-dependabot-signed-by-someone
expect "the composed squash fails when it is not named" nonzero \
  "$(unsigned case-composed)" main..case-composed
expect "the composed squash passes when it is named" zero "$ok" \
  main..case-composed "$(tip case-composed)"
expect "naming the tip does not exempt an unsigned commit under it" nonzero \
  "$(unsigned case-composed-over-unsigned~1)" \
  main..case-composed-over-unsigned "$(tip case-composed-over-unsigned)"
expect "naming the tip does not exempt a composed commit under it" nonzero \
  "$(unexempt case-two-composed~1)" \
  main..case-two-composed "$(tip case-two-composed)"
expect "naming the commit under the tip does not exempt the tip" nonzero \
  "$(unexempt case-two-composed)" \
  main..case-two-composed "$(tip case-two-composed~1)"
expect "an unresolvable name exempts nothing" nonzero \
  "$(unexempt case-composed)" \
  main..case-composed deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
expect "a named commit whose subject has no (#N) is not exempt" nonzero \
  "$(unexempt case-composed-no-pr)" \
  main..case-composed-no-pr "$(tip case-composed-no-pr)"
expect "naming a commit GitHub did not commit exempts nothing" nonzero \
  "$(unsigned case-not-web-flow)" \
  main..case-not-web-flow "$(tip case-not-web-flow)"
expect "an unresolvable range fails (#214)" nonzero \
  "could not resolve the commit range" \
  main..deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
expect "an empty range fails (#214)" nonzero "resolved to no commits" main..main

if [ "$status" -eq 0 ]; then
  echo "dco gate selftest: 23 expectation(s), the gate fails when it should"
fi
exit "$status"
