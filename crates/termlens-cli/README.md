# termlens-cli

The `termlens` command: the [termlens](https://crates.io/crates/termlens)
PTY harness at a shell prompt.

```sh
cargo install termlens-cli --locked

termlens inspect --size 120x40 htop          # run a program, print its screen
termlens inspect --ansi ./target/debug/myapp # …in colour
termlens inspect --cwd ./examples ./myapp    # …somewhere else
termlens diff old.snap new.snap.new          # what changed, cell by cell; exit 1 if anything
termlens render --svg --out failing.svg failing.snap.new
```

A saved screen is the snapshot text format termlens prints (an insta
`.snap`, with or without its header; the block a wait error prints; a
`TERMLENS_ARTIFACT_DIR` file), or the JSON the crate's `serde` feature
writes. `diff` colours the changed cells on a terminal and stays plain in a
pipe; `--color always|never|auto`, and `NO_COLOR` is honoured.

`diff` and `render` read `-` as standard input, for a screen that arrives on
a pipe — out of a CI log, or from the tool that made it a moment earlier.
Only one of `diff`'s two operands can be `-`, since stdin is read once:

```sh
termlens inspect ./myapp | termlens render --svg --out shot.svg -
termlens diff expected.snap -   < actual.snap
```

`render --out PATH` writes there instead of to stdout, and creates nothing
when the render fails. A shell redirect cannot promise that — `> file.svg`
truncates the file before termlens runs, so a failed render leaves an empty
image attached to the bug report.

`inspect --cwd PATH` runs the program in `PATH`, which must already exist:
the working directory is part of how a program is normally run, and
`TerminalBuilder::current_dir` had no way through to the command line.

Exit codes: `0` ran; `diff` exits `1` when the screens differ; `2` means the
command itself could not run — bad arguments, an unreadable file, a program
that could not be spawned.

The `.github/actions/report` action in the termlens repository uses this
binary to put failing screens into a pull request's step summary.
