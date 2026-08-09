# Maintainers

## Current maintainers

| Name       | GitHub                                     | Role                |
| ---------- | ------------------------------------------ | ------------------- |
| Vyncint Ng | [@vyncint](https://github.com/vyncint)     | Lead maintainer     |

## Governance model

`termlens` currently uses a **single-maintainer (BDFL) model**: the lead
maintainer has final say on design, scope, and releases. This is documented so
expectations are clear, not because it is a goal — the intent is to grow a
small maintainer team as the project attracts sustained contributors.

What maintainers do:

- review and merge PRs (all merges are squash merges through required CI),
- triage issues and label good first issues,
- cut releases per [docs/RELEASING.md](docs/RELEASING.md),
- handle security reports per [SECURITY.md](SECURITY.md),
- enforce the [Code of Conduct](CODE_OF_CONDUCT.md).

## Becoming a maintainer

Sustained, high-quality contributions (code, review, triage) over a few months
are the path. Maintainers are added by consensus of the existing maintainer(s)
and get an entry in this file plus `.github/CODEOWNERS`.

## Decision making

Day-to-day decisions happen in issues and PRs. Anything that changes the
public API, the snapshot format, or the wait semantics contract in
[docs/DESIGN.md](docs/DESIGN.md) needs a maintainer's explicit approval.
