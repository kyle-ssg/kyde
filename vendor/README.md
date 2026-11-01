# vendor/

Patched copies of crates.io crates. The root `Cargo.toml` puts each in place of the crates.io
release through `[patch.crates-io]`, and keeps this folder out of the workspace, so these build
as dependencies: Kyde's lints, `cargo fmt` and tests don't run on them.

## cosmic-text 0.14.2

The crates.io package of `cosmic-text` 0.14.2 (upstream commit in `.cargo_vcs_info.json`),
minus what building it as a dependency doesn't need: `tests/`, `benches/`, `fonts/`,
`sample/`, `screenshots/`, the shell scripts, CI files and its `Cargo.lock`.

**Why it's here.** gpui 0.2.2 draws text on Linux through cosmic-text: its Linux text system
rasterises every glyph with cosmic-text's `SwashCache`, always greyscale-antialiased and with
swash's own hinting. That's the only rendering it can do, whatever the desktop is set to.

**Changed from the crates.io package:**
- `Cargo.toml`: `[lints.rust] mismatched_lifetime_syntaxes = "allow"`. From crates.io a
  dependency's warnings are capped; built from this path, 0.14.2's elided lifetimes would fail
  Kyde's `-D warnings` on rustc 1.89+.

Like Kyde's own crates it has no `source` in `Cargo.lock`, so release-please's lockfile entry in
`release-please-config.json` leaves it out by name: it keeps version 0.14.2 when Kyde's version
is bumped. A crate added here needs the same.

To see every change: `diff -r <cargo registry>/cosmic-text-0.14.2 vendor/cosmic-text`.

**To drop it:** delete the `cosmic-text` line under `[patch.crates-io]` in the root
`Cargo.toml` and this folder; the next build puts the crates.io entry back in `Cargo.lock`.
