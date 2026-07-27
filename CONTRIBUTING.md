# Contributing to Kyde

Thanks for taking a look. Kyde is a small, opinionated tool — a fast native git
commit/diff view — and contributions are welcome. The author is upfront about not
being a Rust expert, so PRs that explain the idiomatic way are appreciated. Be kind
about the `.clone()`s.

## Ground rules

- **If a feature adds a lot of bloat, it should be a plugin**, not core. Language
  highlighting is the model: gated behind a Cargo feature, off by default at the
  margins, collapses gracefully when absent. See `Cargo.toml [features]` and
  `crates/kyde-config/src/plugins.rs`.
- **Speed is the whole point.** Anything on a per-keystroke, per-frame, or
  per-file-select hot path must stay fast. If you add such a path, add a `perf_*`
  guard test (see below).
- Keep the macOS-native, no-web, no-Electron, no-React constraint.

## Setup

Requires **Rust 1.96** (pinned in `rust-toolchain.toml`; `rustup` picks it up
automatically). On macOS, gpui compiles Metal shaders and needs Apple's Metal
Toolchain:

```sh
xcodebuild -downloadComponent MetalToolchain   # only if a build errors on it
```

On Linux you need gpui's backend dev libraries — see the `Install Linux deps` step
in `.github/workflows/build.yml` for the exact `apt` list.

```sh
cargo build                 # debug build
cargo run -- /path/to/repo  # run against any git repo (bare = Projects view)
cargo test --workspace      # logic + perf guard tests (see below — plain `cargo test`
                            # only covers the binary, not the crates/)
```

## Before you open a PR

CI runs these and will fail the PR otherwise, so run them locally first:

```sh
cargo fmt --all                         # then commit the result
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all
```

Match the clippy flags exactly — `--workspace` lints all eleven crates rather than just
the binary, and `--all-features` reaches the `cfg`-gated grammar / terminal /
remote-image code; a narrower local run passes while CI fails. CI runs these on both
`ubuntu-latest` and `macos-15` with `RUSTFLAGS: -D warnings`, so plain rustc warnings
fail too — including in the macOS-only `#[cfg(target_os = "macos")]` paths a Linux run
can't see.

A trim build is also checked in CI — if you touch the feature/grammar wiring,
verify it still compiles:

```sh
cargo build --no-default-features --features rust,json,toml
```

To run those checks automatically, enable the repo's git hooks once per clone:

```sh
git config core.hooksPath .githooks
```

`pre-commit` formats staged Rust files (so a commit can't carry unformatted code into
CI); `pre-push` runs fmt-check + clippy `-D warnings` + tests before anything leaves your
machine. Skip a hook with `--no-verify` if you must.

## Tests

- The pure-logic code lives in workspace crates under `crates/` (`kyde-git`,
  `kyde-diff`, `kyde-syntax`, `kyde-theme`, `kyde-config`, `kyde-tree`,
  `kyde-local-history`, …); each carries unit tests in its own `#[cfg(test)] mod tests`,
  plus doctests on the key pure entry points. Run one in isolation with
  `cargo test -p kyde-syntax`.
- The binary's own tests (including the headless-gpui smoke tests) live in-module too —
  it has no lib target, so `tests/` can't reach its `pub fn`s.
- Run the lot with `cargo test --workspace`. A bare `cargo test` builds only the root
  package (the binary), silently skipping every crate above. Note that under a slim
  feature set the `kyde-syntax` tests for grammars you didn't compile will fail — use
  `--workspace` or `-p kyde-syntax` (whose own default is all grammars) for a true green.
- **Perf guards** are named `perf_*` (run just them with `cargo test perf`). They
  push a representative-sized input through a hot path and assert it finishes under
  a deliberately loose budget — the goal is catching algorithmic blowups (accidental
  O(n²), per-keystroke reparse), not 2× CI jitter. Don't tighten them to "realistic"
  numbers; that only makes them flaky.

## Commits & PRs

- Small, focused commits with a clear message. Reference an issue if there is one.
- The PR template will ask what changed and how you tested it — fill it in.
- New user-facing behavior should get a line in `CHANGELOG.md` under `Unreleased`.

## Code layout

`CLAUDE.md` is the deep map of the codebase — every module, the render flow, the
theme system, the plugin gates. Read it before a non-trivial change; it's the
fastest way to understand how a piece fits.

## License

By contributing you agree your work is licensed under the project's
[Apache-2.0](LICENSE) license.
