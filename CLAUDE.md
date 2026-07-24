# Kyde

Fast native macOS git commit/diff tool — an original take on the "commit changes" workflow
familiar from modern IDEs. Goal: **lightning fast**, native, polished and familiar look and
feel. No web, no Electron, no React.

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
  Push, Diff, New Branch, **Language Plugins**, **Fonts**, **Clear Data & Restart** — all are
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

## Stack & why
- **gpui** + **gpui_platform** (Apache-2.0) — Zed's GUI framework. Chosen over Tauri+Monaco
  because the user wants lower latency than JVM/Swing IDEs give, which needs a native GPU stack.
  Decision was: build FRESH on the gpui crate, STUDY Zed for patterns — do NOT fork Zed
  (Zed editor is GPL-3.0, huge, tightly coupled).
- **git binary, shelled out** — same as Zed's `crates/git`. No libgit2/git2 dependency.
- **similar** (Apache-2.0) — line + word diff. Swap to `imara-diff` (what Zed uses) only if
  large-file diffs lag.

## Layout — a Cargo workspace
The root package is the **gpui binary**; the logic lives in **library crates** under `crates/`
(compiler-enforced boundaries, real test targets, independent rebuilds). **gpui is isolated to
the UI layer**: only `kyde-ui` (the widget toolkit) and the binary depend on it. The eight
model/logic crates are gpui-free — `kyde-git`, `kyde-diff`, `kyde-tree`, `kyde-markdown`,
`kyde-update`, `kyde-config`, **`kyde-theme`, `kyde-syntax`** (verify: `cargo tree -p
kyde-syntax | grep gpui` is empty). Colours are **`kyde_color::Color`** — a zero-dep POD in the
tiny `kyde-color` crate; its **optional `gpui` feature** (on only in the binary + `kyde-ui`)
adds `From<Color>` for gpui's `Rgba`/`Hsla`/`Fill`/`Background`, so `.bg(theme_color)` /
`.text_color(theme_color)` still compile with zero call-site changes. `kyde-color` is the one
non-UI crate that *names* gpui, and only behind that off-by-default feature.
Each extracted crate is **aliased back to its old module name** in `main.rs`
(`use kyde_git as git;`, `use kyde_config::keymap;`, …) so every existing `git::` /
`crate::theme::` call site across the binary compiles unchanged.
The binary is grouped into **tiers**, each a module folder. A feature module is an `impl Kyde`
block (reaches `Kyde`'s private fields directly, like `render.rs` always did); a method called
from another module is `pub(crate)`, feature-internal ones stay private. `main.rs` re-exports
shared items (`Divider`, the `ui` toolkit, a few consts) at the crate root, and re-aliases the
widget/util submodules (`use widgets::editor;`, `use platform::shellcmd;`), so `editor::` /
`ui` / `Divider` references resolve unchanged via `use crate::*` in every module.
```
# ── core shell (src/) ──
main.rs       struct Kyde + its ~80 fields, actions!/keymap wiring, native menu/dock,
              ModalWindow, free render helpers, the mod/use/re-export wiring, main().
app.rs        controller core: new/repo/refresh/reload, menus, save/autosave, effective_lang.
render.rs     `impl Render for Kyde` (dispatch) + shared shell helpers (with_scrollbars,
              editor_island_w, render_context_menu).
divider.rs    unified divider dragging (Divider enum + geometry + drag methods).

# ── overlay ──
# `overlay()` (the dismiss-backdrop) lives in main.rs — it pokes Kyde fields, so it's app
# glue, not a reusable component. Everything else reusable is the kyde-ui crate (below).

# ── views/ — per-feature modules (render_* + logic for one feature) ──
browse  tabs  commit  diff_view  push  branch  history  finder  find  rollback
file_ops  modals  onboarding  projects_view  notifications  terminal_panel

# ── widgets/ — gpui-coupled widgets (own Entities/Elements) ──
editor/  mdview.rs  terminal.rs  remote_img.rs

# ── platform/ — small OS utils (no gpui) ──
clipboard.rs  scratch.rs  shellcmd.rs

# ── workspace crates (pure Rust, no Kyde; see crates/<name>) ──
kyde-git      Repo: discover/status/numstat/base_content/working_content/stage/unstage/
              stage_content/apply_patch/commit + Commit/ChangedFile/FileStatus. Shells out
              to `git`. (thiserror: GitError)
kyde-diff     FileDiff::compute() → line Hunks + word ranges; stats();
              partial_new_content(); hunk_patch(). (similar)
kyde-tree     Tree::build/visible — the file-tree model. (std)
kyde-markdown Block/Span markdown model for the preview. (pulldown-cmark)
kyde-update   GitHub release check + self-update download/swap. (thiserror: UpdateError, serde_json)
kyde-config   keymap + plugins + projects: config/persistence (JSON, XDG). (serde)
kyde-color    tiny RGBA `Color` POD shared by theme/syntax. Zero deps; optional `gpui`
              feature → `From<Color>` for gpui `Rgba/Hsla/Fill/Background` (UI layer only).
kyde-theme    runtime dark palette (theme::get/merge, hex JSON). (kyde-color, serde) — gpui-free
kyde-ui       reusable app-agnostic UI toolkit: btn_primary/secondary, tab_pill, Badge +
              file_badge, checkbox, menu_icon, lerp_rgb, scrollbar_thumb, line_stats
              (`+a −r` label), the file-tree row `tree::item<V>` (generic over the view,
              optional trailing element), and `picker` (bounded nav_up/
              nav_down + the selected/hover row pill every list picker uses — finder,
              history, push). Aliased back as `ui` in main.rs. (gpui, kyde-theme)
kyde-syntax   tree-sitter highlight() + fold_regions(); OWNS every grammar crate behind
              per-pack features. Binary depends with default-features=false and forwards
              its own packs (`rust` → `kyde-syntax/rust`); kyde-syntax's own default is
              `full` so `cargo test -p kyde-syntax` covers all grammars. (kyde-color,
              tree-sitter*, kyde-theme) — gpui-free
```

## Theme — runtime config (`src/theme.rs` + `~/.config/kyde/theme.json`)
Colors are a **flat runtime struct** (`theme::Theme`), loaded lazily via `theme::get()`
(`OnceLock`), serialized as hand-editable `"#RRGGBB"` hex. The file **auto-repairs** on load
(`theme::merge`, pure + unit-tested): missing file → write defaults; missing/invalid keys →
filled from defaults; unknown keys → dropped; valid per-key overrides preserved (editing one
color never loses the rest). Rewrites only when something changed. Access anywhere with
`theme::get().<field>` (e.g. `theme::get().primary`). Fonts stay compile-const in
`theme::font` (not themeable): `UI_FAMILY` = **Inter** (all chrome — trees, buttons,
overlays), `FAMILY` = **JetBrains Mono** (code surfaces — diff panes + editor), 13 / 1.2.
Both OFL, bundled in `assets/fonts/`, registered at startup via `main::load_fonts`
(`cx.text_system().add_fonts`). Chrome render fns thread a `ui` family arg; `render_diff`
ignores it and hard-codes `FAMILY`. (SF Mono was rejected — Apple license, not shippable.)

Defaults are an original, hand-authored dark palette in the broad style of modern IDE dark
themes (Darcula-family conventions), tuned for Kyde — not a copied or redistributed theme
file. Key colors and accents:
- `frame_bg` `#0D0E10` — window frame / gaps **behind** the rounded island panels (darkest
  surface; root + topbar + the padded `body` wrapper use it). `main_bg`/`panel_bg` `#191A1C`
  are the **island** surfaces (editor + tree), so they read as panels floating on the frame.
  `divider` (hr/border/secondary-btn border) `#26282B`; `bg_mid` `#26282B`; `bg_light`
  `#323438`. Island corner radius + frame gap: `theme::ISLAND_RADIUS` / `theme::FRAME_GAP`
  (non-themeable consts).
- `text` (general, everything but primary button) + `secondary_text` `#D1D3D9`.
- `primary` (filled button) `#3574F0`, `primary_text` `#FFFFFF`.
- `selected_bg` (selected sidebar/menu row) `#2E436E`; `caret_row` (editor current line)
  stays subtle `#1F2024` — distinct from selection.
- Secondary button = transparent bg + `divider` border + `secondary_text`.
- `status_*`, `diff_*_bg`, `syn_*` round out the palette. `syn_identifier`/`syn_operator`
  set to `#D1D3D9` so general code text matches the general text color.

## History view (git log — `render_history` + app.rs `history_*` + git.rs `log`/`diff_files`)
Third rail mode (`Mode::History`, `icons/history.svg`), reached by the rail clock icon →
`enter_history`. Three panes: **commit list** (left, `Repo::log(rev, 300)` → `git::Commit`
rows showing subject, decoration refs, `short · author · relative-date`), the selected
commit's **changed files** (middle), and a **read-only side-by-side diff** (right, reuses
`render_diff` + `load_diff_panes(readonly=true)`). State on `Kyde`: `history_rev`,
`history_commits`, `history_selected`, `history_files`, `history_file_selected`,
`history_compare`, `history_branch_open`, `history_scroll`.
- **Branch picker**: the `⎇ <rev>` chip toggles `history_branch_open` → a dropdown with a
  **search box** (`history_branch_query`, live-filters) over **LOCAL** (`Repo::branches()`)
  and **REMOTE** (`Repo::remote_branches()`, `refs/remotes/`) sections plus a "HEAD" entry;
  picking one → `set_history_rev` re-logs. Defaults to the current branch.
