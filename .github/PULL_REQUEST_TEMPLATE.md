<!--
Thanks for opening a PR! Heads-up from the README: the maintainer is learning Rust,
so explain the idiomatic way rather than just dunking on the `.clone()`s. 🙏
Delete any section that doesn't apply.
-->

## What & why
<!-- What does this change, and what problem does it solve? Link any issue: "Closes #123". -->

## How to test
<!-- Steps to see it working. Which repo did you point Kyde at? What did you click? -->

## Speed & footprint
Kyde's whole reason to exist is being **genuinely fast** with a small footprint, so every
PR gets the same quick gut-check. Tick what applies:

- [ ] No new work added to a **hot path** (per-keystroke highlight, per-frame render,
      per-file-select diff) — or if there is, it's justified and has a `perf_*` guard test.
- [ ] No new **always-on dependency** that bloats the binary / resident RAM. If a heavy,
      optional thing was added, it's behind a **Cargo feature** (see README → Build) so
      builds can drop it.
- [ ] Startup still feels instant (the app should open before you finish letting go of the
      mouse).
- [ ] N/A — docs / chore / pure refactor with no runtime impact.

## Checklist
- [ ] `cargo build` is clean (no new warnings).
- [ ] `cargo test` is green (`cargo test perf` if you touched a hot path).
- [ ] No vendor code or assets copied from Zed or any GPL source (patterns only — see
      CLAUDE.md → Reference).
- [ ] Updated `CLAUDE.md` if architecture or conventions changed. The README lists headline
      features only: settings, shortcuts and per-platform details stay out of it.
