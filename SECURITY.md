# Security Policy

`termlens` is a development-time test harness. It spawns the programs *you*
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

Resource-exhaustion notes for the paranoid: every buffer a child's output
can reach is bounded, and a hostile child can at worst waste its own test's
time budget. The reader uses a fixed 8 KiB buffer; every wait is
deadline-bounded; retained scrollback is capped at the configured length
(1000 rows by default, `scrollback(0)` to disable) and held as text rather
than cells; at most 8 completed frames and 512 frame timings are retained; a
captured `OSC 52` payload is dropped past 64 KiB; **undelivered query replies
are capped at 1 MiB** and discarded past it, counted and named in the next
wait's error; the capture of a DCS-class header is fixed at 128 bytes; and the
set of distinct unanswered queries kept for diagnostics is capped, with the
remainder counted.

The reply cap is a byte bound rather than a queue depth on purpose: a depth
bounds the wrong thing. Two earlier versions counted queue slots and both
shorted a well-behaved application asking a legitimate batch of startup
probes, while a byte bound leaves the queue itself unbounded — so the drain
can never block on it, which is what keeps a hostile child from deadlocking
the harness rather than merely slowing its own test.

## Supported versions

| Version        | Supported          |
| -------------- | ------------------ |
| latest 0.x     | :white_check_mark: |
| older releases | :x:                |

Only the most recent release receives fixes. Pre-1.0, fixes ship as a new 0.x
release rather than backports.

## Reporting a vulnerability

**Please do not open a public issue for suspected vulnerabilities.**

- Preferred: use GitHub
  [Private Vulnerability Reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
  — the "Report a vulnerability" button under the Security tab.
- Interim / fallback: email the maintainer at <vyncint@icloud.com> with the
  subject line `[SECURITY] termlens`.

You should receive an acknowledgement within 72 hours.

## Disclosure

We follow **90-day coordinated disclosure**: we ask that you give us up to 90
days from your report to publish a fix before any public disclosure. In
practice we aim to be much faster. We will credit reporters in the advisory
and CHANGELOG unless you prefer otherwise.

There is **no bug bounty** for this project.
