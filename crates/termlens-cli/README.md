# termlens-cli

The `termlens` command: the [termlens](https://crates.io/crates/termlens)
PTY harness at a shell prompt.

```sh
cargo install termlens-cli --locked

termlens inspect --size 120x40 htop          # run a program, print its screen
termlens inspect --ansi ./target/debug/myapp # …in colour
termlens diff old.snap new.snap.new          # what changed, cell by cell; exit 1 if anything
termlens render --svg failing.snap.new > failing.svg
```

A saved screen is the snapshot text format termlens prints (an insta
`.snap`, with or without its header; the block a wait error prints; a
`TERMLENS_ARTIFACT_DIR` file), or the JSON the crate's `serde` feature
writes. `diff` colours the changed cells on a terminal and stays plain in a
pipe; `--color always|never|auto`, and `NO_COLOR` is honoured.

Exit codes: `0` ran; `diff` exits `1` when the screens differ; `2` means the
command itself could not run — bad arguments, an unreadable file, a program
that could not be spawned.

The `.github/actions/report` action in the termlens repository uses this
binary to put failing screens into a pull request's step summary.
