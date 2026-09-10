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
#     default and explicit-only views run as well.
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
# Usage: check-semver.sh <baseline-version>
#   env BREAKING_LABEL=true   the PR carries the `breaking` label
set -euo pipefail

baseline=${1:?usage: check-semver.sh <baseline-version>}
package=termlens
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

version="$(cargo metadata --no-deps --format-version 1 \
  | jq -r --arg p "$package" '.packages[] | select(.name == $p) | .version')"

# A `- **Breaking` bullet inside one `## [section]` of the CHANGELOG.
declares_break() {
  awk -v want="$1" '
    /^## \[/ { in_section = (index($0, "## [" want "]") == 1) }
    in_section && /^- \*\*Breaking/ { found = 1 }
    END { exit !found }
  ' CHANGELOG.md
}

declared=""
if [ "${BREAKING_LABEL:-false}" = "true" ]; then
  declared="the pull request carries the \`breaking\` label"
elif declares_break Unreleased; then
  declared="CHANGELOG.md declares a break under [Unreleased]"
elif [ "$version" != "$baseline" ] && declares_break "$version"; then
  declared="CHANGELOG.md declares a break under [$version], which is ahead of the $baseline baseline"
fi

if [ -n "$declared" ]; then
  release_type=major
else
  release_type=patch
fi

checker="$(cargo semver-checks --version)"
echo "semver gate: $checker"
echo "  package $package $version against published $baseline; release type $release_type${declared:+ ($declared)}"

status=0
for view in default-features only-explicit-features all-features; do
  echo "::group::$view"
  if ! cargo semver-checks --package "$package" \
        --baseline-version "$baseline" \
        --release-type "$release_type" \
        "--$view"; then
    echo "::error::the $view view of $package $version is not $release_type-compatible with $baseline (release type $release_type)"
    status=1
  fi
  echo "::endgroup::"
done

# The job summary records what checked what, so a green run is legible later.
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### semver gate"
    echo
    echo "- checker: \`$checker\`"
    echo "- baseline: \`$package $baseline\` (published); tree: \`$version\`"
    echo "- release type: \`$release_type\`${declared:+ — $declared}"
    echo "- views: default-features, only-explicit-features, all-features"
    echo "- result: $([ "$status" -eq 0 ] && echo pass || echo FAIL)"
  } >> "$GITHUB_STEP_SUMMARY"
fi

if [ "$status" -eq 0 ]; then
  echo "semver gate: PASS — $package $version is $release_type-compatible with $baseline in all three views"
fi
exit "$status"
