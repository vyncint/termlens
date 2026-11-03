#!/usr/bin/env bash
# Proves the markdown link gate can fail, before it is trusted to pass (#352).
#
# Each fixture models one way a link rots, and the gate is asserted to notice:
#
#   contributing-good/       what CONTRIBUTING.md legitimately does — relative
#                            links resolved from the file's own directory, an
#                            org link, a fragment, an external site
#   contributing-root-relative/  the #384 shape — a link that resolves from the
#                            repository root but not from the file, which the
#                            gate accepted while it joined relative links to
#                            the root
#   contributing-missing/    a relative link to a target that was renamed away
#   contributing-typo-org/   #351's `…/vyncint/temlens/…`, one letter short
#   readme-relative/         the packaged README with a relative target, which
#                            crates.io resolves against the crate directory
#   readme-missing/          the packaged README with a dead absolute target
#   pr-template-relative/    the pull-request template with a relative target,
#                            which GitHub resolves against the pull request
#   readme-reference-relative/  the packaged README with a relative target in
#                            a reference definition, which the gate did not
#                            read (#657)
#   contributing-reference-missing/  a reference definition to a missing file
#   contributing-tree-missing/  a `…/tree/main/<path>` URL to a missing
#                            directory, whose path the gate did not check
#   contributing-html-missing/  an HTML `src` to a missing file
#
# The first and the last assertions are the ones that keep the gate honest: a
# fixture that must pass, and the repository's own files, which must still
# pass. Usage: tools/link-gate-selftest/run.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
root="$(cd ../.. && pwd)"
gate="$root/.github/scripts/check-readme-links.sh"
status=0

# `<label> <expected: zero|nonzero> <file ...>`
expect() {
  label=$1
  want=$2
  shift 2
  got=0
  "$gate" "$@" > out.log 2>&1 || got=$?
  case "$want:$got" in
    zero:0 | nonzero:[1-9]*)
      printf '  ok    %-58s exit %s\n' "$label" "$got" ;;
    *)
      printf '  FAIL  %-58s exit %s, expected %s\n' "$label" "$got" "$want" >&2
      sed 's/^/        /' out.log >&2
      status=1 ;;
  esac
}

expect "CONTRIBUTING.md: relative links resolve from the file (#384)" zero contributing-good/CONTRIBUTING.md
expect "CONTRIBUTING.md: a link good from the root only fails (#384)" nonzero contributing-root-relative/CONTRIBUTING.md
expect "CONTRIBUTING.md: a deleted in-repo target fails" nonzero contributing-missing/CONTRIBUTING.md
expect "CONTRIBUTING.md: a misspelled org repository fails (#351)" nonzero contributing-typo-org/CONTRIBUTING.md
expect "README.md: a relative target fails (crates.io rewrites it)" nonzero readme-relative/README.md
expect "README.md: a missing absolute target fails" nonzero readme-missing/README.md
expect "PULL_REQUEST_TEMPLATE.md: a relative target fails (#369)" nonzero pr-template-relative/PULL_REQUEST_TEMPLATE.md
expect "README.md: a relative reference definition fails (#657)" nonzero readme-reference-relative/README.md
expect "CONTRIBUTING.md: a reference to a missing target fails" nonzero contributing-reference-missing/CONTRIBUTING.md
expect "CONTRIBUTING.md: a tree/main/ URL to a missing path fails" nonzero contributing-tree-missing/CONTRIBUTING.md
expect "CONTRIBUTING.md: an HTML src to a missing target fails" nonzero contributing-html-missing/CONTRIBUTING.md
expect "this repository's own files pass" zero "$root/README.md" "$root/CONTRIBUTING.md" "$root/.github/PULL_REQUEST_TEMPLATE.md"

rm -f out.log
if [ "$status" -eq 0 ]; then
  echo "link gate selftest: 12 expectation(s), the gate fails when it should"
fi
exit "$status"
