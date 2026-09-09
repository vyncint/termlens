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
- uses: vyncint/termlens/.github/actions/report@v0.10.1
  if: failure()
```

Nothing to install: the action `cargo install`s `termlens-cli` on the
runner. Inputs: `artifact-dir` (defaults to `TERMLENS_ARTIFACT_DIR`, else
`$RUNNER_TEMP/termlens`), `cli-version` (a `termlens-cli` version; empty
means latest), `name` (the artifact's, default `termlens-report`), and
`cli: workspace` for the termlens repository itself.
