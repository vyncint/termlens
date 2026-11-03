#!/usr/bin/env bash
# Every gate CI runs must appear in CONTRIBUTING.md §1's "run these before
# pushing" block. The paragraph under that block promises the workflow is
# the source of truth when the two disagree; this is what makes the promise
# hold. Usage: check-ci-gates-listed.sh [ci.yml] [CONTRIBUTING.md]
set -euo pipefail

ci="${1:-.github/workflows/ci.yml}"
doc="${2:-CONTRIBUTING.md}"

# A gate is a single-line command starting with `cargo`, `.github/scripts/`
# or `tools/`: the shape a contributor can type, and the one §1 lists. Every
# other `run:` is job plumbing (the msrv assertion, the zizmor retry,
# required-green's jq), and plumbing is named below, by step name, rather
# than recognised by shape. This used to read only lines spelled
# `- run: cargo …`, so a gate under a `- name:`, in quotes or in a folded
# `run: >-` block was invisible to it and passed unlisted (#657). Now every
# `run:` is read, and one that is neither a gate nor on this list fails.
plumbing=(
  "Read MSRV from Cargo.toml"
  "Assert the MSRV toolchain is the one in use"
  "Read the breaking label as it is now"
  "Audit .github at pedantic level"
  "Verify every needed job succeeded"
)

# One record per `run:` key, separated by \037 (a tab is IFS whitespace, so
# an empty name between two would vanish): kind (line or block), the step's
# name (empty when it has none), the line number, and for a single-line
# value the command with YAML's quoting and folding undone. A step's keys
# can come in any order, so a step is printed once it ends, when its name is
# known. This is a reader for the YAML this repository writes, not a YAML
# parser: flow mappings and anchors are not steps here.
records="$(awk '
  function indent(s) { match(s, /^ */); return RLENGTH }
  function flush(k) {
    if (has_run[k]) printf "%s\037%s\037%d\037%s\n", kind[k], name[k], line[k], value[k]
    has_run[k] = 0; name[k] = ""
  }
  function unquote(v,   q) {
    q = substr(v, 1, 1)
    if ((q == "\"" || q == "\047") && substr(v, length(v), 1) == q) {
      v = substr(v, 2, length(v) - 2)
      if (q == "\"") gsub(/\\"/, "\"", v); else gsub(/\047\047/, "\047", v)
    }
    return v
  }
  # A plain scalar ends at " #"; a quoted one does not.
  function decomment(v) {
    if (v !~ /^["\047]/) sub(/[ \t]+#.*$/, "", v)
    return v
  }
  {
    raw = $0
    ind = indent(raw)
    blank = (raw ~ /^[ \t]*$/ || raw ~ /^[ \t]*#/)
  }
  # The continuation of a block scalar, or of a plain or quoted scalar that
  # spans lines: anything indented past the key it belongs to.
  cont_k >= 0 && (blank || ind > cont_k) {
    if (!blank && kind[cont_k] == "line") {
      v = raw; sub(/^ +/, "", v)
      value[cont_k] = value[cont_k] (value[cont_k] == "" ? "" : " ") v
    }
    next
  }
  cont_k >= 0 {
    if (kind[cont_k] == "line") value[cont_k] = unquote(decomment(value[cont_k]))
    cont_k = -1
  }
  blank { next }
  {
    # A line less indented than a pending step ends it.
    for (j in has_run) if (has_run[j] && ind < j + 0) flush(j)
  }
  match(raw, /^ *- /) {
    k = RLENGTH
    flush(k)
    rest = substr(raw, k + 1)
  }
  !match(raw, /^ *- /) { k = ind; rest = substr(raw, ind + 1) }
  rest ~ /^name:/ {
    v = rest; sub(/^name:[ \t]*/, "", v)
    name[k] = unquote(decomment(v))
    next
  }
  rest ~ /^run:([ \t]|$)/ {
    v = rest; sub(/^run:[ \t]*/, "", v)
    has_run[k] = 1; line[k] = NR
    if (v ~ /^[|>][-+0-9]*[ \t]*(#.*)?$/) {
      kind[k] = "block"; value[k] = ""
    } else {
      kind[k] = "line"; value[k] = v
    }
    cont_k = k
    next
  }
  END {
    if (cont_k >= 0 && kind[cont_k] == "line") value[cont_k] = unquote(decomment(value[cont_k]))
    for (j in has_run) flush(j)
  }
' cont_k=-1 "$ci")"

is_plumbing() {
  local p
  for p in "${plumbing[@]}"; do
    [ "$1" = "$p" ] && return 0
  done
  return 1
}

status=0
ci_cmds=""
while IFS=$'\037' read -r kind name lineno cmd; do
  [ -z "$kind" ] && continue
  if [ -n "$name" ] && is_plumbing "$name"; then
    continue
  fi
  where="$ci:$lineno${name:+ (step \"$name\")}"
  if [ "$kind" = block ]; then
    echo "::error::$where runs a multi-line block. A gate is one line that CONTRIBUTING.md §1 can list; plumbing is named in the plumbing list in $(basename "$0")"
    status=1
    continue
  fi
  cmd="$(sed -E 's/[[:space:]]+/ /g; s/^ //; s/ $//' <<<"$cmd")"
  case "$cmd" in
    "cargo "* | .github/scripts/* | tools/*)
      ci_cmds="$ci_cmds$cmd"$'\n' ;;
    *)
      echo "::error::$where runs \`$cmd\`, which is neither a gate (cargo …, .github/scripts/…, tools/…) nor a step in the plumbing list in $(basename "$0")"
      status=1 ;;
  esac
done <<<"$records"

cargo_count="$(grep -c '^cargo ' <<<"$ci_cmds" || true)"
script_count="$(grep -Ec '^(\.github/scripts/|tools/)' <<<"$ci_cmds" || true)"

# CONTRIBUTING §1: the shared extraction in extract-gates.sh (#368), plus
# the normalisations this comparison alone needs — the RUSTDOCFLAGS prefix
# and a `+toolchain` selector removed, because `cargo +1.85 check` in the
# doc is `cargo check` under RUSTUP_TOOLCHAIN in the workflow.
doc_cmds="$("$(dirname "${BASH_SOURCE[0]}")/extract-gates.sh" "$doc" \
  | sed -E -e "s/^RUSTDOCFLAGS='-D warnings' //" \
           -e 's/^cargo \+[^ ]+ /cargo /')"

while IFS= read -r cmd; do
  [ -z "$cmd" ] && continue
  if ! grep -Fxq -- "$cmd" <<<"$doc_cmds"; then
    echo "::error::CI runs \`$cmd\` but CONTRIBUTING.md §1 does not list it"
    status=1
  fi
done <<<"$ci_cmds"

# zizmor is pinned in the workflow; the doc must name the same pin.
pin="$(grep -o 'zizmor==[0-9.]*' "$ci" | head -1 || true)"
if [ -n "$pin" ] && ! grep -Fq -- "$pin" "$doc"; then
  echo "::error::CI pins \`$pin\` but CONTRIBUTING.md names a different zizmor, or none"
  status=1
fi

if [ "$status" -eq 0 ]; then
  echo "every CI gate is listed in CONTRIBUTING.md §1 ($cargo_count cargo commands, $script_count script gates, $pin)"
fi
exit "$status"
