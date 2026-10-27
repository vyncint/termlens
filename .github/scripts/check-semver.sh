#!/usr/bin/env bash
# The semver gate (#323): termlens's public API against the last published
# release, with the release type forced and every feature view checked.
#
# Two holes in a naive `cargo semver-checks` run, both measured and both
# asserted by tools/semver-gate-selftest/run.sh:
#
#   * The version number decides how much is checked. A 0.10 -> 0.11 bump is
#     inferred as a major release and every check is skipped, so any
#     contributor who bumps the version in the same PR as a break gets a
#     green gate. The type is therefore forced here and never inferred.
#   * `--all-features` cannot see an item moved behind a feature: that view
#     never loses the item. Cargo calls that removal a major change, so the
#     default and explicit-only views run as well, and so does each feature
#     on its own: an item moved from behind `regex` to behind `regex` and
#     `serde` is lost to a `--features regex` build and to none of the other
#     three views (#598). The features are read from the manifest, so one
#     added later is checked without an edit here.
#
# The baseline is a literal: the last *published* version, which is the one
# a consumer can install. It moves in a follow-up PR after each release, not
# in the release PR — a version that is not on crates.io yet cannot be a
# baseline (docs/RELEASING.md). Never a moving "latest".
#
# The release type is `patch` unless a break is *declared*, in one of two
# places, because the pull-request label alone is invisible on a push to
# main and turned main red after the first labelled break merged elsewhere:
#
#   1. the pull request carries the `breaking` label (BREAKING_LABEL=true,
#      set by the workflow from the event payload);
#   2. CHANGELOG.md records the break: a bullet beginning `- **Breaking`
#      under `## [Unreleased]`, or under the section for the tree's own
#      version when that version is ahead of the baseline — the state main is
#      in between a release merge and the baseline bump that follows publish.
#
# A declared break runs with `--release-type major`. Not `minor`: on a 0.x
# crate cargo-semver-checks still calls a removed item a major change under
# `minor`, so the label would not have let the one intended break through
# (the self-test pins this too).
#
# `major` accepts any break, and a bullet stays in CHANGELOG.md until the
# release, so one declared break used to let every later pull request break
# the API unseen (#598). When the break is declared by bullets the base
# revision already had, so that this change adds none and carries no label,
# the tree is checked a second time, against that revision with `patch`:
# the declared break is part of that baseline, and only a new one fails. A
# change that rewrites a bullet's first line adds a bullet. The workflow
# passes the revision as BASE_REV: a pull request's base, or the commit a
# push moved main from. A tag push, which release.yml runs this for, has
# none, and neither has a local run without it: the second check is then
# skipped, and the log and the summary say so.
#
# Usage: check-semver.sh <baseline-version>
#   env BREAKING_LABEL=true   the PR carries the `breaking` label
#   env BASE_REV=<commit>     the commit this change lands on; empty or all
#                             zeros (a push that created its ref) means none
#   env SEMVER_GATE_BASELINE_ROOT=<dir>
#                             the baseline's source instead of the crates.io
#                             release, for tools/semver-gate-selftest/
#                             only; CI never sets it
#   env SEMVER_GATE_PACKAGE=<name>
#                             the package checked instead of termlens, for
#                             the same selftest: its models must not be named
#                             termlens, or cargo-semver-checks, which reads
#                             every manifest under the repository, finds the
#                             real crate's name defined five times
set -euo pipefail

baseline=${1:?usage: check-semver.sh <baseline-version>}
package="${SEMVER_GATE_PACKAGE:-termlens}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

metadata="$(cargo metadata --no-deps --format-version 1)"
version="$(jq -r --arg p "$package" \
  '.packages[] | select(.name == $p) | .version' <<<"$metadata")"
