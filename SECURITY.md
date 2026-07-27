# Security Policy

## Supported versions

Kyde ships from `main`, and only the latest release is supported. Security fixes land
on `main` and in the next tagged release; older tags are not separately patched.

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Report privately via GitHub's [private vulnerability reporting][gh] (Security →
Report a vulnerability) on this repository, or email **kyle.johnson@flagsmith.com**
with the details and, if possible, a minimal reproduction.

[gh]: https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability

You'll get an acknowledgement within a few days. Once fixed, the advisory and a
credit (if you'd like one) are published with the release.

## Scope / threat model

Kyde is a local desktop app. Things worth a report:

- **Shell-out injection.** Kyde drives git by shelling out to the `git` binary
  (`crates/kyde-git/src/lib.rs`). A repo path, branch name, file name, or commit
  message that can break argument boundaries and run an unintended command is in
  scope.
- **Path traversal / writes outside the open project** — file editing, save, and
  rollback (which deletes files) should never touch paths outside the selected
  repo.
- **Config parsing** — the JSON config files under `~/.config/kyde/` (theme,
  keymap, plugins, projects, history) are user-editable, as is the local-history
  store under `~/.local/share/kyde/`; a crafted file causing memory unsafety is in
  scope (panics that are merely a crash are lower priority).
- **The in-app updater** — it downloads a release archive and swaps the running
  `.app` in place (`crates/kyde-update`). Anything letting an attacker substitute
  that payload is in scope.
- **The single-instance socket** — a later launch hands a project path to the
  running instance over a unix socket in the temp dir (`src/platform/instance.rs`).
  A local process able to make Kyde open or act on a path it shouldn't is in scope.
- **Dependency advisories** — CI runs `cargo-deny` against RustSec, but a flagged
  advisory we've missed is welcome.

Out of scope: issues requiring an already-compromised local account, and the
inherent trust in opening a repository you don't control (Kyde runs `git` in it,
same as any git GUI).
