#!/usr/bin/env bash
# Every link in the markdown we ship, or hand a contributor, must point at
# something that exists.
#
# The rule each file gets depends on where it is rendered from, for a reason
# worth knowing before changing either half:
#
# * `README.md` is packaged. `crates/termlens/Cargo.toml` sets
#   `readme = "../../README.md"`, so crates.io renders it as the crate's
#   README and rewrites a *relative* link against the crate's directory in the
#   repository rather than the repository root -- `docs/DESIGN.md` became
#   `…/crates/termlens/docs/DESIGN.md`, and every one of the ten relative
#   links on the 0.10.2 page was a 404. Nothing in this repository could see
#   it, because the same file renders correctly on GitHub, where it *is* at
#   the root. So absolute links only, and that rule applies to `README.md`
#   alone.
# * `CONTRIBUTING.md` is not packaged and legitimately uses relative links
#   (`AGENTS.md`, `docs/RELEASING.md`, `.github/workflows/ci.yml`), so for it
#   the relative form is fine. The target still has to exist, and until #352
#   no file checked that.
# * `.github/PULL_REQUEST_TEMPLATE.md` is not packaged either, but GitHub
#   renders it in the body of every pull request, where a relative target
#   resolves against the pull request's URL rather than the repository root:
#   `../CONTRIBUTING.md` from `/vyncint/termlens/pull/351` is a 404 in every
#   pull request that uses the template (#369). So it gets the absolute-only
#   rule too, for a different URL than the README's.
#
# So each file gets the rule that generalises -- an in-repo target names a path
# that is really here -- and only the packaged one gets the absolute-only rule
# on top. A relative link is resolved against the linking file's own directory,
# the way GitHub, crates.io and a browser resolve it. Joining it to the
# repository root instead is what let three links in `docs/LIMITATIONS.md`
# point at `docs/docs/…` while this gate called that file green (#384).
#
# The other half is a URL naming no in-repo path at all: #351 shipped
# `…/vyncint/temlens/…` (one letter short) into `CONTRIBUTING.md` and all
# sixteen checks reported success. The account is asserted against the list
# below rather than asked over the network, deliberately: a link checker that
# asks GitHub is online, flaky, and can report a 200 for the wrong reason,
# which is the failure mode this script exists to avoid. `temlens` is not in
# the list.
#
# Usage: check-readme-links.sh [file ...]   (default: README.md)
set -euo pipefail

base="https://github.com/vyncint/termlens/blob/main/"

# The repositories a document here has any business linking: the four projects
# CONTRIBUTING.md names as sharing one contributor pattern, oxidelake and
# oxmera, which follow the same commit rules, DCO and AI policy without being
# named in that blockquote (it is copied word for word across the named
# repositories, so it changes in all of them or none), plus termlens's own demo, which
# `docs/DESIGN.md` cites for the coverage study. The account holds many more
# repositories than these; the list is deliberately the short one, because a
# list of everything would wave through the typo this gate exists to catch.
#
# So a name outside it is *usually* a typo, and occasionally a real repository
# nobody has linked before -- which is why the message below says the list is
# what failed, rather than claiming the repository does not exist. Adding one
# is a word.
known_repos="launchbound mossaic oxidelake oxmera reconverge termlens termlens-demo"

files=("$@")
if [ "${#files[@]}" -eq 0 ]; then
  files=(README.md)
fi

root=$(cd "$(dirname "$0")/../.." && pwd)
status=0
total=0

for file in "${files[@]}"; do
  if [ ! -f "$file" ]; then
    echo "LINK GATE: \"$file\" is not a file" >&2
    status=1
    continue
  fi

  # A relative link resolves against the file that contains it, the way the
  # reader's browser resolves it. At the root that is the root, so those files
  # are unaffected; under docs/ it is the difference #384 was about.
  dir=$(cd "$(dirname "$file")" && pwd)

  # Only files rendered from a URL where a relative link cannot resolve may
  # not use them: README.md is packaged, and the pull-request template is
  # read in a pull request body (see the header).
  absolute_only=no
  case "${file##*/}" in
    README.md | PULL_REQUEST_TEMPLATE.md) absolute_only=yes ;;
  esac

  checked=0
  while IFS= read -r link; do
    [ -z "$link" ] && continue
    checked=$((checked + 1))
    case "$link" in
      '' | '#'* | mailto:*)
        ;;
      http://* | https://*)
        case "$link" in
          "$base"*)
            path=${link#"$base"}
            path=${path%%#*}
            if [ ! -e "$root/$path" ]; then
              echo "$file LINK: \"$path\" is linked but not in the repository" >&2
              status=1
            fi
            ;;
          */github.com/vyncint/*)
            rest=${link#*github.com/vyncint/}
            repo=${rest%%[/?#]*}
            known=no
            for candidate in $known_repos; do
              if [ "$repo" = "$candidate" ]; then
                known=yes
              fi
            done
            if [ "$known" = no ]; then
              echo "$file LINK: github.com/vyncint/$repo is not a repository this gate knows" >&2
              echo "  If the link is a typo, fix it. If the repository is real and newly" >&2
              echo "  linked, add it to known_repos in $(basename "$0")." >&2
              status=1
            fi
            ;;
        esac
        ;;
      *)
        if [ "$absolute_only" = yes ]; then
          case "${file##*/}" in
            PULL_REQUEST_TEMPLATE.md)
              echo "$file LINK: relative target \"$link\" — GitHub resolves it" >&2
              echo "  against the pull request's URL, where it does not exist. Use ${base}${link#../}" >&2 ;;
            *)
              echo "README LINK: relative target \"$link\" — crates.io rewrites it against" >&2
              echo "  crates/termlens/, where it does not exist. Use ${base}${link}" >&2 ;;
          esac
          status=1
        else
          path=${link%%#*}
          path=${path%%\?*}
          if [ ! -e "$dir/$path" ]; then
            echo "$file LINK: \"$link\" is linked but not in the repository" >&2
            echo "  From this file it resolves to \"$dir/$path\"." >&2
            status=1
          fi
        fi
        ;;
    esac
  done <<<"$(grep -oE '\]\([^)]+\)' "$file" | sed -E 's/^\]\(//; s/\)$//' | sort -u || true)"

  total=$((total + checked))
  echo "$file: $checked distinct link target(s)"
done

if [ "$status" -eq 0 ]; then
  echo "markdown links: $total target(s) across ${#files[@]} file(s), every in-repo target present"
fi
exit "$status"