# Every feature a consumer can ask for alone. `default` is the default view
# already, and so is a feature that is the whole default set (`insta`).
features="$(jq -r --arg p "$package" '
  .packages[] | select(.name == $p) | .features as $f
  | $f | keys[] | select(. != "default" and [.] != ($f.default // []))
' <<<"$metadata")"
views="default-features only-explicit-features all-features $(echo $features)"

# The `- **Breaking` bullets inside one `## [section]` of the CHANGELOG on
# standard input.
breaking_bullets() {
  awk -v want="$1" '
    /^## \[/ { in_section = (index($0, "## [" want "]") == 1) }
    in_section && /^- \*\*Breaking/ { print }
  '
}

declared=""
bullets=""
if [ "${BREAKING_LABEL:-false}" = "true" ]; then
  declared="the pull request carries the \`breaking\` label"
else
  bullets="$(breaking_bullets Unreleased < CHANGELOG.md)"
  if [ -n "$bullets" ]; then
    declared="CHANGELOG.md declares a break under [Unreleased]"
  elif [ "$version" != "$baseline" ]; then
    bullets="$(breaking_bullets "$version" < CHANGELOG.md)"
    if [ -n "$bullets" ]; then
      declared="CHANGELOG.md declares a break under [$version], which is ahead of the $baseline baseline"
    fi
  fi
fi

if [ -n "$declared" ]; then
  release_type=major
else
  release_type=patch
fi

base_rev="${BASE_REV:-}"
case "$base_rev" in
  *[!0]*) ;;
  *) base_rev="" ;;
esac

# Whether the tree is checked again against the base revision, and why.
recheck=""
recheck_reason=""
if [ "${BREAKING_LABEL:-false}" = "true" ]; then
  recheck_reason="not checked against a base revision: the label declares this pull request's own break"
elif [ -n "$bullets" ]; then
  if [ -z "$base_rev" ]; then
    recheck_reason="not checked against a base revision, since BASE_REV names none: a further break would pass under \`major\`"
  elif ! git rev-parse -q --verify "$base_rev^{commit}" > /dev/null; then
    echo "::error::BASE_REV $base_rev is not a commit in this clone; the checkout needs the history (fetch-depth: 0)"
    exit 1
  else
    base_changelog="$(git show "$base_rev:CHANGELOG.md" 2> /dev/null || true)"
    added=""
    while IFS= read -r bullet; do
      grep -Fxq -- "$bullet" <<<"$base_changelog" || added="$added$bullet"$'\n'
    done <<<"$bullets"
    if [ -n "$added" ]; then
      recheck_reason="not checked against $base_rev: this change adds the declaring bullet, so its break is the one declared"
    else
      recheck="$base_rev"
      recheck_reason="checked against $base_rev with \`patch\` as well: the declaring bullet was there already, so this change may add no break of its own"
    fi
  fi
fi

if [ -n "${SEMVER_GATE_BASELINE_ROOT:-}" ]; then
  published="$baseline at $SEMVER_GATE_BASELINE_ROOT"
  set -- --baseline-root "$SEMVER_GATE_BASELINE_ROOT"
else
  published="published $baseline"
  set -- --baseline-version "$baseline"
fi

checker="$(cargo semver-checks --version)"
echo "semver gate: $checker"
echo "  package $package $version against $published; release type $release_type${declared:+ ($declared)}"
echo "  views: $views"
if [ -n "$recheck_reason" ]; then
  echo "  $recheck_reason"
fi

status=0
checks=0
# `check <view> <release type> <against> <baseline args…>`
check() {
  view=$1
  release=$2
  against=$3
  shift 3
  case "$view" in
    default-features | only-explicit-features | all-features) set -- "$@" "--$view" ;;
    *) set -- "$@" --only-explicit-features --features "$view" ;;
  esac
  checks=$((checks + 1))
  echo "::group::$view, against $against, release type $release"
  if ! cargo semver-checks --package "$package" --release-type "$release" "$@"; then
    echo "::error::the $view view of $package $version is not $release-compatible with $against"
    status=1
  fi
  echo "::endgroup::"
}

for view in $views; do
  check "$view" "$release_type" "$published" "$@"
done
if [ -n "$recheck" ]; then
  for view in $views; do
    check "$view" patch "$recheck" --baseline-rev "$recheck"
  done
fi

# The job summary records what checked what, so a green run is legible later.
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### semver gate"
    echo
    echo "- checker: \`$checker\`"
    echo "- baseline: \`$package $baseline\` (published); tree: \`$version\`"
    echo "- release type: \`$release_type\`${declared:+ — $declared}"
    echo "- views: $views"
    if [ -n "$recheck_reason" ]; then
      echo "- $recheck_reason"
    fi
    echo "- result: $([ "$status" -eq 0 ] && echo pass || echo FAIL) ($checks checks)"
  } >> "$GITHUB_STEP_SUMMARY"
fi

if [ "$status" -eq 0 ]; then
  echo "semver gate: PASS — $package $version is $release_type-compatible with $published in every view${recheck:+, and patch-compatible with $recheck}"
fi
exit "$status"
