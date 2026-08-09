# Security Policy

`termtest` is a development-time test harness. It spawns the programs *you*
tell it to spawn, in a PTY, with the environment you configure — it is not
intended to process untrusted input in production. That said, bugs that allow
a spawned program's output to corrupt the harness process (e.g. memory safety
issues in escape-sequence handling) are treated as security issues.

## Posture

What the project does continuously, enforced by required CI on every change:

- **No `unsafe` code** in the crate (`unsafe_code` lint, clippy
  `-D warnings`). Escape sequences from the child are parsed by pure-Rust
  code; nothing from the terminal stream is ever executed or evaluated.
- **Dependency policy** (`cargo-deny` job): RUSTSEC advisories, yanked
  crates, license allowlist, and crates.io-only sources — on every PR, with
  weekly grouped Dependabot updates and Dependabot alerts enabled.
- **Workflow security** (`zizmor` job at `--persona=pedantic`): every
  GitHub Action is pinned to a full commit SHA, checkouts don't persist
  credentials, the workflow token is read-only by default with write scopes
  granted per job, and inputs reach shell steps only via environment
  variables. Accepted findings are documented in `.github/zizmor.yml`.
- **Release integrity**: `v*` tags are ruleset-protected (admin-only),
  releases re-run the full CI gates, and publishing prefers crates.io
  Trusted Publishing (short-lived OIDC tokens) over stored secrets.
- **Provenance**: every commit requires a DCO sign-off from a human author
  of record; bot-authored commits are rejected by CI.

Resource-exhaustion notes for the paranoid: the emulator runs with zero
scrollback, the reader uses a fixed buffer, every wait is deadline-bounded,
and a hostile child can at worst waste its own test's time budget.

## Supported versions

| Version        | Supported          |
| -------------- | ------------------ |
| latest 0.x     | :white_check_mark: |
| older releases | :x:                |

Only the most recent release receives fixes. Pre-1.0, fixes ship as a new 0.x
release rather than backports.

## Reporting a vulnerability

**Please do not open a public issue for suspected vulnerabilities.**

- Preferred (once this repository is public): use GitHub
  [Private Vulnerability Reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
  — the "Report a vulnerability" button under the Security tab.
- Interim / fallback: email the maintainer at <vyncint@icloud.com> with the
  subject line `[SECURITY] termtest`.

You should receive an acknowledgement within 72 hours.

## Disclosure

We follow **90-day coordinated disclosure**: we ask that you give us up to 90
days from your report to publish a fix before any public disclosure. In
practice we aim to be much faster. We will credit reporters in the advisory
and CHANGELOG unless you prefer otherwise.

There is **no bug bounty** for this project.
