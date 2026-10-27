# termlens report action

Puts the screens a failing [termlens](https://crates.io/crates/termlens)
suite left behind into the pull request: insta's `.snap.new` files (with the
cell diff against the `.snap` beside each) and every screen the library
wrote to `TERMLENS_ARTIFACT_DIR`, rendered as text in the step summary and
as SVG/HTML in an uploaded artifact.

```yaml
- run: cargo test
  env:
    TERMLENS_ARTIFACT_DIR: ${{ runner.temp }}/termlens
    INSTA_UPDATE: new
- uses: vyncint/termlens/.github/actions/report@v0.11.7
  if: failure()
```

`INSTA_UPDATE: new` is what makes insta write a `.snap.new` on CI. Its
default, `auto`, writes none when the `CI` variable is set, and GitHub sets
it on every runner, so without the line a failing snapshot leaves nothing
here to show. Inline snapshots (`assert_snapshot!(value, @"…")`) are not
rendered: insta keeps their new values in `.pending-snap` files, JSON
records for `cargo insta review` rather than a screen, and the action does
not collect them.

Nothing to install: the action `cargo install`s `termlens-cli` on the
runner. Inputs: `artifact-dir` (defaults to `TERMLENS_ARTIFACT_DIR`, else
`$RUNNER_TEMP/termlens`), `cli-version` (a `termlens-cli` version; empty
means latest), `name` (the artifact's, default `termlens-report`), and
`cli: workspace` for the termlens repository itself.
