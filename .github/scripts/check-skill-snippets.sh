#!/usr/bin/env bash
# check-skill-snippets.sh — compile every Rust block in skills/termlens/SKILL.md
# against the crate in this checkout.
#
# The skill is what a coding agent copies from, so a snippet that no longer
# compiles teaches the wrong API to everyone who installed it. Every fenced
# block tagged exactly ```rust must be a complete integration test that
# compiles in a consumer package owning a binary called `myapp` — the shape
# the recipes are written for. Blocks tagged ```rust,ignore are fragments and
# are skipped; ```toml, ```sh and ```text are never Rust.
#
# Compiles, does not run: the stub `myapp` here is `fn main() {}`, and the
# recipes' runtime claims are exercised against a real application when the
# skill is written, not on every push.
#
# Usage: check-skill-snippets.sh [SKILL.md]
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
skill="${1:-$root/skills/termlens/SKILL.md}"
[ -f "$skill" ] || { echo "::error::$skill does not exist"; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
consumer="$tmp/consumer"
mkdir -p "$consumer/src" "$consumer/tests"

cat > "$consumer/Cargo.toml" <<EOF
[package]
name = "skill-consumer"
version = "0.0.0"
edition = "2021"
publish = false

[[bin]]
name = "myapp"
path = "src/main.rs"

[dev-dependencies]
termlens = { path = "$root/crates/termlens" }
insta = "1"

[workspace]
EOF
echo 'fn main() {}' > "$consumer/src/main.rs"

# One file per block, and an index from file to the line in SKILL.md where
# the block opens, so a compile error points back at the prose.
awk -v dir="$consumer/tests" -v index_file="$tmp/index" '
  /^```rust$/ { n++; f = sprintf("%s/snippet_%02d.rs", dir, n); printf "snippet_%02d.rs  %s:%d\n", n, FILENAME, NR + 1 >> index_file; inblock = 1; next }
  /^```/ && inblock { inblock = 0; close(f); next }
  inblock { print > f }
  END { print n + 0 }
' "$skill" > "$tmp/count"
count="$(cat "$tmp/count")"
if [ "$count" -eq 0 ]; then
  echo "::error::no \`\`\`rust blocks found in $skill"
  exit 1
fi

if ! (cd "$consumer" && cargo check --quiet --tests); then
  echo "::error::a Rust block in $skill no longer compiles against crates/termlens"
  echo "snippet files map to the skill as follows:"
  cat "$tmp/index"
  exit 1
fi
echo "check-skill-snippets: $count Rust blocks in ${skill#"$root"/} compile against crates/termlens"