- **Path scope**: right-clicking a folder/file in the Browse tree → **Git History** calls
  `enter_history_for(path)`, which logs only commits touching that subtree (recursive for a
  folder) — `Repo::log`/`diff_files` take an `Option<&Path>` pathspec. A `▸ <path>` scope
  chip in the header clears it (back to whole-repo) on click. `history_path` holds the scope.
- **Compare modes** (`CompareMode`, segmented control top-right): `Before` = commit vs its
  parent (`<hash>^`), `Latest` = vs `HEAD`, `Local` = vs the working tree. `history_revs`
  maps the mode to `(from, to)` (to `None` = working tree); `select_history_commit` →
  `Repo::diff_files(from, to)` for the file list; `select_history_file` loads each side via
  `committed_content`/`working_content`.
- git.rs additions: `Commit` struct, `log`, generalized `diff_files(from, to: Option<&str>)`
  (shared by `push_files`), and the `parse_name_status` helper.

## Terminal panel (src/terminal.rs — `terminal` Cargo feature)
A real PTY-backed VTE terminal, bottom-docked with multi-tab support. **Gated behind the
`terminal` Cargo feature** (in `default`): off → the module + alacritty's ~2MB of `.rodata`
parse tables leave the binary entirely, same compile-time `cfg` gate as the language packs
(`cargo build --no-default-features --features rust,json` drops it). Engine = **`alacritty_terminal`
0.26** (Apache-2.0, the crate Zed uses) — grid + VTE + PTY in one. `futures` provides the
wakeup channel.
- `TerminalView` (Entity, Focusable): owns `Arc<FairMutex<Term<EventProxy>>>` + a `Notifier`
  (writes input/resize to the PTY) + the IO-thread `JoinHandle`. Typed text + control/arrow
  keys are translated to PTY bytes in `on_key` (Up/Down = shell history `ESC[A/B`, Ctrl+letter
  = control byte, Cmd-V = paste w/ bracketed-paste mode). **History, tab-completion, line
  editing are the shell's job** — we only relay keystrokes + render bytes.
- `EventProxy` (alacritty `EventListener`): the IO thread can't touch gpui entities (not
  `Send`), so it forwards `Event`s over a `futures::mpsc` channel to a `cx.spawn` foreground
  pump (`on_event`) → repaint on `Wakeup`, write-back on `PtyWrite`, title/exit/clipboard.
- `TerminalElement` (custom Element, like `editor::EditorElement`): each frame locks the grid,
  measures the monospace cell, computes cols/rows from bounds + `resize`s the PTY, then shapes
  one `ShapedLine` per visible row with per-cell fg/bg (resolved via `ANSI_PALETTE` / 256-cube
  in `default_indexed`, OSC overrides honoured) + a block/beam/underline cursor.
