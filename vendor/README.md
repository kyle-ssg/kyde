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
swash's own hinting. That's the only rendering it can do, whatever the desktop is set to. The
patch lets Kyde's Linux setting Settings → Appearance → Text → Your desktop's draw glyphs as the
desktop does: antialiasing off (a mask of only fully on and off pixels) and the hinting strength
the desktop names (none, slight, medium, full).

**Why cosmic-text and not gpui.** The change is the same either way: which pixels a glyph
outline becomes. gpui compiles cosmic-text on Linux only (macOS draws text with Core Text,
Windows with DirectWrite), so macOS and Windows build exactly what crates.io ships, and the
patch is one file of a small crate instead of a copy of gpui. A gpui patch confined to its Linux
text system couldn't switch renderings live either: gpui keeps every glyph it draws in its own
atlas, keyed without a rendering, and has no way to drop them. So the rendering is chosen once
per run, before the first glyph.

**Changed from the crates.io package** (each spot is marked `Kyde patch`):
- `src/swash.rs`: `GlyphRendering` (antialias on/off + `GlyphHinting`) maps to new
  `CacheKeyFlags` bits, so two renderings never share a `SwashCache` entry. A key without those
  bits (gpui passes `CacheKeyFlags::empty()`) takes the rendering `set_glyph_rendering` chose,
  once, before the first glyph; with none chosen it's the default, which draws through the
  unchanged `swash_image`, byte for byte as before. Other renderings draw colour glyphs as before
  and outlines through skrifa (swash's own outline library) with FreeType's targets, as cairo
  picks them: light for slight, normal for medium and full, and mono for any hinting without
  antialiasing; then the coverage is thresholded at half when antialiasing is off. Text fitted to the pixel grid (medium,
  full, or antialiasing off) is drawn at whole pixels rather than gpui's quarter-pixel offsets.
- `src/glyph_cache.rs`: the five new `CacheKeyFlags` bits.
- `Cargo.toml`: skrifa as a dependency of the `swash` feature (already in the tree through swash,
  same version); `[lints.rust] mismatched_lifetime_syntaxes = "allow"`, because from crates.io a
  dependency's warnings are capped but built from this path 0.14.2's elided lifetimes would
  fail Kyde's `-D warnings` on rustc 1.89+.

Upstream, gpui's Linux text system would pass the flags with each glyph instead of the process
choosing once; the flags and `rendered_image` could go to cosmic-text as they are.

Like Kyde's own crates it has no `source` in `Cargo.lock`, so release-please's lockfile entry in
`release-please-config.json` leaves it out by name: it keeps version 0.14.2 when Kyde's version
is bumped. A crate added here needs the same. Match on `@.name.value`, not `@.name`:
release-please's TOML parser wraps every value in an object, so `@.name != '…'` is always true.

To see every change: `diff -r <cargo registry>/cosmic-text-0.14.2 vendor/cosmic-text`.

**To drop it:** delete the `cosmic-text` line under `[patch.crates-io]` in the root
`Cargo.toml` and this folder; the next build puts the crates.io entry back in `Cargo.lock`.
