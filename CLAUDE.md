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
  because the user wants beyond-WebStorm latency, which only a native GPU stack gives.
  Decision was: build FRESH on the gpui crate, STUDY Zed for patterns — do NOT fork Zed
  (Zed editor is GPL-3.0, huge, tightly coupled).
- **git binary, shelled out** — same as Zed's `crates/git`. No libgit2/git2 dependency.
- **similar** (Apache-2.0) — line + word diff. Swap to `imara-diff` (what Zed uses) only if
  large-file diffs lag.

## Layout — a Cargo workspace
The root package is the **gpui binary**; the pure, gpui-free logic lives in **library
crates** under `crates/` (compiler-enforced boundaries, real test targets, independent
rebuilds). Each extracted crate is **aliased back to its old module name** in `main.rs`
(`use kyde_git as git;`, `use kyde_config::keymap;`, …) so every existing `git::` /
`crate::theme::` call site across the binary compiles unchanged.
The binary is split into **per-feature modules**, each a crate-root child module with an
`impl Kyde` block (so it reaches `Kyde`'s private fields directly, like `render.rs` always
did). A method called from another module is `pub(crate)`; feature-internal ones stay private.
`main.rs` re-exports shared items (`Divider`, the UI toolkit, a few consts) at the crate root
so every module gets them via `use super::*`.
```
# ── binary: core shell ──
src/main.rs   struct Kyde + its ~80 fields, actions!/keymap wiring, native menu/dock,
              ModalWindow, free render helpers, the `mod`/`use`/re-export wiring, main().
src/app.rs    controller core: new/repo/refresh/reload, menus, save/autosave, effective_lang,
              escape/fps, mode switch. (Was ~3.4k lines; feature logic now lives per-module.)
src/render.rs `impl Render for Kyde` (the dispatch) + shared shell helpers (with_scrollbars,
              editor_island_w, render_context_menu). (Was ~6.4k lines.)
src/ui.rs     shared UI toolkit — btn_primary/btn_secondary, tab_pill, menu_icon, lerp_rgb,
              scrollbar_thumb, pack metadata. Re-exported at the crate root.
src/divider.rs unified divider dragging (Divider enum + geometry + drag methods).

# ── binary: feature modules (each = render_* + logic for one feature) ──
src/browse.rs       file tree + editor pane + markdown/font preview + open-file/preview-tab
src/tabs.rs         editor tab strip + close logic
src/commit.rs       git staging view (changed-files tree, per-hunk stage, message box)
src/diff_view.rs    side-by-side diff, gutter staging, hunk nav, Show-Diff view + modal body
src/push.rs         push tab + push confirmation modal
src/branch.rs       branch switcher + status bar + fetch/pull/push
src/history.rs      git-log view (commit list + files + read-only diff)
src/finder.rs       fuzzy finder + Find-in-Files + command palette
src/find.rs         in-editor find/replace bar
src/rollback.rs     rollback (discard) modal
src/file_ops.rs     new file / rename / delete / scratch / reveal
src/modals.rs       native modal windows + bodies (plugins, fonts, clear-data, new-branch)
src/onboarding.rs   first-run keymap picker + shell-command install
src/projects_view.rs projects landing + project open/switch/session plumbing
src/notifications.rs crash / op-error / update banners
src/terminal_panel.rs bottom terminal panel glue (terminal feature)

# ── binary: gpui-coupled widgets + small OS utils (not yet crated) ──
src/editor.rs / src/mdview.rs / src/terminal.rs / src/remote_img.rs / src/scratch.rs /
src/shellcmd.rs / src/clipboard.rs

# ── workspace crates (pure Rust, no Kyde; see crates/<name>) ──
kyde-git      Repo: discover/status/base_content/working_content/stage/unstage/apply_patch/
              commit + Commit/ChangedFile/FileStatus. Shells out to `git`. (anyhow)
kyde-diff     FileDiff::compute() → line Hunks + word ranges; hunk_patch(). (similar)
kyde-tree     Tree::build/visible — the file-tree model. (std)
kyde-markdown Block/Span markdown model for the preview. (pulldown-cmark)
kyde-update   GitHub release check + self-update download/swap. (anyhow, serde_json)
kyde-config   keymap + plugins + projects: config/persistence (JSON, XDG). (serde)
kyde-theme    runtime dark palette (theme::get/merge, hex JSON). (gpui Rgba, serde)
kyde-syntax   tree-sitter highlight() + fold_regions(); OWNS every grammar crate behind
              per-pack features. Binary depends with default-features=false and forwards
              its own packs (`rust` → `kyde-syntax/rust`); kyde-syntax's own default is
              `full` so `cargo test -p kyde-syntax` covers all grammars. (gpui Rgba,
              tree-sitter*, kyde-theme)
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
`[profile.dev]` is `opt-level = 1`, no LTO — gpui is fast enough in debug for UI testing. Add a
grammar to `--features` only when testing that language. Use `cargo check` for compile-verify
between edits. Run `cargo fmt` + `clippy` + `test` ONCE at the end (CI = fmt + clippy + test),
not per iteration; a default/release build is only for perf claims or shipping. NOTE: under
slim features the `kyde-syntax` highlight tests for un-built grammars (typescript, …) fail —
expected; use `--workspace` or `-p kyde-syntax` (full grammars) for a true green.

Smoke-tested: launches, renders, no panic. NOTE: `screencapture` of the window fails
silently unless the terminal has macOS Screen-Recording permission (System Settings →
Privacy & Security → Screen Recording) — grant it if you want to script screenshots.

## Roadmap
1. ✅ gpui window + 3-pane layout
2. ✅ live `git status` → colored changed-files tree
3. ✅ side-by-side diff with line-hunk backgrounds + word-range model
4. ✅ clickable center-gutter `»`/`☐` → `Repo::apply_patch()` (stage/revert hunk)
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
Clicking a branch → `checkout_branch` (`git checkout`) then `refresh`.

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
file's pack isn't installed). Clicking any changed line cell stages that hunk (= include);
gutter `»` reverts. `render_diff_modal` reuses `render_diff`. `line_byte_starts` maps line
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
  WebStorm/VSCode default keystroke + label. `key_for(name)` = override else preset default.
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
- Onboarding overlay = keymap picker. On first run it's **forced** (`onboarding_forced`):
  no Close button, non-dismissable backdrop (`overlay(cx, dismissable)`) — a keymap MUST be
  chosen. Preset cards select on click (highlight = thick `border_2` accent + a same-family
  `linear_gradient`); `onboarding_choice` holds the pending pick; the bottom-right primary
  **Continue** button confirms via `choose_preset` (saves, re-applies, clears forced).
- Reopen any time via the **native menu**: Kyde → Settings… (`cx.set_menus`, dispatches
  `OpenKeymap`; also bound to ⌘,). Quit = `Quit` action → `cx.quit()`. No in-app toolbar
  button (settings is native-menu-only).
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
`theme::ui::ACCENT` = WebStorm blue `#3473EE`). Installed packs persist to
`~/.config/kyde/plugins.json` (`plugins::Plugins`, XDG-respecting like keymap).
Shipped packs: JSON, TypeScript (ts+tsx), JavaScript, Rust, Markdown (block-only),
Shell (bash), CSS, SCSS (reuses CSS grammar), `.env`, `.gitignore`. `.env`/`.gitignore`
use small builtin line highlighters (no grammar). tree-sitter core bumped to **0.25**
because tree-sitter-md 0.5 emits grammar ABI 15 (0.24's highlighter caps at ABI 14).

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
  catch-all keeps both matches exhaustive under any feature combo.
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
- Plain Rust, still in the binary (small OS utils, not yet crated): `scratch.rs`,
  `shellcmd.rs`, `clipboard.rs`.
- gpui UI in the binary: `main.rs` (entry/wiring/overlays/helpers), `app.rs` (Kyde controller
  methods), `render.rs` (`impl Render` + `render_*`), `editor.rs` (text widget),
  `mdview.rs`, `terminal.rs`, `remote_img.rs`. Compile on gpui 0.2.2. The god trio
  (main/app/render) is the next decomposition target (feature-owned state/entities).

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