- State on `Kyde` (all `#[cfg(feature = "terminal")]`): `term_tabs: Vec<Entity<TerminalView>>`,
  `term_active`, `term_open`, `term_height`, `term_resizing`. `act_toggle_terminal` (⌃`)
  toggles the panel + lazily spawns the first tab; `render_terminal_panel` draws the drag-resize
  divider + tab strip (title/×/＋, IntelliJ-style) + active terminal. Panel is only shown with a
  project open (the shell roots at `repo_root`).
- KNOWN SCAFFOLD GAPS: no mouse text-selection/copy yet (Cmd-C); Esc dispatches the app
  `EscapeKey` (root "Kyde" context) instead of reaching the terminal; no scrollback search.

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
--all-features -- -D warnings`** (the `--workspace` lints all ten crates, not just the binary;
`--all-features` reaches the cfg-gated grammar/terminal/remote-images paths) on **both
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

Smoke-tested: launches, renders, no panic. NOTE: `screencapture` of the window fails
silently unless the terminal has macOS Screen-Recording permission (System Settings →
Privacy & Security → Screen Recording) — grant it if you want to script screenshots.

## Roadmap
1. ✅ gpui window + 3-pane layout
2. ✅ live `git status` → colored changed-files tree
3. ✅ side-by-side diff with line-hunk backgrounds + word-range model
4. ✅ clickable center-gutter `»`/`☐` (revert hunk / include hunk in commit)
5. ✅ editable commit message + Commit button → `Repo::commit()`
6. ✅ Browse mode: expandable folder tree (`src/tree.rs`), tree-sitter highlighter
   (`src/highlight.rs`), real editor (`src/editor.rs`), `Repo::save_file`.

## Branch switcher (src/git.rs + render_status_bar/render_branch_popup)
Bottom status bar (`render_status_bar`, shown only when a repo is open) has a clickable
`⎇ <branch>` chip at the **bottom-right**. `Kyde.current_branch` is refreshed in
`refresh()` from `Repo::current_branch` (`git symbolic-ref --short HEAD`, `None` = detached).
Clicking → `toggle_branch_popup`: loads `branch_list` via `Repo::branches`
(`git for-each-ref --sort=-committerdate refs/heads/` = recency order) and focuses
`branch_query` (single-line `CodeEditor`, live-filters on `EditorEvent::Changed`).
`render_branch_popup` (anchored bottom-right, transparent backdrop closes it) shows: search
box, **+ New Branch** (`create_branch` = `git checkout -b`, name from the query), **Recent**
(top 5 by recency, current excluded), **All Branches** (alphabetical, current marked `✓`).
Clicking (or right-clicking) a branch row opens the branch ACTIONS menu at the cursor
(`MenuTarget::Branch`, IDE-style — no instant checkout): **Checkout** (`checkout_branch`
→ `git checkout` + `refresh`; worktree-aware — jumps when checked out elsewhere, hidden in a
pinned linked worktree) and **Merge “X” into “Y”** (`menu_merge_branch` — see Merge view).
Rows carry `↑a ↓b` ahead/behind badges vs current (background gather on open). Debug shot:
`KYDE_SHOT=branch-menu`.

## Worktree switcher (src/views/worktree.rs + kyde-git `Repo::worktrees`)
A `layers.svg` chip next to the branch chip (hidden when the repo has no linked worktrees —
`worktree.list.len() ≤ 1`). `Repo::worktrees()` wraps `git worktree list --porcelain`
(`Worktree { path, branch, head, is_main }`, bare/prunable skipped; parser =
`parse_worktrees`, unit-tested) and is read in the refresh `RepoSnapshot` (off the UI
thread) into `Kyde.worktree: WorktreePopup`. Clicking → `toggle_worktree_popup`: rows show
dir name, `⎇ branch`, a changed-files count badge (one background `git status` per worktree,
gathered ON POPUP OPEN only — `counts`/`counts_gen`, never on the render path) and `✓` on
the active one; clicking a row → `switch_worktree` → `open_project(path)` (session
save/restore preserves per-worktree UI state). The **branch popup is worktree-aware**:
a branch checked out in another worktree shows a layers-icon + dir-name marker, and
`checkout_branch` jumps to that worktree (`other_worktree_for_branch`) instead of letting
`git checkout` fail with "already checked out at …". Inside a *linked* worktree
(`in_linked_worktree`) plain checkouts are disabled — rows render dimmed/inert (the
worktree is pinned to its branch); jump rows, the current branch, and + New Branch
(`checkout -b` doesn't touch files) stay active. The chip carries a worktree-count badge +
"Worktrees" tooltip. New icons go in the `Assets` `include_bytes!` match in main.rs — an
unregistered path renders as NOTHING, silently (layers.svg was caught in QA).

## Merge view (src/views/merge.rs + kyde-git merge ops + kyde-diff::merge)
Branch-to-branch merge with IntelliJ-style conflict resolution. **Branch popup**: leaf rows
carry `↑a ↓b` ahead/behind badges vs the current branch (`Repo::branch_ahead_behind`, one
`rev-list --left-right --count` per branch, gathered in background ON POPUP OPEN — the
worktree-counts pattern); right-click a row → `MenuTarget::Branch` menu with **Merge “X” into
“Y”** / Checkout. `menu_merge_branch` runs `Repo::merge_branch` (`git merge --no-edit`) off
the UI thread → `MergeOutcome::{UpToDate, Merged, Conflicts}`. Clean/up-to-date → a neutral
✓ note banner (`merge.note`, ×-dismissed). Conflicts → the merge stays IN PROGRESS and the
**resolve window** opens (`ModalKind::Merge`, a native window at the main window's bounds),
a TWO-STAGE dialog:
- **Stage 1 — conflicts list** (`render_conflicts_list`, always shown first): "Merging branch
  X into branch Y" over a table of conflicted files — badge + name + dir, and **Yours/Theirs
  columns** (Modified/Added/Deleted per side, from `Repo::conflict_entries` = one
  `git ls-files -u -z`, classified by which index stages exist). Row selection +
  **Accept Yours / Accept Theirs** (whole-file resolve: stage-2/3 content, or delete when
  that side deleted — `git checkout --ours/--theirs` semantics) and **Merge…** (or
  double-click) → stage 2. Resolved rows show ✓ and go inert.
- **Stage 2 — 3-pane resolve** (`render_merge_resolve`): toolbar = ↑/↓ chunk nav
  (`merge_nav_chunk`, far left), ‹ Back, **Compare Contents** dropdown (`MergeCompare` via
  `ui::select` — hand-rolled panels got clipped by later-painted pane siblings): the 3-pane
  view; **Left/Right and Middle** = INTERACTIVE 2-pane subsets of the live merge panes
  (same editors + apply gutter — the middle IS the editable result); Base pairs + Left and
  Right = read-only comparisons in separate `cmp_l`/`cmp_r` editors (`compare_pair` returns
  `Some` only for these). Then **Apply non-conflicting changes: » Left · »« All · « Right**
  (`merge_apply_clean`), and a
  **whitespace mode** dropdown (`WhitespaceMode::{Exact,Trim,IgnoreAll}`, UI default
  `IgnoreAll` — comparison normalizes, display never changes; switching re-chunks the
  file). Three read-only
  `CodeEditor` panes — **Yours | Result | Theirs** — row-aligned via per-chunk fillers and
  one shared `ScrollHandle`; two 56px gutters put `»`/`«` (apply) + `×` (ignore) on EVERY
  changed chunk's first row (✓/− = undo). Model = `kyde_diff::merge::Merge3` (diff3-style:
  `side_hunks` base→ours + base→theirs aligned over base; touching regions merge → conflict,
  like git). NOTHING auto-applies: every non-stable chunk has a `Resolution { ours, theirs:
  Pending|Applied|Ignored }` (`Same` chunks ride the ours side); the center pane rebuilds
  from `result_lines` on every change; pending conflicts keep base, tinted
  `theme.diff_conflict_bg` (a palette key, in the CVD variants too; ignored chunks go
  `diff_deleted_bg` grey). Footer: **Abort Merge**, Accept Yours/Theirs, **Apply** (gated on
  ALL chunks decided; save merged text + `git add`, back to the list) — the list offers
  **Commit Merge** (`git commit --no-edit` → git's `MERGE_MSG`) once every file is resolved.
- **Banner** (`render_merge_banner`, bottom stack): shown whenever the refresh snapshot sees
  MERGE_HEAD (`Repo::merging()` → friendly branch name), so a conflicted merge/pull started
  OUTSIDE kyde is also offered Resolve/Commit/Abort. Conflicted count comes live from
  `git status` (`FileStatus::Conflict`). State lives in `MergeView` (`merge` on `Kyde`);
  stage contents come from `Repo::conflict_stage` (`git show :1/2/3:path`).
- Screenshots: `KYDE_SHOT=merge-conflicts` (list) / `merge` (3-pane, non-conflicting applied)
  / `merge-compare` (manual) + `KYDE_SHOT_REPO=<clone with an in-progress conflicted merge>`
  (fixture built by scripts/screenshots.sh; region capture, 2 windows like rollback).

## Compare view (src/views/compare.rs — issue #42)
Compare any two files side-by-side in a native window (`ModalKind::Compare`, opens at the
main window's bounds like Diff/Merge). Entry points: **cmd-click two files** in the Browse
tree (multi-select, `BrowseView.multi_selected` — ordered, cleared by any plain click or
project switch; rows render selected) → right-click → **Compare Selected**; or right-click
a non-active **tab** → **Compare with Current Tab** (active tab = left). `CompareView`
(`compare` on `Kyde`): two read-only `CodeEditor` panes row-aligned via the diff_view
helpers (`diff_line_bgs`/`diff_word_bgs`/`diff_fillers`) + one shared `ScrollHandle` (the
merge Compare Contents pattern); per-side `effective_lang` syntax. The center gutter puts
**« »** on every hunk's first aligned row (from `aligned_rows`): `»` copies the LEFT
side's lines into the right FILE, `«` the reverse — both directions come from
`FileDiff::partial_new_content` (right←left = all hunks except i; left←right = only i —
outside hunks the sides are identical). The header offers whole-file `«`/`»` (make one
side match the other) + a difference count. Applying WRITES the target file
(`write_open_file` — repo/root/absolute-scratch all work), reloads a clean open Browse
buffer showing it, re-diffs in place, and refreshes git status. Smoke test:
`compare_applies_hunks_both_directions`. Debug shot: `KYDE_SHOT=compare` +
`KYDE_SHOT_FILE`/`KYDE_SHOT_FILE_B`.

## Local History (issue #7 — crates/kyde-local-history + src/views/local_history.rs)
IntelliJ-style per-file snapshots, independent of git. **Model = `kyde-local-history`**
(pure Rust, sha2 only — already transitively in-tree): per-project store under
`$XDG_DATA_HOME/kyde/local-history/<name>-<fnv64(path)>/` with **content-addressed blobs**
(`blobs/<aa>/<hash>`, SHA-256, temp+rename writes — identical content stored once, an
unchanged save writes zero bytes) and an **append-only `events.jsonl` journal**
(`Event { ts_ms, path, hash, kind: Change|External|Label, label }`; corrupt lines skipped on
load, never fatal). `Store::prune` (run once per project-open, in the background) drops
events past retention, rewrites the journal atomically, GCs unreferenced blobs, and is
`prune.lock`-guarded against a second instance (stale >10min locks stolen). Timestamps are
caller-supplied (testable); `format_ts` (civil-from-days, tz offset passed in — the app reads
`date +%z` once) + `relative_ts` are pure. Perf guards: `perf_record_large_file_stays_fast`,
`perf_load_10k_event_journal_stays_fast`.
- **Recording** (`views/local_history.rs`, `LocalHistoryView` = `lh` on `Kyde`; ALL store IO
  off the UI thread, everything gated on the master switch): saves funnel through
  `lh_note_save` → pending set → ONE throttled flush (default 10s) reads each file's FINAL
  on-disk state (`lh_flush`) — a burst's last save is never lost, dedup makes no-ops free.
  `lh_note_open` records a **baseline** on a file's first sight and **External change** when
  disk ≠ last snapshot. Destructive ops snapshot targets FIRST via `lh_snapshot_now` (inline
  read — the caller is about to overwrite — background write): "Before rollback" /
  "Before checkout X" / "Before delete" / "Before hunk revert" / "Before compare apply" /
  "Before merge resolve" / "Before revert", plus "Commit: <subject>" stamped on committed
  files. Label events always append (timeline markers) even at unchanged content.
  `lh_sync_store` (called from `refresh`) keeps the store pointed at the open project.
- **Window** (`ModalKind::LocalHistory`, opens at the main window's bounds): left column =
  snapshot timeline (title + `format_ts · relative_ts`) over a **changed-since panel**
  (IDE-style): the distinct files of `events[0..=selected]` that still DIFFER from
  their state at the snapshot (a touched-but-changed-back file — deleted then restored —
  is dropped; content-hash check per candidate, on selection change only), as a
  fully-expanded tree (`tree::Tree` + `ui::tree::item`); clicking a file shows ITS diff. Right = snapshot ↔
  current read-only aligned panes (the compare-view pattern: `diff_line_bgs`/
  `diff_word_bgs`/`diff_fillers`, shared `ScrollHandle`); the snapshot side is the file's
  **effective base** (`lh_effective_base_for`: newest event at-or-before the selected row,
  else — first seen after it — its OLDEST later event, usually the open baseline; header
  says "First seen · ts" then). Every panel file therefore has a working diff + revert. Center gutter `»` = restore ONE hunk
  (`FileDiff::partial_new_content(|j| j != hi)`), header **Revert to This Version** = the
  targeted file. **Right-click menus** (`MenuTarget::LhRow`/`LhPath`, rendered in THIS
  window like the rollback modal; items live in `lh_menu_items` — `render_context_menu`
  only dispatches): a timeline row offers **Revert This Change and After** (every
  changed-since file back to its state at the snapshot), a file row **Revert This File**
  (JUST it), a folder row **Revert This Folder** (its subtree). Every revert labels the
  pre-write state "Before revert" INLINE (`lh_label_sync` — not the background
  `lh_snapshot_now`, so the immediately-reloaded timeline shows the marker), reloads a
  clean open Browse buffer, re-diffs, refreshes git status; a first-seen-after file
  reverts to its earliest-known state, never deleted (guessing a deletion would be worse). Entry points: right-click
  file/FOLDER/tab → **Local History**, ⌘⇧A palette. A folder (or repo-root) scope lists
  every file's events under it (`Store::events_under`, component-wise prefix); rows/header
  carry the file name.
- **Config** (`kyde-config::history::HistoryCfg` → `history.json`): `enabled` (default ON —
  dedup + debounce make steady-state cost ≈0), `retention_days` (7, clamp 1..=90),
  `throttle_secs` (10, clamp 1..=300). Settings → **Local History** section (toggle +
  steppers; toggling opens/drops the store live). Off = zero work: no store, no reads, no
  writes.
- **Clear Local History** (destructive): Settings → Local History button + ⌘⇧A palette →
  `open_clear_local_history` → a native confirmation window (`ModalKind::ClearLocalHistory`,
  Enter confirms / Escape cancels like the other confirm dialogs) → `do_clear_local_history`
  → `Store::clear()` (deletes the journal + every blob, resets the in-memory index; the
  store stays open and recording continues from empty) + empties any open timeline. Both
  entry points need a store (project open + enabled).
- Smoke tests: `local_history_records_opens_and_saves`, `local_history_revert_restores_the_
  snapshot`, `local_history_disabled_records_nothing`,
  `local_history_changed_since_panel_and_reverts`, `local_history_clear_confirms_and_wipes`.
  Debug shot: `KYDE_SHOT=local-history` + `KYDE_SHOT_FILE=<json>` (seeds two snapshots
  synchronously, then opens the window) — in the README set (`scripts/screenshots.sh
  local-history`, region mode, 2 windows; the script exports a throwaway `XDG_DATA_HOME` so
  shot seeding never writes into the user's real history store).

## Window chrome — native blend + activity rail (render)
The window uses a **transparent titlebar** (`WindowOptions.titlebar = TitlebarOptions {
appears_transparent: true, traffic_light_position: point(16,16) }`) so our `frame_bg` chrome
shows behind the macOS traffic lights — no separate toolbar. Layout under the root (frame_bg,
flex_col): a draggable `titlebar` strip (h40, `pl(84)` to clear the traffic lights,
`window_control_area(WindowControlArea::Drag)`), then `main_row` (flex_row) = the **left
activity rail** (`RAIL_W` 48px) + the padded island `body`. The rail holds two icon buttons —
`icons/folder.svg` = Browse, `icons/git-branch.svg` = Commit — active one tinted `text` +
`bg_light`, else `line_number`. Icons are **Lucide SVGs** (MIT) in `assets/icons/`, served by
the `Assets` `AssetSource` (`Application::with_assets`) and drawn with `svg().path(..)`
(`stroke="currentColor"` → colored via `.text_color`). Resize math accounts for the rail:
`tree_width = cursor.x − RAIL_W − FRAME_GAP`.

## Side-by-side diff (render_diff + aligned_rows)
`render_diff` renders **row-aligned** rows, NOT two independent columns: `aligned_rows(d)`
flattens `FileDiff` into `DiffRow { old, new, hunk, kind, hunk_start }` — equal regions
advance both sides in lockstep, each hunk pairs its old/new lines and pads the shorter side
with filler (`None`). Each row is `[left flex_1 min_w_0 | gutter w56 flex_none | right
flex_1 min_w_0]`, so the two panes are always 50/50 and vertically aligned, and the center
gutter chevrons line up with their hunk (gutter content only on the `hunk_start` row, via
`hunk_controls`). Cells are `whitespace_nowrap` + `overflow_hidden` (no wrap → uniform
`row_h` = 18px → alignment holds). Lines are syntax-colored with `editor::line_runs` +
`gpui::StyledText::with_runs`, using spans cached on `Kyde.old_spans/new_spans`
(computed in `select()` via `effective_lang`, so no per-render reparse; empty when the
file's pack isn't installed). Each hunk's gutter row has a checkbox (include this hunk in
the commit — unticked hunks stay on disk but out of the commit, IntelliJ/VSCode-style
partial commit; state = `CommitView.excluded_hunks`, applied by `commit_now` via
`FileDiff::partial_new_content` + `Repo::stage_content`) and a `»` (revert the hunk).
`render_diff_modal` reuses `render_diff`. `line_byte_starts` maps line
index → byte offset so per-line span slicing matches `highlight::highlight`'s indices.

## Browse file tree (src/tree.rs + render_browse)
`tree::Tree::build(&all_files)` turns the flat sorted `Repo::list_files()` (gitignored
already excluded) into a lazy dir→children map (root = `""`); rebuilt in `refresh`. Children
sort folders-first then case-insensitive name. `Tree::visible(&expanded)` DFS-flattens to
`Row { path, is_dir, depth }`, descending only into expanded dirs. State on `Kyde`:
`file_tree`, `expanded: HashSet<PathBuf>` (toggled by `toggle_dir`, dir-row click), plus
`tree_width: f32` / `tree_resizing: bool` for the drag-resizable divider. Rows: `▸`/`▾`
chevron + folder SVG (`icons/folder.svg`) for dirs; for files `file_badge()` returns a
`Badge` — `Tag(monogram, color)` for known types (rs/ts/js/json/md/css/html/sh) or
`Icon("icons/file-lines.svg", color)` for everything else (generic lines/document icon).
SVGs come from the `Assets` source. `depth*14px` indent; each row is `mx(6)` + `rounded_md`
so the hover/selected background is an inset rounded pill (IntelliJ-style), scrollable.
The divider (`browse-divider`, `cursor_col_resize`) sets `tree_resizing`; the root's
`on_mouse_move`/`on_mouse_up` update `tree_width` (cursor x, clamped 180–900) accounting for
`RAIL_W + FRAME_GAP`. Right-click a row always opens the menu: Commit/Rollback only when
`has_changes_under`, plus **Reveal in Finder** (`reveal_in_os` → `open -R`) always.
`apply_snapshot` drops Deleted-status paths from `all_files` — `git ls-files` keeps
listing a tracked file whose working copy was deleted, and a nonexistent file in the
tree/⌘P reads as a bug (the deletion still shows in the Commit view).

## Editor tabs (render_tab_bar)
Opening a file appends to `open_tabs: Vec<PathBuf>` (deduped, open order); `open_path` is the
active tab. `render_tab_bar` draws one tab each (file_badge icon + name + `×`), active tab on
`main_bg` (others `panel_bg`); the `×` closes (`close_tab`, `cx.stop_propagation()` so it
doesn't also activate), active+dirty shows `●` instead. Left-click activates (re-`open_file`),
right-click opens `MenuTarget::Tab(idx)` → Close / Close Others / Close Tabs to the Right /
Reveal in Finder (`close_tab`/`close_other_tabs`/`close_tabs_right`, each picking a sensible
new active tab; empty → `clear_open`). `open_tabs` cleared on project switch. No tabs →
`render_no_file`.

## The editor (src/editor.rs)
Real gpui text widget, modeled on gpui's `examples/input.rs` but multi-line. `CodeEditor`
entity + custom `EditorElement` (Element impl). Typed text comes through
`EntityInputHandler::replace_text_in_range` (IME-correct); control keys via `actions!` +
`KeyBinding` (bound once in `editor::bind_keys`, key_context "CodeEditor"). Offsets are
UTF-8 bytes. Caret/selection painted in `prepaint`; layout cached in `paint` for mouse +
vertical movement. Used for BOTH the file editor and the commit box (lang=PlainText).
Remaining: undo/redo, soft-wrap, caret-follow scrolling, rope buffer for huge files.

## Keymap / finder / onboarding (src/keymap.rs + main.rs)
- `keymap.rs`: `Keymap { preset, overrides }` serialized to `~/.config/kyde/keymap.json`
  (XDG_CONFIG_HOME respected). `ACTIONS` table holds each configurable action's name +
  per-preset default keystroke + label. `key_for(name)` = override else preset default.
  `Keymap::load()` returns `(km, first_run)`; first_run drives onboarding.
- `main::apply_keymap(cx, &km)` clears ALL bindings then rebinds: editor keys
  (`editor::bind_keys`), finder nav (context "FileFinder", fixed), and the configurable
  app actions (global, context None). Call it again after a preset change.
- gpui action types live in `main.rs` (`actions!`). Key contexts: "Kyde" (root, app
  actions), "CodeEditor"/"CodeInput" (multi/single-line editors), "FileFinder" (overlay).
  Single-line inputs use "CodeInput" so Enter/Up/Down bubble to the finder instead of being
  eaten by the editor.
- Finder: `finder_query` is a single-line `CodeEditor`; `cx.subscribe` to its
  `EditorEvent::Changed` re-runs `recompute_finder` (fuzzy-matcher / SkimMatcherV2).
  `act_go_to_file` focuses the input immediately **and** via `window.defer` (the input
  element isn't in the tree yet on first open). Each result row shows its `file_badge()`
  icon. Single-line `CodeEditor`s render with a **transparent** background (only multi-line
  editors paint `main_bg`) and paint the caret even when empty, so a focused search box
  reads as focused with no box behind the placeholder.
- **Find in Files** (`FinderMode::Content`, `find_in_files` = ⌘⇧F both presets) reuses the
  same overlay: the query is a **literal** (non-fuzzy) full-text search via `Repo::grep`
  (`git grep -F -n -I -i --untracked`, exit-1/no-match → empty, capped 500), recomputed live
  on each keystroke into `content_results: Vec<ContentHit{path,line,text}>`. A count strip
  ("N matches in M files") sits under the input; each result row shows the matched line
  (mono font, trimmed/capped 200ch) + `path:line`. Enter/click → `open_file_at_line` (opens
  in Browse, selects the line, scrolls it ~3 rows below the top via `file_scroll.set_offset`).
  Also reachable from the ⌘⇧A palette ("Find in Files").
- Onboarding overlay = **first-run keymap picker only**. On first run it's **forced**
  (`onboarding_forced`): no Close button, non-dismissable backdrop (`overlay(cx, dismissable)`)
  — a keymap MUST be chosen. Preset cards select on click; `onboarding_choice` holds the
  pending pick; the **Continue** button confirms via `choose_preset` (saves, re-applies).
- **Settings window** (`src/views/settings.rs`, `ModalKind::Settings`): Kyde → Settings… / ⌘,
  (`OpenKeymap` → `open_keymap` → `open_settings`) opens a native `ModalWindow` with an
  IntelliJ-style sidebar (`SettingsSection`: Appearance / Keymap / Language Packs) + content
  pane. **Appearance** = theme (Dark; presets later) + live px steppers for `ui_font_size` /
  `editor_font_size` / `tree_row_height` (`theme::update(|t| …)` mutates the RwLock-backed live
  theme + saves + repaints, no restart — see `kyde-theme`). **Keymap** = preset picker
  (`choose_preset`). **Language Packs** = `render_plugins_body`. Quit = `Quit` → `cx.quit()`.
- **Shell-command checkbox** (`render_shell_command_row`, shown in the picker on both first
  run and reopened Settings). Ticked + Continue → `shellcmd::install()` symlinks our
  `current_exe()` into `~/.local/bin/ky` (or `kyde` if `ky` is taken), VSCode-style — no
  shell-rc editing, no sudo (that dir is on PATH and user-writable). `shellcmd::state()`
  (pure, unit-tested) drives the row: `Installed`/`Available`/`NameTaken`/`Unavailable`; it
  scans the install dir first then the rest of `$PATH`, treating our own symlink as "installed"
  and any other command under the name as a conflict it won't clobber. Default-checked
  (`onboarding_install_cmd: true`); errors surface in `shell_cmd_error` under the row.

## Language packs (opt-in highlighting — `src/highlight.rs` + `src/plugins.rs`)
Syntax highlighting is a **plugin**: nothing is parsed by default (speed). Each
`Lang` maps to an installable `Pack` (`highlight::PACKS`). On opening a file,
`Kyde::effective_lang` highlights with the real grammar only if the pack is
installed, else falls back to `PlainText` (no tree-sitter) and shows a top-of-editor
"Install <name> support?" banner (`render_install_banner`, primary button
`theme::ui::ACCENT` = accent blue `#3473EE`). Installed packs persist to
`~/.config/kyde/plugins.json` (`plugins::Plugins`, XDG-respecting like keymap).
Shipped packs: JSON, TypeScript (ts+tsx), JavaScript, Rust, Markdown (block-only),
Shell (bash), CSS, SCSS (reuses CSS grammar), `.env`, `.gitignore`. `.env`/`.gitignore`
use small builtin line highlighters (no grammar). tree-sitter core bumped to **0.25**
because tree-sitter-md 0.5 emits grammar ABI 15 (0.24's highlighter caps at ABI 14).

### Error highlighting (on by default per pack — issue #47)
Wavy red squiggles under parse errors (tree-sitter `ERROR` + `MISSING` nodes), **ON by
default for every installed pack**, with a per-pack opt-OUT: the "Error highlighting"
checkbox on each installed pack's row in the Language Plugins window
(`toggle_pack_errors`; hidden for "font" — not a language). Persisted as
`errors_disabled` in plugins.json (`Plugins::errors_on/set_errors`; empty set — and any
pre-existing plugins.json — means all on; uninstall drops the opt-out, so reinstall
returns to default-on). Pipeline: `kyde_syntax::error_ranges(source, lang)` (pure —
parses, prunes clean subtrees via `node.has_error()`, merges overlaps, widens
zero-width MISSING to one char; perf guard `perf_error_ranges_large_files_stay_fast`)
→ cached on `CodeEditor.error_ranges`, recomputed with `spans` in `recompute_folds`
only when `errors_on` (`set_error_highlight`, set by the app from
`Kyde::errors_enabled_for(lang)` on open/install/toggle — flag BEFORE
`set_content`/`set_lang` so one recompute does the right thing) → element prepaint
splits each visible row's `TextRun`s at error boundaries (`apply_error_squiggles`,
pure + unit-tested) and sets `underline: wavy` in `theme.syn_error` (new palette key,
CVD variants ride the conflict accent) — the squiggle paints inside the normal
`ShapedLine` pass, so virtualization is free (off-screen rows never split). Squiggles
appear wherever the editor's flag is on (Browse; diff/merge panes never set it).
Debug shot: `KYDE_SHOT=error-highlight` + `KYDE_SHOT_FILE=<invalid .json>`.

### ⌘-click navigation (on by default per pack — issue #26)
Hold ⌘ and hover a NAVIGABLE thing → accent underline + pointer cursor; ⌘-click acts.
Three targets (classified by `CodeEditor::symbol_at`, in priority order):
1. **An import specifier** → opens the referenced file. `kyde_syntax::import_links`
   (Rust `mod x;`/`use` paths incl. wildcards + `as`; TS/JS `import`/`export from`/
   `require()`/dynamic `import()`; Python `import a.b`/`from .rel import c`) →
   `EditorEvent::OpenImport` → `Kyde::open_import_link` → `kyde_syntax::resolve_import`
   (pure: TS relative specifiers + extension/index candidates, Python dotted/relative →
   `x.py`/`__init__.py`, Rust `mod` → sibling and `crate::`/`super::`/`self::` →
   progressively-shorter `src/` paths; bare specifiers = external → `None`) against
   `browse.all_files`.
2. **A symbol defined in THIS buffer** (vars/types/fns — `kyde_syntax::definition_sites`:
   Rust items + `let`/params/closures/for-patterns, TS/JS declarations + declarators/
   params/methods/arrows, Python `def`/`class`/assignments/params/for) → the editor
   jumps in place (`select_range`, reveals). Definitions shadow import bindings.
3. **A use of an IMPORTED symbol** (`kyde_syntax::import_bindings` — Rust use
   names/aliases/lists, TS default/named-`as`/namespace, Python module first-segments +
   `from` names/aliases) → `EditorEvent::OpenSymbol{link,name}` →
   `Kyde::open_import_symbol`: resolve the import's file, open it (new tab), find
   `name` in ITS definition_sites, select it.
4. **METHODS/symbols defined in imported files** (`obj.method()` where the class came
   from an import): `Kyde::refresh_external_defs` resolves every import target and
   indexes those files' `definition_sites` in the BACKGROUND (name → path+range,
   pushed via `CodeEditor::set_external_defs`; recomputed only when the buffer's
   import TARGET SET changes — `import_targets()` comparison — and generation-guarded
   against stale results; cleared by `set_content`). Lowest lookup priority; click →
   `EditorEvent::OpenDefinition{path,range}` → `Kyde::open_definition_at`. No type
   inference — a name defined in several imported files resolves to the first.
ON by default for every installed pack that supports it (Rust, TS/TSX, JS, Python);
per-pack opt-OUT via the "⌘-click imports" checkbox (persisted `links_disabled`,
`Plugins::links_on/set_links` — the error-highlighting model). Editor caches
(`import_links` + `import_bound` + `defs` = `compute_nav`) recompute with `spans` only
when `links_on` (`set_link_navigation`, pushed from `Kyde::links_enabled_for` at the
same four sites as the error flag); perf guards `perf_import_links_*` +
`perf_definition_sites_*`. ⌘ state rides each mouse-move's OWN `modifiers` (a
`ModifiersChanged` only reaches the editor via focus — unreliable on unfocused panes)
AND `on_modifiers_changed` recomputes hover from `last_mouse`, so pressing ⌘ over a
symbol lights it up without moving. Underline = `apply_underline_ranges` on
`hover_range`. Smoke tests: `cmd_click_import_opens_the_target_file`,
`cmd_click_symbol_jumps_to_its_definition`. Debug shot: `KYDE_SHOT=imports`
(`force_link_hover`).

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
Cargo feature, `pack_ext`/`pack_size` in modals.rs, and tests for each capability
added (the `every_lang_with_a_pack_actually_highlights` style). Langs 4+5 currently
cover: Rust, TS/TSX, JS, Python. JSON additionally has `sort_object_keys` (with JS/TS).

### Two independent layers — Cargo features (build) vs install list (runtime)
The plugin system is **two separate gates**, do not conflate them:
- **Cargo features** (`Cargo.toml [features]`, one per pack: `rust`, `typescript`,
  `css` (= CSS+SCSS), …, plus `full` = all, `default = ["full"]`). These are
  **compile-time `cfg` gates** (conditional compilation, like `#ifdef`), NOT runtime
  feature flags — resolved once at build, baked into the binary. Each gates (a) the
  `optional` grammar crate dep and (b) the matching arms in `highlight::config()` /
  `grammar()` / the `PACKS` table, all `#[cfg(feature = "…")]`. An off feature drops
  the grammar crate **and** its code; the lang then collapses to the existing
  "no pack → `PlainText`" path (zero new runtime branches). A `_ => return None`
  catch-all keeps both matches exhaustive under any feature combo. **GOTCHA:** with
  *zero* grammars (`--no-default-features`), every value-producing arm `cfg`s out and the
  match is only `_ => return None`, so (a) the result type is uninferable — both fns carry an
  explicit annotation (`let (...): (tree_sitter::Language, &str, &str, &str)` /
  `let lang: tree_sitter::Language`), and (b) the code after the match is unreachable + `lang`
  unused — both fns carry `#[allow(unreachable_code, unused_variables)]` (no-ops with ≥1
  grammar). Keep these when adding/refactoring grammars, or `cargo build --no-default-features`
  breaks (E0282 + warnings).
- **Install list** (`plugins.json`, `plugins::Plugins`) — the **runtime** toggle:
  which *compiled-in* grammar is active for this user (drives the install banner).

**Why the Cargo features exist (memory + size, the speed/footprint pitch):** the
runtime opt-in (PlainText-by-default) already saves *heap* — no `HighlightConfiguration`
built, no parse tree / span `Vec` retained — but it canNOT reclaim the grammar parse
tables themselves: those are `static` data in the binary's `.rodata`, linked in and
demand-paged into resident RAM regardless of `plugins.json`. The only way to shed them
is to not link them — i.e. a Cargo feature. Measured (release, `lto=thin`): `full`
**18.57 MB** vs `--no-default-features --features rust,json,toml` **12.81 MB**
= **−5.76 MB (−31%)** binary + resident RAM (and ~3× faster compile). So: runtime
install list = heap + per-keystroke parse cost; Cargo features = binary image + the
`.rodata` parse tables. Trim builds:
```sh
cargo build --release                                          # full (default)
cargo build --release --no-default-features --features rust,json,toml
```

## Projects landing view (src/projects.rs + main.rs)
`repo_root: Option<PathBuf>` — `None` renders the Projects view (`render_projects`), `Some`
renders Commit/Browse. No CLI arg → `None` (landing); a path arg opens it directly.
`projects::Recents` (most-recent-first, deduped, capped 50) persists to
`~/.config/kyde/projects.json`; `open_project` touches+saves+refreshes. Rows show a
colored initials chip (`color_for`/`initials`), name, `~`-abbreviated path (`pretty_path`),
and branch read straight from `.git/HEAD` (`branch_of`, no shell). Search box filters by
substring. "Open"/"New Project" → `pick_folder` (native `cx.prompt_for_paths`, dirs only,
async via `cx.spawn`). The OS folder dialog has no initial-dir field in gpui 0.2.2, so
"default to ~" isn't forced. (No Clone Repository — deliberately dropped.)

Launch: `kyde` shell function in `~/.zshrc` runs the newest of
`target/{release,debug}/kyde`, args passed through (bare = Projects view).
(`gs` is ghostscript — not aliased.)

## Sort ops (issues #43/#41 — Sort Lines + Sort Object Keys)
Right-clicking the editor pane (the `MenuTarget::EditorGit` menu) offers, above the git
commands: **Sort Lines** when the selection spans ≥2 lines OR the caret sits in a
JSON/JS/TS object, and **Sort Object Keys** when the caret sits in such an object.
**Inside an object, Sort Lines DELEGATES to the key sort** (`object_sort`, issue #43's
"within a JSON object it should just sort json") — a textual line sort there would move
entries without their commas and break the syntax, so the textual path never runs when
an object encloses the caret (even an already-sorted one). Availability is computed at MENU-OPEN in
browse.rs (never in the render arm — an open menu must not re-parse per frame) and rides
two bools on the `EditorGit` variant; the right-click first moves the caret under the
pointer via `CodeEditor::caret_to` (kept if clicking inside the selection — IDE
convention). Both are also configurable keymap actions (`sort_lines` ⌥⌘L / `sort_keys`
⌥⌘S, both presets) and ⌘⇧A palette entries; handlers are gated to Browse with a file
open. Logic is pure + unit-tested: `editor::sort_lines_in` (expand selection to whole
lines, case-insensitive stable sort, selection ending at a line start excludes that
line) and `kyde_syntax::sort_object_keys` (SELECTION-aware: a caret sorts the innermost
enclosing `object`; a ranged selection is whitespace-trimmed — selecting a block WITH its
indent targets the block, not the enclosing object — and sorts the smallest covering
node, so a selection across sibling objects in an array sorts each of them; recursive,
formatting-preserving — entry texts move verbatim, comma/indent separators stay in their
slots, arrays keep element order, objects with spreads/methods/comments/errors never
reorder). Both apply via `replace_range_text` (one undo step, no-op when
already sorted / read-only) and re-select the sorted block. Smoke test:
`sort_ops_rewrite_selection_and_object`. Debug shot: `KYDE_SHOT=sort-menu` +
`KYDE_SHOT_FILE=<json>`.

## Back/forward file navigation (⌘⌥← / ⌘⌥→)
Visited-file history on `BrowseView` (`nav_history`/`nav_index`/`nav_suppress`, capped
100): every `open_file_inner` records a visit (`nav_record` — consecutive dupes collapse;
tab re-activation records nothing); `nav_back`/`nav_forward` walk it and reopen via the
normal open path with `nav_suppress` on, so navigating never records; opening a NEW file
after going back truncates the forward branch (IDE convention). Configurable actions
`nav_back`/`nav_forward` (⌘⌥←/⌘⌥→ both presets) + ⌘⇧A palette entries; history cleared
on project switch/close. Smoke test: `nav_back_and_forward_walk_the_visit_history`.

## Views & right-click flow (main.rs)
Opening a project lands in **Browse (code) view**, not git — `open_project`/`new` default
`Mode::Browse`. Git is reached on demand:
- Right-click a file in the Browse tree → **Commit** → switches to Commit view, selecting
  that file if it's changed (`menu_commit_file`).
- Right-click a changed file in the Commit tree → **Show Diff** (floating diff viewer over
  the commit view, `render_diff_modal`, reuses `render_diff`) or **Stage** (`stage_file`,
  whole-file `git add`).
- Right-click Browse file → **Rollback** → `render_rollback_modal`: checkbox tree of all
  changes (pre-checked), a "Delete local copies of added files" toggle, Close/Rollback.
  Right-click a row → Show Diff (`MenuTarget::RollbackFile`). `do_rollback` per checked file:
  modified/deleted → `git checkout HEAD -- f` (`Repo::discard`); added (staged-new) → unstage
  + optional `delete_file`; untracked → `delete_file` only if delete-added is set.
- Context menu = `context_menu: Option<ContextMenu{at: Point<Pixels>, target}>`, opened by
  `MouseButton::Right` handlers carrying the cursor position; rendered absolutely at `at`
  with a transparent dismiss backdrop (`render_context_menu`). `MenuTarget` = `BrowseFile`
  (Commit + Rollback) / `CommitFile` (Show Diff + Stage) / `RollbackFile` (Show Diff). The
  shared `overlay()` backdrop closes finder/onboarding/diff modal (rollback is `overlay(false)`
  = modal, closed via its Close button).

## Module status
- Plain Rust, tested, now **own workspace crates** (`crates/<name>`): `kyde-git`,
  `kyde-diff`, `kyde-tree`, `kyde-markdown`, `kyde-update`, `kyde-config` (keymap/plugins/
  projects), `kyde-theme`, `kyde-syntax` (highlight + grammars).
- gpui but **Kyde-agnostic**, its own crate: `kyde-ui` (the reusable toolkit — buttons, badge,
  tree row, …; depends only on gpui + kyde-theme).
- Plain Rust, still in the binary (small OS utils, not yet crated): `platform/{scratch,
  shellcmd}.rs`.
- gpui UI in the binary: core shell `main.rs`/`app.rs`/`render.rs`/`divider.rs`; the `views/`
  feature modules; the `widgets/` (editor, mdview, terminal, remote_img).
  Compile on gpui 0.2.2.
- **The `Kyde` god struct is decomposed into feature-owned sub-structs** (all defined at the
  crate root in `main.rs`, fields reachable from the feature modules like `Kyde`'s own):
  `BrowseView` (`browse` — tree, tabs, file editor, md split), `CommitView` (`commit`),
  `DiffPanes` (`diff` — both diff editors + model, shared by commit/history/push/Show-Diff),
  `HistoryView` (`history`), `BranchPopup` (`branch`), `SyncState` (`sync` — ahead/behind +
  push/pull/fetch), `Finder` (`finder`), `FindBar` (`find`), `Onboarding`, `Fps`, `TermState`
  (`term` — control state always compiled, PTY tabs feature-gated inside). Each `new(cx)`
  constructor owns its editors + subscriptions. `Kyde` keeps only core/shared state (repo
  root, `files`/`selected` repo status, mode, window handles, drags) — ~60 fields, down from
  ~120. `refresh` reads a `RepoSnapshot` on a background thread (never git on the UI thread);
  op-errors raised alongside a refresh go through `fail_pending`/`pending_error`, not
  `op_error`, so the async status read can't wipe them.

## Performance regression tests (the speed pitch is the whole point)
"Lightning fast" is a hard requirement, so the hot paths have **perf-guard unit
tests** (`fn perf_*`, in the same module's `#[cfg(test)] mod tests`). They run a
representative-sized input through a hot path and `assert!` it finishes under a
time budget via `std::time::Instant`. Existing guards:
- `highlight.rs::perf_highlight_and_fold_large_file_stays_fast` — `highlight()` +
  `fold_regions()` on ~4000 lines (both run on **every keystroke**).
- `diff.rs::perf_compute_large_diff_stays_fast` — `FileDiff::compute()` on 4000
  lines (runs on every file selection).

Conventions when adding/maintaining them:
- **Loose budgets on purpose** (currently 2s for work that takes ms). The goal is
  to catch algorithmic blowups — accidental O(n²), re-parse loops, per-keystroke
  reparse of the whole buffer — NOT 2× CI jitter. Don't tighten to "realistic"
  numbers; that just makes them flaky on slow/loaded machines.
- Name them `perf_*` so `cargo test perf` runs only the guards; add
  `-- --nocapture` is not needed (the failure message prints the measured time).
- Add a guard whenever you introduce a new per-keystroke / per-frame / per-select
  hot path (e.g. a rope buffer, word-diff on huge files, tree rebuilds). Keep the
  comment pointing back here.
- They live in `mod tests` (not `tests/`) because this is a bin crate with no lib
  target, so integration tests can't reach the pub fns.
- gpui entry point is `Application::new().run(...)` (no `gpui_platform` crate — that was a
  research error; everything is in the single `gpui` crate, font-kit on by default).

## gpui gotchas
- API on crates.io moves fast; builder/method names in `main.rs` may drift from installed
  0.2.x. Verify with `cargo doc -p gpui --open` and the `gpui/examples` in the Zed repo.
- Non-UI code (`git.rs`, `diff.rs`, `theme.rs`) is plain Rust and stable.
- gpui gives no text-editor widget — that's step 6's main cost.

## Reference (read for patterns; GPL, do NOT copy code)
- Diff = Editor over MultiBuffer + DiffTransforms: Zed `crates/editor`, `multi_buffer`, `buffer_diff`.
- Per-hunk stage via partial patch: Zed `crates/git_ui`, `editor/src/git.rs`.
- Syntax highlight: Zed `crates/language/src/syntax_map.rs` (tree-sitter + `.scm` queries).
- Reusable directly (Apache-2.0): `gpui`, `sum_tree`, `util`, `collections`.
