#!/usr/bin/env bash
# .github/scripts/check-package-contents.sh — neither published crate ships
# its integration tests (#385, #519), and both ship their licence texts (#863).
#
# A test in the tarball runs from the unpacked crate, where the workspace it
# was written against is gone: the library's suite builds fixture binaries
# from `fixtures/` (publish = false), and the CLI's corpus test reads
# `../termlens/tests/compat`, outside its package. Shipped, each fails for
# anyone who runs `cargo test` on the download. Both manifests say
# `exclude = ["tests/"]`; this is the check that keeps it that way.
#
# Both crates are `MIT OR Apache-2.0`, and both licences ask for their text to
# travel with every copy: MIT's notice "shall be included in all copies", and
# Apache-2.0 §4(a) gives recipients "a copy of this License". A crates.io
# tarball is such a copy, and the texts sat only at the repository root, which
# no package includes. Each crate directory links `LICENSE-MIT` and
# `LICENSE-APACHE` to the root's; `cargo package` stores the files themselves.
#
# `cargo package --list` reads the manifest and the tree only, so it works on
# a release commit whose library version is not on crates.io yet.
#
# Usage: .github/scripts/check-package-contents.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

fail=0
for crate in termlens termlens-cli; do
  listed="$(cargo package --list --allow-dirty -p "$crate")"
  shipped="$(printf '%s\n' "$listed" | grep '^tests/' || true)"
  if [ -n "$shipped" ]; then
    echo "::error::package contents: $crate ships tests/; add it to \`exclude\` in its Cargo.toml" >&2
    printf '%s\n' "$shipped" | sed 's/^/  /' >&2
    fail=1
  else
    count="$(printf '%s\n' "$listed" | wc -l | tr -d ' ')"
    echo "package contents: $crate ships no tests/ ($count files)"
  fi
  for licence in LICENSE-MIT LICENSE-APACHE; do
    if ! printf '%s\n' "$listed" | grep -Fxq "$licence"; then
      echo "::error::package contents: $crate does not ship $licence; link it in crates/$crate/ to the root's" >&2
      fail=1
    fi
  done
done
exit "$fail"
