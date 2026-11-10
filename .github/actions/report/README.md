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
- uses: vyncint/termlens/.github/actions/report@v0.11.8
  if: failure()
  with:
    artifact-dir: ${{ runner.temp }}/termlens   # a step's env: reaches that step only
    name: termlens-report-${{ matrix.os }}      # one artifact per matrix leg
```

`INSTA_UPDATE: new` is what makes insta write a `.snap.new` on CI. Its
default, `auto`, writes none when the `CI` variable is set, and GitHub sets
it on every runner, so without the line a failing snapshot leaves nothing
here to show. Inline snapshots (`assert_snapshot!(value, @"…")`) are not
rendered: insta keeps their new values in `.pending-snap` files, JSON
records for `cargo insta review` rather than a screen, and the action does
not collect them.

Nothing to install: the action `cargo install`s `termlens-cli` on the
runner. Inputs:

- `artifact-dir`: where the tests wrote their screens. It defaults to
  `TERMLENS_ARTIFACT_DIR` when that is set for the job or the workflow,
  else `$RUNNER_TEMP/termlens`. A variable in the test step's own `env:`
  reaches that step only, so the action never sees it: pass the same value
  here, as above. Without it the action finds nothing, unless the value
  happens to be `$RUNNER_TEMP/termlens`. It must be an absolute path, in
  both places: `cargo test` runs each test binary in its own package's
  directory, so a relative `TERMLENS_ARTIFACT_DIR` puts each crate's
  screens in that crate's directory, where the action does not look. Given
  a relative one, the action warns and says so in the summary.
- `name`: the artifact's, default `termlens-report`. Give each matrix leg
  its own, as above. upload-artifact refuses a second artifact of one name
  in a run, so with the default only one failing leg's screens are kept.
- `cli-version`: a `termlens-cli` version; empty means the latest.
- `cli: workspace`, for the termlens repository itself.
