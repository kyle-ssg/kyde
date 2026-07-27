# Kyde

Fast native macOS git commit/diff tool — an original take on the "commit changes" workflow
familiar from modern IDEs. Goal: **lightning fast**, native, polished and familiar look and
feel. No web, no Electron, no React.

> This file holds the **rules and conventions** — the things not derivable from the code.
> Per-feature behaviour is deliberately NOT documented here: read the module. Feature docs
> rot faster than they help, and every session pays to load them.

## Hard requirements (the whole point)
- Genuinely fast — native GPU rendering and low input latency are non-negotiable (the
  motivation is JVM/Swing IDEs feeling sluggish for this one workflow).
- Polished, familiar dark look & feel — an original theme tuned to feel at home for IDE
  users (see Theme below). No vendor code or assets copied.
- Side-by-side diff with **word-level inline highlighting** and a **center gutter** whose
  `»` chevrons + checkboxes stage/revert individual hunks (like `git add -p`, IntelliJ-style).
- Folder open + per-file editing with tree-sitter syntax highlighting.

## UI principles (non-negotiable)
- **Every modal is a native OS window (`ModalWindow`), never an in-app overlay.** Rollback,
  Push, Diff, New Branch, Language Plugins, Settings, Merge, Compare, Local History — all are
  separate native windows with a real macOS titlebar, opened via `open_modal_window(kind,
  title, w, h, cx)` and dispatched through `ModalKind` → `render_*_body` (each body fills the
  window via `size_full`; the window provides chrome/bg/font). To add a modal: add a
  `ModalKind` variant, a `*_win: Option<WindowHandle<ModalWindow>>` field + `modal_slot` arm,
  a `ModalKind` arm in `ModalWindow::render`, and a `pub(crate) fn render_<x>_body(&mut self,
  cx)`. Do NOT build modal dialogs as `overlay(cx, _)` children of the root. (The fuzzy
  finder and first-run keymap picker are transient *overlays*, not modals — they stay as-is.)
- **Buttons use the shared `btn_primary` / `btn_secondary` helpers** (`render.rs`), never
  hand-rolled. Primary = accent fill + `primary_text`; secondary = transparent + `divider`
  border + `secondary_text`. Caller chains `.on_mouse_down(...)`.
- **New icons MUST be registered** in the `Assets` `include_bytes!` match in `main.rs`. An
  unregistered path renders as NOTHING, silently — this has bitten us in QA. Icons are
  Lucide SVGs (MIT) in `assets/icons/`, drawn via `svg().path(..)` with `stroke="currentColor"`
  so `.text_color` tints them.
- **Never run git on the UI thread.** `refresh` reads a `RepoSnapshot` on a background
  thread. Same for any store/network IO (local-history writes, release feeds).
- **Menu availability is computed at menu-OPEN, never in the render arm** — an open menu
  must not re-parse per frame.

## Stack & why
- **gpui** (Apache-2.0) — Zed's GUI framework. Chosen over Tauri+Monaco because the user
  wants lower latency than JVM/Swing IDEs give, which needs a native GPU stack. Decision was:
  build FRESH on the gpui crate, STUDY Zed for patterns — do NOT fork Zed (Zed editor is
  GPL-3.0, huge, tightly coupled).
- **git binary, shelled out** — same as Zed's `crates/git`. No libgit2/git2 dependency.
- **similar** (Apache-2.0) — line + word diff. Swap to `imara-diff` (what Zed uses) only if
  large-file diffs lag.
- **alacritty_terminal** (Apache-2.0) — grid + VTE + PTY for the terminal panel.
- **tree-sitter 0.25** — grammars. (0.24's highlighter caps at ABI 14; tree-sitter-md 0.5
  emits ABI 15.)

