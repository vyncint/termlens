# Security Policy

`termtest` is a development-time test harness. It spawns the programs *you*
tell it to spawn, in a PTY, with the environment you configure — it is not
intended to process untrusted input in production. That said, bugs that allow
a spawned program's output to corrupt the harness process (e.g. memory safety
issues in escape-sequence handling) are treated as security issues.

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
