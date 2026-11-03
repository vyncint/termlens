#!/usr/bin/env bash
# check-skill-snippets.sh — compile every Rust block in skills/termlens/SKILL.md
# and README.md against the crate in this checkout.
#
# The skill is what a coding agent copies from, so a snippet that no longer
# compiles teaches the wrong API to everyone who installed it. Every fenced
# block tagged exactly ```rust must be a complete integration test that
# compiles in a consumer package owning a binary called `myapp` — the shape
# the recipes are written for. Blocks tagged ```rust,ignore are fragments and
# are skipped; ```toml, ```sh and ```text are never Rust.
#
# README.md is held to the same tags, with one difference: its blocks are
# mostly fragments of a test, so a block without `#[test]` is compiled as the
# body of a function that returns `termlens::Result<()>` and has a
# `t: Terminal` and a `screen: Screen` in scope — the names the prose uses.
# It compiles with every library feature on, because it shows `decode` and
# `regex`. README's decode example used `?` on a `DecodeError`, which does not
# convert into `termlens::Error`, and nothing compiled it (#660).
#
# Any other Rust tag fails rather than being skipped, and so does a fence the
# extraction cannot place: this matched only an unindented, bare ```rust, so a
# broken ```rust,no_run block or one indented under a list item passed (#657).
# A tag is skipped only by saying ,ignore.
#
# Compiles, does not run: the stub `myapp` here is `fn main() {}`, and the
# recipes' runtime claims are exercised against a real application when the
# skill is written, not on every push.
#
# `SNIPPETS_EXTRACT_ONLY=1` stops after the extraction and prints where the
# snippet files are: for tools/snippet-gate-selftest/ only, which checks the
# extraction without a toolchain. CI never sets it.
#
# Usage: check-skill-snippets.sh [SKILL.md [README.md]]
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
skill="${1:-$root/skills/termlens/SKILL.md}"
readme="${2:-$root/README.md}"
for f in "$skill" "$readme"; do
  [ -f "$f" ] || { echo "::error::$f does not exist"; exit 1; }
done

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# `consumer <dir> <termlens features> <extra dev-dependency lines>`
consumer() {
  mkdir -p "$1/src" "$1/tests"
  cat > "$1/Cargo.toml" <<EOF
[package]
name = "snippet-consumer"
version = "0.0.0"
edition = "2021"
publish = false

[[bin]]
name = "myapp"
path = "src/main.rs"

[dev-dependencies]
termlens = { path = "$root/crates/termlens", features = [$2] }
insta = "1"
$3

[workspace]
EOF
  echo 'fn main() {}' > "$1/src/main.rs"
}

# `extract <markdown> <tests dir>`: one file per compiled block, and an
# index line from file to the line in the markdown where the block opens, so
# a compile error points back at the prose. Prints the number of blocks
# written. Fails, naming the line, on a Rust tag that is neither `rust` nor
# `rust,ignore`, and on a fence left open at the end.
#
# A fence is any run of three or more backticks at any indentation, closed
# by a run at least as long with nothing after it, as CommonMark has it. The
# content of a non-Rust fence is passed over, so a ```rust line inside a
# ```text example opens nothing. A block indented under a list item has
# that indentation removed. No regex intervals: BSD awk lacks them.
extract() {
  awk -v dir="$2" -v short="${2#"$tmp"/}" -v index_file="$tmp/index" '
    function fence_len(s,   n) { n = 0; while (substr(s, n + 1, 1) == "`") n++; return n }
    {
      match($0, /^[ \t]*/)
      lead = RLENGTH
      rest = substr($0, lead + 1)
      ticks = fence_len(rest)
    }
    infence && ticks >= flen && rest ~ /^`+[ \t]*$/ {
      if (out != "") close(out)
      infence = 0; out = ""; next
    }
    infence {
      if (out == "") next
      # The fence'"'"'s own indentation comes off, and no more than that.
      body = $0
      for (i = 0; i < findent && substr(body, 1, 1) ~ /[ \t]/; i++) body = substr(body, 2)
      print body > out
      next
    }
    ticks >= 3 {
      info = substr(rest, ticks + 1)
      sub(/^[ \t]+/, "", info); sub(/[ \t]+$/, "", info)
      if (index(info, "`") > 0) next   # an inline code span, not a fence
      infence = 1; flen = ticks; findent = lead; out = ""; opened = NR
      lang = tolower(info); sub(/[ ,{].*$/, "", lang)
      if (lang != "rust" && lang != "rs") next
      tag = tolower(info); gsub(/[ \t]/, "", tag)
      if (tag == "rust,ignore") next
      if (tag != "rust") {
        printf "::error file=%s,line=%d::```%s is neither ```rust (compiled) nor ```rust,ignore (skipped)\n", FILENAME, NR, info > "/dev/stderr"
        bad = 1
        next
      }
      n++
      out = sprintf("%s/snippet_%02d.rs", dir, n)
      printf "%s/snippet_%02d.rs  %s:%d\n", short, n, FILENAME, NR + 1 >> index_file
      # An empty block still compiles as an empty file.
      printf "" > out
      next
    }
    END {
      if (infence) {
        printf "::error file=%s,line=%d::a fence opened here is never closed\n", FILENAME, opened > "/dev/stderr"
        bad = 1
      }
      if (bad) exit 1
      print n + 0
    }
  ' "$1"
}

# A fragment is wrapped once its block is whole: whether it is a complete
# test is known only from all of it.
wrap_fragments() {
  for f in "$1"/snippet_*.rs; do
    [ -e "$f" ] || continue
    grep -q '#\[test\]' "$f" && continue
    {
      echo '#![allow(unused)]'
      echo 'use termlens::{Screen, Terminal};'
      echo 'fn fragment(mut t: Terminal, screen: Screen) -> termlens::Result<()> {'
      cat "$f"
      echo '    Ok(())'
      echo '}'
    } > "$f.wrapped"
    mv "$f.wrapped" "$f"
  done
}

status=0
total=0
# `run <markdown> <consumer> <mode> <features> <extra deps>`
run() {
  consumer "$2" "$4" "$5"
  if ! count="$(extract "$1" "$2/tests")"; then
    status=1
    return
  fi
  if [ "$count" -eq 0 ]; then
    echo "::error::no \`\`\`rust blocks found in $1"
    status=1
    return
  fi
  [ "$3" = fragment ] && wrap_fragments "$2/tests"
  total=$((total + count))
  if [ -n "${SNIPPETS_EXTRACT_ONLY:-}" ]; then
    echo "extracted $count block(s) from $1 into $2/tests"
    return
  fi
  if ! (cd "$2" && CARGO_TARGET_DIR="$tmp/target" cargo check --quiet --tests); then
    echo "::error::a Rust block in $1 no longer compiles against crates/termlens"
    status=1
  fi
}

run "$skill" "$tmp/skill" test '' ''
run "$readme" "$tmp/readme" fragment '"decode", "regex"' 'regex = "1"'

if [ -n "${SNIPPETS_EXTRACT_ONLY:-}" ]; then
  # The selftest reads the files, so they must outlive this script.
  trap - EXIT
  echo "snippets under $tmp"
  exit "$status"
fi
if [ "$status" -ne 0 ]; then
  echo "snippet files map to the markdown as follows:"
  cat "$tmp/index" 2>/dev/null || true
  exit 1
fi
echo "check-skill-snippets: $total Rust blocks in ${skill#"$root"/} and ${readme#"$root"/} compile against crates/termlens"