## Layout — a Cargo workspace
The root package is the **gpui binary**; the logic lives in **library crates** under `crates/`
(compiler-enforced boundaries, real test targets, independent rebuilds). **gpui is isolated to
the UI layer**: only `kyde-ui` (the widget toolkit) and the binary depend on it — the other ten
crates are gpui-free (verify: `cargo tree -p kyde-syntax | grep gpui` is empty). Colours are
**`kyde_color::Color`** — a zero-dep POD in the tiny `kyde-color` crate; its **optional `gpui`
feature** (on only in the binary + `kyde-ui`) adds `From<Color>` for gpui's
`Rgba`/`Hsla`/`Fill`/`Background`, so `.bg(theme_color)` / `.text_color(theme_color)` still
compile with zero call-site changes. `kyde-color` is the one non-UI crate that *names* gpui,
and only behind that off-by-default feature.

Each extracted crate is **aliased back to its old module name** in `main.rs`
(`use kyde_git as git;`, `use kyde_config::keymap;`, …) so every `git::` / `crate::theme::`
call site across the binary compiles unchanged.

The binary is grouped into **tiers**, each a module folder. A feature module is an `impl Kyde`
block (reaches `Kyde`'s private fields directly, like `render.rs` always did); a method called
from another module is `pub(crate)`, feature-internal ones stay private. `main.rs` re-exports
shared items (`Divider`, the `ui` toolkit, a few consts) at the crate root, and re-aliases the
widget/util submodules (`use widgets::editor;`, `use platform::shellcmd;`), so `editor::` /
`ui` / `Divider` references resolve unchanged via `use crate::*` in every module.

```
# ── core shell (src/) ──
main.rs       struct Kyde + its fields, actions!/keymap wiring, native menu/dock, ModalWindow,
              the Assets include_bytes! match, free render helpers, mod/use wiring, main().
app.rs        controller core: new/repo/refresh/reload, menus, save/autosave, effective_lang.
render.rs     `impl Render for Kyde` (dispatch) + shared shell helpers (with_scrollbars,
              editor_island_w, render_context_menu).
divider.rs    unified divider dragging (Divider enum + geometry + drag methods).
# `overlay()` (the dismiss-backdrop) lives in main.rs — it pokes Kyde fields, so it's app
# glue, not a reusable component. Everything reusable is the kyde-ui crate.

# ── views/ — per-feature modules (render_* + logic for one feature) ──
branch  browse  changelog  commit  compare  diff_view  file_ops  find  finder  history
local_history  merge  modals  notifications  onboarding  projects_view  push  rollback
settings  tabs  terminal_panel  worktree

# ── widgets/ — gpui-coupled widgets (own Entities/Elements) ──
editor/  mdview.rs  terminal.rs  remote_img.rs

# ── platform/ — small OS utils (no gpui) ──
clipboard.rs  scratch.rs  shellcmd.rs  instance.rs (single-instance socket)

# ── workspace crates (pure Rust, no Kyde; see crates/<name>) ──
kyde-git      Repo: discover/status/numstat/base_content/working_content/stage/unstage/
              stage_content/apply_patch/commit/log/diff_files/merge/worktrees + Commit/
              ChangedFile/FileStatus. Shells out to `git`. (thiserror: GitError)
kyde-diff     FileDiff::compute() → line Hunks + word ranges; stats(); partial_new_content();
              hunk_patch(); merge::Merge3 (diff3 3-way model). (similar)
kyde-tree     Tree::build/visible — the file-tree model. (std)
kyde-markdown Block/Span markdown model for the preview. (pulldown-cmark)
kyde-update   GitHub release check + self-update download/swap, plus the changelog feed
              (`release_notes`/`parse_releases`). (thiserror: UpdateError, serde_json)
kyde-config   keymap + plugins + projects + history cfg: persistence (JSON, XDG). (serde)
kyde-local-history  per-project snapshot store: content-addressed blobs + append-only
              events.jsonl journal, prune/GC. (sha2)
kyde-color    tiny RGBA `Color` POD shared by theme/syntax. Zero deps; optional `gpui`
              feature → `From<Color>` for gpui `Rgba/Hsla/Fill/Background` (UI layer only).
kyde-theme    runtime dark palette (theme::get/merge/update, hex JSON). (kyde-color, serde)
kyde-ui       reusable app-agnostic UI toolkit: btn_primary/secondary, tab_pill, Badge +
              file_badge, checkbox, menu_icon, select, lerp_rgb, scrollbar_thumb, line_stats,
              the file-tree row `tree::item<V>`, and `picker` (bounded nav + the selected/
              hover row pill every list picker uses). Aliased back as `ui`. (gpui, kyde-theme)
kyde-syntax   tree-sitter highlight() + fold_regions() + error_ranges() + import_links/
              definition_sites/resolve_import + sort_object_keys; OWNS every grammar crate
              behind per-pack features. Binary depends with default-features=false and
              forwards its own packs (`rust` → `kyde-syntax/rust`); kyde-syntax's own default
              is `full` so `cargo test -p kyde-syntax` covers all grammars. (kyde-color,
              tree-sitter*, kyde-theme)
```

**The `Kyde` god struct is decomposed into feature-owned sub-structs** (defined at the crate
root in `main.rs`, fields reachable from the feature modules like `Kyde`'s own): `BrowseView`,
`CommitView`, `DiffPanes`, `HistoryView`, `BranchPopup`, `SyncState`, `Finder`, `FindBar`,
`MergeView`, `CompareView`, `LocalHistoryView`, `ChangelogView`, `Onboarding`, `TermState`, …
Each `new(cx)` constructor owns its editors + subscriptions. `Kyde` keeps only core/shared
state. Add new feature state to a sub-struct, not to `Kyde`.

## Theme — runtime config (`crates/kyde-theme` + `~/.config/kyde/theme.json`)
Colors are a **flat runtime struct** (`theme::Theme`) behind an `RwLock`, serialized as
hand-editable `"#RRGGBB"` hex. The file **auto-repairs** on load (`theme::merge`, pure +
unit-tested): missing file → write defaults; missing/invalid keys → filled from defaults;
unknown keys → dropped; valid per-key overrides preserved (editing one color never loses the
rest). Rewrites only when something changed. Read anywhere with `theme::get().<field>`;
mutate live with `theme::update(|t| …)` (mutates + saves + repaints, no restart). Never
`.unwrap()` the lock — recover from poison (`kyde-theme::{read,write}_theme`).

Defaults are an original, hand-authored dark palette in the broad style of modern IDE dark
themes (Darcula-family conventions), tuned for Kyde — **not** a copied or redistributed theme
file. Keep it that way. The actual hex values live in the defaults; don't mirror them here.
Structural notes that aren't obvious from the values: `frame_bg` is the window frame *behind*
the rounded island panels, while `main_bg`/`panel_bg` are the island surfaces — so panels read
as floating. `ISLAND_RADIUS` / `FRAME_GAP` are non-themeable consts. Adding a palette key means
adding it to the CVD variants too.

Fonts stay compile-const in `theme::font` (**not** themeable): `UI_FAMILY` = **Inter** (all
chrome), `FAMILY` = **JetBrains Mono** (code surfaces — diff panes + editor). Both OFL, bundled
in `assets/fonts/`, registered at startup via `main::load_fonts`. Chrome render fns thread a
`ui` family arg; `render_diff` ignores it and hard-codes `FAMILY`. (SF Mono was rejected —
Apple license, not shippable.)

## Build / run
Rust 1.96 + Metal Toolchain are installed. gpui needs Apple's Metal Toolchain to compile
its shaders — if a fresh machine errors with "missing Metal Toolchain", run
`xcodebuild -downloadComponent MetalToolchain` (needs full Xcode, ~700MB).
```sh
cargo build              # the binary (default = full grammars + terminal)
cargo test --workspace   # binary tests + every crate's tests (the regression gate)
cargo test -p kyde-syntax  # one crate in isolation (its default = full grammars)
cargo run -- /path/to/any/git/repo
```
**Fast iteration when rebuilding just to click/screenshot-test:** build DEBUG with slim
features, run that binary — don't `--release` + clippy every loop. `[profile.release]` is
`lto = "thin"` + `codegen-units = 1` + `opt-level = 3` (40s–5min); `default = ["full", …]`
compiles every tree-sitter grammar (~18MB `.rodata`, the bulk of compile time). So:
```sh
cargo build --no-default-features --features terminal,rust,json && ./target/debug/kyde /path/to/repo
```
`[profile.dev]` is `opt-level = 1`, no LTO — fast incremental rebuilds — but
`[profile.dev.package."*"]` bumps every *dependency* to `opt-level = 3`, so gpui/alacritty/
tree-sitter run smoothly in debug (deps rarely recompile, so the cost is one-time). Add a
grammar to `--features` only when testing that language. Use `cargo check` for compile-verify
between edits. Run `cargo fmt` + `clippy` + `test` ONCE at the end (CI = fmt + clippy + test),
not per iteration; a default/release build is only for perf claims or shipping. NOTE: under
slim features the `kyde-syntax` highlight tests for un-built grammars (typescript, …) fail —
expected; use `--workspace` or `-p kyde-syntax` (full grammars) for a true green.

## Code-quality policy (enforced, CI fails otherwise)
Lints live centrally in **`[workspace.lints]`** (root `Cargo.toml`); every member opts in with
`[lints] workspace = true`. CI's `check` job runs **`cargo clippy --workspace --all-targets
--all-features -- -D warnings`** (the `--workspace` lints all eleven crates, not just the
binary; `--all-features` reaches the cfg-gated grammar/terminal/remote-images paths) on **both
ubuntu-latest AND macos-15** (Kyde ships macOS-only, so the `#[cfg(target_os="macos")]` code
must be gated — a Linux-only gate can't see it). `RUSTFLAGS: "-D warnings"` is workspace-wide,
so plain rustc warnings fail every step, not just clippy. Rules:
- **No `unwrap()`/`expect()` in non-test code** — `clippy::{unwrap_used,expect_used}` are
  `deny` (tests exempt via `clippy.toml`). A genuinely-infallible call is restructured away or,
  rarely, kept as `expect()` under a narrowest-scope `#[allow(clippy::expect_used)]` with a
  comment proving why (e.g. main-window open, PTY spawn, `build.rs`). Never `.unwrap()` a lock —
  recover from poison (`kyde-theme::{read,write}_theme`).
- **Typed errors** — library crates use **`thiserror`** (`kyde-git::GitError`,
  `kyde-update::UpdateError`); every variant carries context. `anyhow` is binary-only. No
  `Box<dyn Error>`.
- **`clippy::pedantic`** is on (`warn` + curated `allow`s in `[workspace.lints]`, each
  justified). Add a new allow there with a comment, never a blanket crate-root allow.
- **`#![deny(missing_docs)]`** on every lib crate; public items are documented, key pure
  entry points have **doctests** (run by `cargo test`).
- **MSRV `1.96`** — `rust-version` in every `Cargo.toml` + `rust-toolchain.toml`; the `msrv`
  CI job builds the workspace on 1.96. `unsafe` needs a `// SAFETY:` comment. Supply chain is
  gated by `cargo-deny` (advisories + licenses + bans).

### Workspace Cargo conventions (don't regress these)
- **Metadata + shared deps are inherited, never redeclared.** `[workspace.package]`
  (version/edition/rust-version/license/repository) and `[workspace.dependencies]`
  (gpui/anyhow/thiserror/serde/serde_json/futures/objc2*) live in the root `Cargo.toml`. A new
  crate uses `version.workspace = true`, `serde.workspace = true`, etc. — NOT a literal
  `version = "0.1.0"` / `serde = "1"`. Proof it's clean: `rg '^edition = "20' crates/*/Cargo.toml`
  and `rg '^anyhow = "1"' crates/*/Cargo.toml` must both be empty. release-please bumps
  `[workspace.package].version`.
- **gpui is pinned exactly** (`gpui = "=0.2.2"` in `[workspace.dependencies]`) and **`Cargo.lock`
  is committed** — reproducible builds. Don't loosen to `"0.2"`. `cargo build --locked` must pass.
- **`[profile.dev.package."*"] opt-level = 3`** optimizes *dependencies* in debug while our
  crates stay at `opt-level = 1` (fast incremental rebuilds). Keep both.
- **Features stay independent down to the empty set**: `cargo build --workspace
  --no-default-features` must be green (a CI-adjacent gate). See the zero-grammar gotcha under
  Language packs — `kyde-syntax`'s `config()`/`grammar()` need their explicit tuple/`Language`
  type annotation + `#[allow(unreachable_code, unused_variables)]`, because with no grammar
  feature every match arm `cfg`s out and the match collapses to `_ => return None`.

## Language packs — two independent gates
The plugin system is **two separate gates**, do not conflate them:
- **Cargo features** (`Cargo.toml [features]`, one per pack: `rust`, `typescript`,
  `css` (= CSS+SCSS), …, plus `full` = all, `default = ["full"]`). These are
  **compile-time `cfg` gates** (conditional compilation, like `#ifdef`), NOT runtime
  feature flags — resolved once at build, baked into the binary. Each gates (a) the
  `optional` grammar crate dep and (b) the matching arms in `config()` / `grammar()` / the
  `PACKS` table, all `#[cfg(feature = "…")]`. An off feature drops the grammar crate **and**
  its code; the lang then collapses to the existing "no pack → `PlainText`" path (zero new
  runtime branches). A `_ => return None` catch-all keeps both matches exhaustive under any
  feature combo. **GOTCHA:** with *zero* grammars (`--no-default-features`), every
  value-producing arm `cfg`s out and the match is only `_ => return None`, so (a) the result
  type is uninferable — both fns carry an explicit annotation (`let (...):
  (tree_sitter::Language, &str, &str, &str)` / `let lang: tree_sitter::Language`), and (b) the
  code after the match is unreachable + `lang` unused — both fns carry
  `#[allow(unreachable_code, unused_variables)]` (no-ops with ≥1 grammar). Keep these when
  adding/refactoring grammars, or `cargo build --no-default-features` breaks (E0282 + warnings).
- **Install list** (`plugins.json`, `plugins::Plugins`) — the **runtime** toggle: which
  *compiled-in* grammar is active for this user (drives the install banner).

**Why both exist:** the runtime opt-in (PlainText-by-default) saves *heap* — no
`HighlightConfiguration` built, no parse tree / span `Vec` retained — but it canNOT reclaim the
grammar parse tables: those are `static` data in the binary's `.rodata`, linked in and
demand-paged into resident RAM regardless of `plugins.json`. The only way to shed them is to
not link them — i.e. a Cargo feature. Measured (release, `lto=thin`): `full` **18.57 MB** vs
`--no-default-features --features rust,json,toml` **12.81 MB** = **−5.76 MB (−31%)** binary +
resident RAM, and ~3× faster compile.

### What "adding a language pack" MEANS (the per-language feature contract)
A pack is not just colors. Wiring a new `Lang` into kyde-syntax involves up to FIVE
per-language capabilities — implement what the grammar supports, and note the gaps:
1. **Highlighting** (required): `config()` arm (grammar + highlight query + CAPTURES).
2. **Folding** (free): `grammar()` arm — `fold_regions` works generically from it.
3. **Error highlighting** (free): `error_ranges` works generically from `grammar()`.
4. **⌘-click imports** (opt-in per lang): arms in `imports_with_bindings` (link + the
   names it binds) + a `resolve_import` strategy for the language's module system +
   the lang in `import_links`/`definition_sites`/`sort_object_keys` gate matches.
5. **Definitions** (opt-in per lang): `definition_sites` arms for the language's
   declaration forms (drives same-file jump + imported-symbol landing).
Also: `Lang::from_path` extension mapping, `Lang::pack` id, `PACKS` entry behind the
Cargo feature, `pack_ext`/`pack_size` in `views/modals.rs`, and tests for each capability
added (the `every_lang_with_a_pack_actually_highlights` style). Langs 4+5 currently
cover: Rust, TS/TSX, JS, Python. JSON additionally has `sort_object_keys` (with JS/TS).

Error highlighting and ⌘-click navigation are **ON by default** for every installed pack that
supports them, with a per-pack opt-OUT persisted in `plugins.json` (`errors_disabled` /
`links_disabled`; an empty set — and any pre-existing file — means all on, and uninstall drops
the opt-out so reinstall returns to default-on).

## Performance regression tests (the speed pitch is the whole point)
"Lightning fast" is a hard requirement, so the hot paths have **perf-guard unit
tests** (`fn perf_*`, in the same module's `#[cfg(test)] mod tests`). They run a
representative-sized input through a hot path and `assert!` it finishes under a
time budget via `std::time::Instant`. Guards exist for highlight+fold, error ranges,
import links, definition sites, diff compute, and the local-history store.

Conventions when adding/maintaining them:
- **Loose budgets on purpose** (currently 2s for work that takes ms). The goal is
  to catch algorithmic blowups — accidental O(n²), re-parse loops, per-keystroke
  reparse of the whole buffer — NOT 2× CI jitter. Don't tighten to "realistic"
  numbers; that just makes them flaky on slow/loaded machines.
- Name them `perf_*` so `cargo test perf` runs only the guards (the failure message
  prints the measured time — no `--nocapture` needed).
- Add a guard whenever you introduce a new per-keystroke / per-frame / per-select
  hot path (e.g. a rope buffer, word-diff on huge files, tree rebuilds). Keep the
  comment pointing back here.
- They live in `mod tests` (not `tests/`) because the binary has no lib target, so
  integration tests can't reach its pub fns.

## Testing conventions
- **Headless-gpui smoke tests** live in-module in the binary and drive real flows
  end-to-end. Prefer one per feature over narration in this file.
- Tests that touch the local-history store MUST call `isolate_history()` (sets a throwaway
  `XDG_DATA_HOME`), or the refresh-time scan writes into the developer's real
  `~/.local/share/kyde`.
- **Debug screenshots**: `KYDE_SHOT=<name>` (+ `KYDE_SHOT_FILE`, `KYDE_SHOT_REPO`, …) drives
  the app into a specific state for capture; `scripts/screenshots.sh` builds the fixtures.
  Shots share one config dir — a theme-changing shot must use `set_palette_ephemeral` (no
  save) or it taints later shots.
- **Single instance is off under `KYDE_SHOT`** (and via `KYDE_SINGLE_INSTANCE=0`), because the
  screenshot suite launches instances back to back and would otherwise forward into the first
  one instead of starting a fresh window.

## gpui gotchas
- API on crates.io moves fast; builder/method names may drift from installed 0.2.x. Verify
  with `cargo doc -p gpui --open` and the `gpui/examples` in the Zed repo.
- Entry point is `Application::new().run(...)` — there is no `gpui_platform` crate (that was
  an early research error); everything is in the single `gpui` crate, font-kit on by default.
- gpui gives no text-editor widget — `widgets/editor/` is ours, modeled on gpui's
  `examples/input.rs` but multi-line. Typed text comes through
  `EntityInputHandler::replace_text_in_range` (IME-correct); control keys via `actions!` +
  `KeyBinding`. Offsets are UTF-8 bytes. Still missing: soft-wrap, rope buffer for huge files.
- Background threads can't touch gpui entities (not `Send`). The pattern everywhere is:
  worker thread → `futures::mpsc` channel → a `cx.spawn` foreground pump. Used by the
  terminal `EventProxy`, the fs watcher, and the single-instance socket.
- Non-UI crates (`kyde-git`, `kyde-diff`, `kyde-theme`, …) are plain Rust and stable.
- `screencapture` of the window fails **silently** unless the terminal has macOS
  Screen-Recording permission (System Settings → Privacy & Security → Screen Recording).

## Reference (read for patterns; GPL, do NOT copy code)
- Diff = Editor over MultiBuffer + DiffTransforms: Zed `crates/editor`, `multi_buffer`, `buffer_diff`.
- Per-hunk stage via partial patch: Zed `crates/git_ui`, `editor/src/git.rs`.
- Syntax highlight: Zed `crates/language/src/syntax_map.rs` (tree-sitter + `.scm` queries).
- Reusable directly (Apache-2.0): `gpui`, `sum_tree`, `util`, `collections`.
