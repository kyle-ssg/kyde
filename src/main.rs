//! Kyde — fast native macOS git commit/diff tool, IntelliJ "Commit Changes" style.
//!
//! Features: changed-files tree + side-by-side diff, per-hunk stage/revert chevrons,
//! editable commit message, folder browse with a real syntax-highlighted editor,
//! a fuzzy "Go to File" finder (Cmd+Shift+O), a configurable keymap with WebStorm
//! and VSCode presets, and a re-accessible onboarding / keymap picker.

// ── core shell ──
mod app;
mod divider;
mod render;
pub(crate) use divider::{full_island_w, Divider, DIFF_GUTTER_W};
// Reusable, app-agnostic UI toolkit (its own crate). Aliased to `ui` (so `ui::tree::item`
// resolves) and glob-re-exported so call sites use `btn_primary`/`file_badge`/… unqualified.
pub(crate) use app::{
    CONTENT_MIN_QUERY, CONTENT_SEARCH_DEBOUNCE, FINDER_RESULT_CAP, SCROLL_CONTEXT_ROWS,
    STATUS_REFRESH_DEBOUNCE,
};
use kyde_ui as ui;
pub(crate) use kyde_ui::*;

// ── per-feature views (impl Kyde blocks; see src/views/) ──
mod views;

// ── gpui-coupled widgets; re-aliased so `editor::`/`mdview::`/… paths stay unchanged ──
mod widgets;
use widgets::editor;
use widgets::mdview;
#[cfg(feature = "remote-images")]
use widgets::remote_img;
#[cfg(feature = "terminal")]
use widgets::terminal;

// ── small OS utilities ──
mod platform;
use platform::{scratch, shellcmd};

// ── workspace crates, aliased back to their old module names ──
use kyde_config::keymap;
use kyde_config::plugins;
use kyde_config::projects;
use kyde_diff as diff;
use kyde_git as git;
use kyde_markdown as markdown;
use kyde_syntax as highlight;
use kyde_theme as theme;
use kyde_tree as tree;
use kyde_update as update;

use diff::{FileDiff, HunkKind};
use editor::{CodeEditor, EditorEvent};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use git::{ChangedFile, FileStatus, Repo};
use gpui::PathPromptOptions;
use gpui::{
    actions, div, img, prelude::*, px, svg, App, Application, Bounds, Context, Entity, FocusHandle,
    Focusable, FontWeight, KeyBinding, Menu, MenuItem, MouseButton, Pixels, Point, ScrollHandle,
    SharedString, Window, WindowBounds, WindowOptions,
};
use highlight::Lang;
use keymap::{Keymap, Preset};
use plugins::Plugins;
use projects::Recents;
use std::path::PathBuf;

// Configurable app actions (keystrokes come from the keymap config).
actions!(
    kyde,
    [
        GoToFile,
        FindInFiles,
        SaveFile,
        DoCommit,
        OpenKeymap,
        ModeCommit,
        ModeBrowse,
        Actions,
        NewScratch,
        EscapeKey,
        ToggleTerminal,
        NewTerminalTab,
        CloseTerminalTab,
        ClearTerminal,
        TerminalBackspace,
        TerminalEscape,
        DeleteFile,
        CloseTab,
        DiffNextChange,
        DiffPrevChange
    ]
);
// File-finder navigation (fixed keys, context "FileFinder").
actions!(
    kyde_finder,
    [FinderUp, FinderDown, FinderConfirm, FinderClose]
);
// In-editor find / replace (cmd-f / cmd-r, plus cmd-g navigation).
actions!(
    kyde_find,
    [
        FindInFile,
        ReplaceInFile,
        FindNext,
        FindPrev,
        CloseFind,
        ReplaceOne,
        ReplaceAll
    ]
);
// Native menu bar actions.
actions!(
    kyde_menu,
    [Quit, ToggleFps, ClearData, OpenPlugins, OpenProject]
);

/// The native macOS menu bar: the app menu (Settings/Plugins/Quit) + a File menu with
/// "Open…" and a live "Recent Projects" submenu. Rebuilt whenever recents change (startup +
/// each `open_project`) so the recent list stays current.
fn app_menus(recents: &Recents) -> Vec<Menu> {
    let recent_items: Vec<gpui::MenuItem> = recents
        .paths
        .iter()
        .take(15)
        .map(|p| {
            gpui::MenuItem::action(
                projects::name_of(p),
                OpenRecentProject(p.to_string_lossy().into_owned()),
            )
        })
        .collect();
    let mut file_items = vec![MenuItem::action("Open…", OpenProject)];
    if !recent_items.is_empty() {
        file_items.push(MenuItem::separator());
        file_items.push(MenuItem::submenu(Menu {
            name: "Recent Projects".into(),
            items: recent_items,
        }));
    }
    vec![
        Menu {
            name: "Kyde".into(),
            items: vec![
                MenuItem::action("Settings…", OpenKeymap),
                MenuItem::action("Plugins…", OpenPlugins),
                MenuItem::action("Toggle FPS Monitor", ToggleFps),
                MenuItem::separator(),
                MenuItem::action("Clear Data & Restart…", ClearData),
                MenuItem::separator(),
                MenuItem::action("Quit Kyde", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: file_items,
        },
    ]
}

/// Dock-tile menu action: open a specific recent project by its path. Carries
/// data, so it's a derived `Action` rather than a unit struct from `actions!`.
#[derive(Clone, PartialEq, Default, Debug, gpui::Action)]
#[action(namespace = kyde, no_json)]
struct OpenRecentProject(String);

/// Build the macOS Dock right-click menu: a "Recent Projects" submenu
/// (WebStorm-style), most-recent first. Empty when there are no recents.
/// Rebuilt on startup and whenever a project opens, so it stays current.
fn dock_menu(recents: &Recents) -> Vec<gpui::MenuItem> {
    let items: Vec<gpui::MenuItem> = recents
        .paths
        .iter()
        .take(15)
        .map(|p| {
            gpui::MenuItem::action(
                projects::name_of(p),
                OpenRecentProject(p.to_string_lossy().into_owned()),
            )
        })
        .collect();
    if items.is_empty() {
        return Vec::new();
    }
    vec![gpui::MenuItem::submenu(gpui::Menu {
        name: "Recent Projects".into(),
        items,
    })]
}

/// Clear and (re)apply all key bindings from a keymap config.
fn apply_keymap(cx: &mut App, km: &Keymap) {
    cx.clear_key_bindings();
    editor::bind_keys(cx);
    mdview::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new("up", FinderUp, Some("FileFinder")),
        KeyBinding::new("down", FinderDown, Some("FileFinder")),
        KeyBinding::new("enter", FinderConfirm, Some("FileFinder")),
        KeyBinding::new("escape", FinderClose, Some("FileFinder")),
    ]);
    bind_app(cx, km, "go_to_file", GoToFile);
    bind_app(cx, km, "find_in_files", FindInFiles);
    bind_app(cx, km, "save", SaveFile);
    bind_app(cx, km, "commit", DoCommit);
    bind_app(cx, km, "mode_commit", ModeCommit);
    bind_app(cx, km, "mode_browse", ModeBrowse);
    bind_app(cx, km, "open_keymap", OpenKeymap);
    bind_app(cx, km, "actions", Actions);
    bind_app(cx, km, "new_scratch", NewScratch);
    // Escape: close any open modal, else cancel the Commit view (fixed key).
    cx.bind_keys([KeyBinding::new("escape", EscapeKey, Some("Kyde"))]);
    // Backspace: delete the selected Browse-tree file/folder (fixed key). Bound to the
    // "Kyde" context, NOT globally, so the deeper editor/commit-box/terminal Backspace
    // bindings win whenever one of those is focused — this only fires at the app root.
    cx.bind_keys([KeyBinding::new("backspace", DeleteFile, Some("Kyde"))]);
    // Jump between diff changes (Alt+↓ / Alt+↑). Global so they fire while the diff editor is
    // focused; no-op when no diff is showing.
    cx.bind_keys([
        KeyBinding::new("alt-down", DiffNextChange, None),
        KeyBinding::new("alt-up", DiffPrevChange, None),
    ]);
    // Standard quit shortcut (not user-configurable).
    cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
    // Close the active editor tab (standard ⌘W; not user-configurable). Global (no context) so
    // it fires no matter what's focused — editor, tree, commit box, terminal.
    cx.bind_keys([KeyBinding::new("cmd-w", CloseTab, None)]);
    // Toggle the bottom terminal panel (fixed key, IDE-standard); ⌘T = new tab while the
    // terminal is focused (scoped to its key context so it doesn't shadow elsewhere).
    #[cfg(feature = "terminal")]
    cx.bind_keys([
        KeyBinding::new("ctrl-`", ToggleTerminal, None),
        KeyBinding::new("cmd-t", NewTerminalTab, Some("Terminal")),
        // ⌘W closes the active terminal tab when the terminal is focused (iTerm-style);
        // scoped to "Terminal" so it shadows the editor-tab ⌘W (`CloseTab`).
        KeyBinding::new("cmd-w", CloseTerminalTab, Some("Terminal")),
        // ⌘K clears the terminal; "Terminal" context (depth) beats the "Kyde"-scoped commit.
        KeyBinding::new("cmd-k", ClearTerminal, Some("Terminal")),
        // Bare backspace / escape are app shortcuts in "Kyde" (DeleteFile / EscapeKey). gpui
        // dispatches binding ACTIONS *before* on_key_down and an action stops propagation by
        // default, so on_key_down never runs once a binding matches — meaning a no-op here would
        // swallow the key (nothing reaches the PTY). So these route to actions whose handlers
        // (on TerminalView) write the raw byte to the shell, exactly like the editor binds
        // backspace to its own buffer action. The "Terminal" context (depth) shadows "Kyde".
        KeyBinding::new("backspace", TerminalBackspace, Some("Terminal")),
        KeyBinding::new("escape", TerminalEscape, Some("Terminal")),
    ]);
    // In-editor find / replace (fixed keys).
    cx.bind_keys([
        KeyBinding::new("cmd-f", FindInFile, None),
        KeyBinding::new("cmd-r", ReplaceInFile, None),
        KeyBinding::new("cmd-g", FindNext, None),
        KeyBinding::new("cmd-shift-g", FindPrev, None),
        KeyBinding::new("escape", CloseFind, Some("FindBar")),
        KeyBinding::new("enter", FindNext, Some("FindBar")),
        KeyBinding::new("shift-enter", FindPrev, Some("FindBar")),
    ]);
}

fn bind_app<A: gpui::Action>(cx: &mut App, km: &Keymap, name: &str, action: A) {
    if let Some(k) = km.key_for(name) {
        // Scope to the "Kyde" root context (NOT global/None) so a deeper widget context —
        // "Terminal", "CodeEditor" — cleanly overrides the same key by dispatch depth. A
        // context-less binding gets *max* depth and would tie with (and sometimes beat) the
        // widget binding (e.g. ⌘K commit shadowing the terminal's Clear). "Kyde" is always in
        // the focus stack (the root carries it), so these still fire everywhere else.
        cx.bind_keys([KeyBinding::new(&k, action, Some("Kyde"))]);
    }
}

/// Activity-rail width = button (38) + a frame-gap margin each side, so the icon sits with
/// equal gap to the window edge (left) and the island (right). The islands begin at this x.
const RAIL_W: f32 = 38.0 + theme::FRAME_GAP * 2.0;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Commit,
    Browse,
    History,
}

/// Which editor the in-editor find/replace bar acts on (set from focus when it opens).
#[derive(Clone, Copy, PartialEq)]
enum FindTarget {
    /// The Browse file editor.
    File,
    /// The diff's left (base) pane — read-only, so replace is disabled.
    DiffLeft,
    /// The diff's right (working) pane.
    DiffRight,
}

/// How the history view diffs the selected commit. `Before` = vs its parent (what the commit
/// changed), `Latest` = vs HEAD, `Local` = vs the working tree.
/// The two tabs of the git (Commit) view: staging working changes vs pushing committed ones.
#[derive(Clone, Copy, PartialEq)]
enum GitTab {
    Commit,
    Push,
}

#[derive(Clone, Copy, PartialEq)]
enum CompareMode {
    /// This commit vs its parent — what the commit changed (read-only).
    Before,
    /// This commit vs your working tree — editable.
    Local,
    /// The parent (before this commit) vs your working tree — editable.
    BeforeLocal,
}

impl CompareMode {
    const ALL: [CompareMode; 3] = [
        CompareMode::Before,
        CompareMode::Local,
        CompareMode::BeforeLocal,
    ];

    /// Short label for the dropdown trigger chip.
    fn label(self) -> &'static str {
        match self {
            CompareMode::Before => "Compare to previous commit",
            CompareMode::Local => "Compare with Local",
            CompareMode::BeforeLocal => "Compare before with Local",
        }
    }

    /// One-line explanation shown in the dropdown menu (clears up the taxonomy).
    fn desc(self) -> &'static str {
        match self {
            CompareMode::Before => "This commit vs its parent — what the commit changed",
            CompareMode::Local => "This commit vs your working tree — editable",
            CompareMode::BeforeLocal => {
                "The parent (before this commit) vs your working tree — editable"
            }
        }
    }

    /// Stable element-id key (independent of the display label).
    fn key(self) -> &'static str {
        match self {
            CompareMode::Before => "cmp-before",
            CompareMode::Local => "cmp-local",
            CompareMode::BeforeLocal => "cmp-before-local",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum FinderMode {
    Files,
    Actions,
    /// New Scratch File language picker (results index into `scratch::LANGS`).
    Scratch,
    /// Find in Files: full-text content search (results in `content_results`).
    Content,
}

/// One hit in the Find-in-Files content search.
#[derive(Clone)]
pub(crate) struct ContentHit {
    pub path: PathBuf,
    pub line: u32,
    pub text: String,
}

/// One entry in the Cmd+Shift+A actions palette.
#[derive(Clone, Copy)]
enum PaletteAction {
    GoToFile,
    FindInFiles,
    NewScratch,
    CommitView,
    BrowseView,
    SelectInTree,
    Rollback,
    Settings,
    RevealInFinder,
    RevealInTerminal,
    Plugins,
    Fonts,
}

/// Action-finder entries: (label, action, keymap-action name for the shortcut
/// chip — `""` when the action has no bindable key and so shows none).
const PALETTE: &[(&str, PaletteAction, &str)] = &[
    ("Select in File View", PaletteAction::SelectInTree, ""),
    ("Reveal in Finder", PaletteAction::RevealInFinder, ""),
    ("Reveal in Terminal", PaletteAction::RevealInTerminal, ""),
    ("Go to File", PaletteAction::GoToFile, "go_to_file"),
    ("Find in Files", PaletteAction::FindInFiles, "find_in_files"),
    ("New Scratch File", PaletteAction::NewScratch, "new_scratch"),
    (
        "Commit / Git view",
        PaletteAction::CommitView,
        "mode_commit",
    ),
    (
        "Browse / Code view",
        PaletteAction::BrowseView,
        "mode_browse",
    ),
    ("Rollback changes", PaletteAction::Rollback, ""),
    ("Settings / Keymap", PaletteAction::Settings, "open_keymap"),
    ("Manage Plugins", PaletteAction::Plugins, ""),
    ("Preview Fonts", PaletteAction::Fonts, ""),
];

/// What a right-click context menu was opened on.
#[derive(Clone)]
enum MenuTarget {
    /// A path in the Browse tree (`bool` = is_dir), or the open editor file.
    BrowseFile(PathBuf, bool),
    /// Right-click inside the editor pane — git commands only (no file ops).
    EditorGit(PathBuf),
    /// A path (file or folder) in the Commit tree — `bool` = is_dir.
    CommitPath(PathBuf, bool),
    /// A changed file in the Rollback modal, by index into `files` (→ View Diff).
    RollbackFile(usize),
    /// A file in the Push modal, by index into `push_files` (→ View Diff).
    PushFile(usize),
    /// An open editor tab, by index into `open_tabs`.
    Tab(usize),
    /// The tab-bar overflow chooser (`▾`): a flat list of every open tab to jump to.
    TabList,
    /// A commit row in the History list (by index into `history_commits`) — its menu offers
    /// the same compare modes as the header dropdown.
    HistoryCompare(usize),
}

struct ContextMenu {
    at: Point<Pixels>,
    target: MenuTarget,
}

/// Snapshot of a scroll view's `(pane_w, vp_w, vp_h, max_w, max_h)` used to debounce the
/// one-frame scrollbar reframe (see `Kyde::with_scrollbars`).
pub(crate) type ScrollDims = (Pixels, Pixels, Pixels, Pixels, Pixels);

/// An in-progress scrollbar-thumb drag. Carries the `ScrollHandle` so the shared scrollbar
/// helper drives whichever view is being dragged (editor, tree, …).
#[derive(Clone)]
pub(crate) struct SbDrag {
    pub handle: ScrollHandle,
    pub horizontal: bool,
    pub start_cursor: f32,
    pub start_off: f32,
}

/// Which scrollable view a `with_scrollbars` call is for — keys its reframe-dims slot.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SbView {
    Editor,
    Tree,
    MdEditor,
    MdPreview,
    Diff,
    /// The diff's single shared horizontal scrollbar.
    DiffLeftH,
}

/// An open "name this file" prompt (the small modal with a text input).
#[derive(Clone)]
enum NamePrompt {
    /// Create a new file inside this directory (rel path; `""` = repo root).
    NewFile(PathBuf),
    /// Rename this existing file (rel path) to the typed name in its own folder.
    Rename(PathBuf),
}

/// A minimal text tooltip view (gpui 0.2.2 ships no ready-made one). Built on
/// demand by `.tooltip(..)`; styled to match the chrome.
struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::get();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(t.bg_light)
            .border_1()
            .border_color(t.divider)
            .text_color(t.text)
            .font_family(theme::font::UI_FAMILY)
            .text_size(px(theme::get().ui_font_size))
            .shadow_lg()
            .child(self.0.clone())
    }
}

/// Per-project UI state stashed when switching to another open-project tab, so switching back
/// restores the project exactly as you left it (which file is open, the editor tabs, the tree
/// expansion, the active mode). The *active* project's state lives in the `Kyde` fields
/// directly; this only holds the inactive tabs' snapshots.
struct ProjectSession {
    mode: Mode,
    open_path: Option<PathBuf>,
    open_tabs: Vec<PathBuf>,
    preview_tab: Option<PathBuf>,
    selected: Option<usize>,
    expanded: std::collections::HashSet<PathBuf>,
}

struct Kyde {
    /// None = no project open → the Projects landing view.
    repo_root: Option<PathBuf>,
    /// Roots of every open project, in tab order. The active one == `repo_root`. Project tabs
    /// render only when this holds more than one. Empty ⇔ `repo_root` is None (landing view).
    open_projects: Vec<PathBuf>,
    /// Saved per-project UI state for the *inactive* tabs (keyed by root). See `ProjectSession`.
    project_sessions: std::collections::HashMap<PathBuf, ProjectSession>,
    mode: Mode,
    focus_handle: FocusHandle,
    keymap: Keymap,
    plugins: Plugins,
    /// Packs the user dismissed via "Ignore extension" (session-only, suppresses the banner).
    ignored_packs: std::collections::HashSet<&'static str>,

    // Projects landing
    recents: Recents,
    project_search: Entity<CodeEditor>,

    // Commit mode
    files: Vec<ChangedFile>,
    selected: Option<usize>,
    /// Changed files highlighted as a group in the commit list (e.g. after a folder
    /// "Commit" picks every change under it). Cleared on a plain single-file click.
    commit_focus: std::collections::HashSet<PathBuf>,
    /// Commit view: changed files as a folder tree + which are checked-to-commit.
    commit_tree: tree::Tree,
    commit_expanded: std::collections::HashSet<PathBuf>,
    commit_checked: std::collections::HashSet<PathBuf>,
    current_diff: Option<FileDiff>,
    /// Syntax spans for the selected file's before/after content (cached on select,
    /// so the diff doesn't re-parse the whole file every render). Empty when the
    /// file's language pack isn't installed.
    old_spans: Vec<highlight::Span>,
    new_spans: Vec<highlight::Span>,
    commit_editor: Entity<CodeEditor>,
    /// Set by `enter_commit`; `render_commit` consumes it to focus the commit-message input
    /// on the next frame (deferred so the editor element is in the tree first), so opening the
    /// Commit view drops the caret straight into the message box.
    focus_commit_msg: bool,
    /// Side-by-side diff editors: left = base (read-only), right = working (editable,
    /// live-saves). `diff_path` is the file they're showing.
    diff_left: Entity<CodeEditor>,
    diff_right: Entity<CodeEditor>,
    diff_path: Option<PathBuf>,
    /// Selected file is an image → commit view previews it (like Browse) instead of running
    /// it through the text diff. Kept separate from `diff_path` (which stays `None` for binary
    /// files) so the diff autosave never fires and truncates the image to empty.
    diff_image: Option<PathBuf>,
    /// Read-only diff (push view: committed `@{u}` vs `HEAD`). Suppresses the gutter
    /// revert chevrons + autosave — there's no working-tree change to stage/revert.
    diff_readonly: bool,
    /// Base (HEAD/index) text of the diffed file, kept so we can re-diff live as the
    /// right (working) pane is edited without re-reading git each keystroke.
    diff_base: String,
    /// Shared 2D scroll for BOTH diff panes (single element each → gpui axis-locks the wheel,
    /// so a vertical gesture doesn't drift horizontally; both panes track it → aligned in both
    /// axes, horizontal scroll shared across the side-by-side).
    diff_scroll: ScrollHandle,
    /// Left pane's fraction of the diff island width (the draggable center divider sets it).
    diff_split: f32,
    /// The single in-flight divider drag, if any: which divider + the grab offset captured at
    /// mouse-down (cursor coord minus the divider's position). One field for every divider —
    /// see `Divider` and `Kyde::drag_divider`.
    divider_drag: Option<(Divider, f32)>,
    /// Scroll handle for the Browse editor pane — drives the hover scrollbars.
    file_scroll: ScrollHandle,
    /// Active scrollbar-thumb drag (which scroll handle, axis, grab cursor, grab offset).
    /// Carries the `ScrollHandle` so the shared scrollbar works on any scrollable view.
    sb_drag: Option<SbDrag>,
    /// Per-view snapshot of the dims the scrollbars were last drawn with, so a layout change
    /// schedules exactly one follow-up frame (scroll metrics are only known *after* a paint, so
    /// the first frame after open/resize is stale). Keyed by view; converges, no redraw loop.
    scroll_dims: std::collections::HashMap<SbView, ScrollDims>,
    /// Vertical scroll handle for the markdown split's code (left) pane.
    md_editor_scroll: ScrollHandle,
    /// Vertical scroll handle for the markdown split's rendered preview (right) pane.
    md_preview_scroll: ScrollHandle,
    /// Persistent selectable rendered-markdown view (holds the preview's text selection).
    md_view: Option<gpui::Entity<mdview::MarkdownView>>,
    /// One-shot: has the Projects search box been auto-focused since the landing appeared?
    /// Reset while a project is open, so returning to the landing re-focuses search.
    projects_search_focused: bool,
    /// Editor pane width (px) of the markdown side-by-side split (drag-resizable).
    md_editor_w: f32,

    // Browse mode
    all_files: Vec<PathBuf>,
    file_tree: tree::Tree,
    /// Directories currently expanded in the Browse tree.
    expanded: std::collections::HashSet<PathBuf>,
    /// Width of the Browse file-tree pane, drag-resizable via the divider.
    tree_width: f32,
    /// True when the file tree is minimized to a thin strip (the `−` button).
    tree_collapsed: bool,
    /// True when the commit view's changed-files panel is minimized to a thin strip (its `−`
    /// button), giving the side-by-side diff the full width. Independent of `tree_collapsed`.
    commit_collapsed: bool,
    open_path: Option<PathBuf>,
    /// Open editor tabs, left→right in open order. `open_path` = the active one.
    open_tabs: Vec<PathBuf>,
    /// VS Code-style *preview* tab: at most one tab is temporary, shown in italics. A
    /// single-click in the tree opens here, reusing this same slot for the next single-click;
    /// a double-click (or editing the file) promotes it to a permanent tab (`= None`).
    preview_tab: Option<PathBuf>,
    /// Project-scoped scratch files (absolute paths, outside the repo), shown in the tree.
    scratches: Vec<PathBuf>,
    /// Scroll position of the (horizontally scrollable) tab strip, so opening a
    /// tab that's off-screen can scroll it into view.
    tab_scroll: ScrollHandle,
    /// Highlighted row in the Browse tree (file OR folder); drives the breadcrumb.
    /// Distinct from `open_path` so selecting a folder doesn't change the editor.
    selected_path: Option<PathBuf>,
    /// Scroll position of the Browse tree, so "Select Opened File in Tree" can
    /// scroll an off-screen row into view.
    tree_scroll: ScrollHandle,
    file_editor: Entity<CodeEditor>,

    // In-editor find / replace bar. Targets whichever editor was focused when it opened
    // (`find_target`) — the Browse file editor, or a diff pane in the Show-Diff view.
    find_open: bool,
    find_replace: bool,
    find_target: FindTarget,
    find_query: Entity<CodeEditor>,
    replace_query: Entity<CodeEditor>,
    find_matches: Vec<std::ops::Range<usize>>,
    find_idx: usize,
    /// Bumped on every diff-pane edit; the debounced autosave only fires when its captured
    /// generation still matches (so we don't spawn `git status` + re-diff per keystroke).
    diff_edit_gen: u64,
    /// Bumped on every Find-in-Files keystroke; the debounced background `git grep` only
    /// applies its results when its captured generation still matches (drops stale searches).
    finder_gen: u64,
    /// FPS monitor (toggled from the Kyde menu): smoothed frames-per-second + last frame time.
    show_fps: bool,
    fps_value: f32,
    /// Throttled snapshot of `fps_value` — the value the overlay displays, held steady for a
    /// readable beat rather than re-rendering a blurred number every frame.
    fps_shown: f32,
    fps_last: Option<std::time::Instant>,
    /// Throttle timer for the `fps_shown` snapshot (~5/sec).
    fps_file_last: Option<std::time::Instant>,

    // Overlays
    finder_open: bool,
    /// Files (Go to File) vs Actions (the Cmd+Shift+A palette) — same overlay, two modes.
    finder_mode: FinderMode,
    finder_query: Entity<CodeEditor>,
    /// Commit-view changed-files filter (single-line search above the file list).
    commit_search: Entity<CodeEditor>,
    finder_results: Vec<PathBuf>,
    /// Content-search hits (used when `finder_mode == Content`).
    content_results: Vec<ContentHit>,
    /// Matching palette-action indices (used when `finder_mode == Actions`).
    action_results: Vec<usize>,
    finder_selected: usize,
    onboarding_open: bool,
    /// True until the user has picked a keymap — the picker can't be dismissed while set.
    onboarding_forced: bool,
    /// Language-pack manager: a native modal window (like Rollback/Push), + its search box.
    plugins_win: Option<gpui::WindowHandle<ModalWindow>>,
    plugins_query: Entity<CodeEditor>,
    /// Font specimen modal window: the bundled families at each weight, large preview lines.
    fonts_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// "Clear Data & Restart" confirmation — a native modal window (native-menu action).
    clear_data_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// Settings — a native modal window with a category sidebar (Appearance/Keymap/…).
    settings_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// Which Settings category the sidebar has selected.
    settings_section: SettingsSection,
    /// Cached `(path, registered family name)` for the open font file's preview.
    font_preview: Option<(PathBuf, SharedString)>,
    /// Frame counter driving the welcome-screen ASCII shimmer (bumped each animation frame).
    welcome_frame: u32,
    /// The preset currently highlighted in the picker (confirmed via Continue).
    onboarding_choice: Preset,
    /// Pending state of the "Install shell command" checkbox in the picker;
    /// applied (symlink created) when the user confirms with Continue.
    onboarding_install_cmd: bool,
    /// Last shell-command install error, shown under the checkbox.
    shell_cmd_error: Option<String>,
    /// Contents of `crash.log` if the previous run crashed — drives the report banner.
    pending_crash: Option<String>,
    /// Last failed git operation (commit/push/rollback/branch/checkout/status), surfaced
    /// in a dismissible banner so a silent failure never looks like success. Cleared on
    /// the next successful `refresh`. `None` = no outstanding error.
    op_error: Option<String>,
    /// Open right-click context menu, if any.
    context_menu: Option<ContextMenu>,
    /// Show-Diff viewer — its own native OS window (`None` when closed).
    diff_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// True while the Show-Diff modal window is open. The modal renders the shared
    /// `diff_left`/`diff_right` editors, so the inline diff is suppressed meanwhile (rendering
    /// one editor in two windows desyncs scroll). Set on open, cleared on the window's release.
    diff_modal_open: bool,
    /// The main window's bounds, refreshed each frame in `impl Render for Kyde`.
    main_window_bounds: Option<gpui::WindowBounds>,
    /// True when the main window is showing the full-screen Show-Diff view (rollback/push diff).
    /// It replaces the normal mode content and reuses the inline `render_diff`, so it inherits
    /// all editor functionality (find, divider drag, scroll). Escape / the Back button exits.
    diff_view_open: bool,
    /// Rollback — its own native OS window (real titlebar + traffic lights). `None` closed.
    rollback_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// "Create New Branch" dialog — its own native window. The name is typed into
    /// `branch_query` (reused), with these toggles.
    new_branch_win: Option<gpui::WindowHandle<ModalWindow>>,
    new_branch_checkout: bool,
    new_branch_overwrite: bool,
    rollback_checked: std::collections::HashSet<PathBuf>,
    rollback_delete_added: bool,
    /// Delete-confirmation modal: the (path, is_dir) pending deletion.
    delete_target: Option<(PathBuf, bool)>,
    /// New-file / rename modal state + its single-line name input.
    name_prompt: Option<NamePrompt>,
    name_input: Entity<CodeEditor>,

    // Branch switcher (bottom-right status bar + popup)
    current_branch: Option<String>,
    branch_list: Vec<String>,
    /// Remote-only branches (short name, e.g. "feature-x") that have no local head yet —
    /// shown under a "Remote" section so freshly-fetched branches are checkout-able.
    branch_remotes: Vec<String>,
    branch_popup_open: bool,
    branch_query: Entity<CodeEditor>,
    /// Expanded nodes in the branch tree (section keys like "sec:recent" and folder
    /// keys like "sec:local/feat").
    branch_expanded: std::collections::HashSet<String>,
    /// Bumped on every edit; a debounced task only refreshes git status once this
    /// stops changing (so typing stays snappy but status/tab colors catch up).
    refresh_gen: u64,
    /// Commits the current branch is ahead of its upstream (drives the push badge).
    ahead: Option<usize>,
    /// Commits the upstream is ahead of us, last-fetch fresh (drives the pull badge).
    behind: Option<usize>,
    /// True while a `git push` is in flight (disables the button, shows "Pushing…").
    pushing: bool,
    /// True while a `git commit` is in flight (disables the button, shows "Committing…").
    committing: bool,
    /// True while a `git pull` (fetch + rebase) is in flight (shows "Pulling…").
    pulling: bool,
    /// True while a `git fetch` is in flight (shows "Fetching…").
    fetching: bool,
    /// Last push error, surfaced as the push button's tooltip.
    push_msg: Option<String>,
    /// Push confirmation — its own native OS window (`None` closed).
    push_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// Files a push would send (`push_base()..HEAD`), shown like the commit/rollback list.
    push_files: Vec<ChangedFile>,
    /// Base revision those files are diffed against (`@{u}` or the empty tree).
    push_base: String,
    /// Which tab the git (Commit) view is showing — staging changes or pushing commits.
    git_tab: GitTab,
    /// Selected file index in the Push tab's list (drives its read-only diff).
    push_selected: Option<usize>,

    // Self-update
    /// A newer release found on GitHub (drives the update banner); `None` = up to date / unknown.
    update_available: Option<update::Release>,
    /// True while a download-swap is in flight (disables the button, shows progress).
    updating: bool,

    // History (git log) view
    /// Revision being logged — a branch name, or "HEAD" for the current branch.
    history_rev: String,
    /// Path the log is scoped to (a folder/file), or `None` for the whole repo. Set when
    /// the history view is opened from a Browse-tree folder's right-click menu.
    history_path: Option<PathBuf>,
    /// Commits shown in the log list (newest first).
    history_commits: Vec<git::Commit>,
    /// Selected commit index into `history_commits`.
    history_selected: Option<usize>,
    /// Files changed by the selected commit under the current compare mode.
    history_files: Vec<ChangedFile>,
    /// Selected file index into `history_files`.
    history_file_selected: Option<usize>,
    /// Folder tree of `history_files` (right pane of the history panel).
    history_files_tree: tree::Tree,
    /// Expanded dirs in the history files tree.
    history_files_expanded: std::collections::HashSet<PathBuf>,
    /// Search box filtering the history files tree.
    history_files_query: Entity<CodeEditor>,
    /// Height (px) of the history bottom panel (drag the top edge to resize).
    history_panel_h: f32,
    /// When true the history bottom panel is minimised to just its toolbar (the header
    /// chevron toggles it), giving the diff the full height.
    history_panel_collapsed: bool,
    /// What the selected commit is diffed against.
    history_compare: CompareMode,
    /// Compare-mode dropdown open in the history view.
    history_compare_open: bool,
    /// Branch-picker dropdown open in the history view.
    history_branch_open: bool,
    /// Local branches for the history branch picker (loaded when the dropdown opens).
    history_locals: Vec<String>,
    /// Remote-tracking branches for the history branch picker.
    history_remotes: Vec<String>,
    /// Search box filtering the history branch dropdown.
    history_branch_query: Entity<CodeEditor>,
    /// Search box filtering the commit list (subject / author / hash).
    history_commit_query: Entity<CodeEditor>,
    /// Scroll position of the commit list.
    history_scroll: ScrollHandle,
    /// Fraction (0..1) of the history panel width given to the commit-list pane on the left
    /// (resizable); the changed-files pane fills the rest on the right. Defaults to 2/3.
    history_commit_frac: f32,

    // Bottom terminal panel (gated behind the `terminal` Cargo feature).
    /// One `TerminalView` entity per tab, left→right in open order.
    #[cfg(feature = "terminal")]
    term_tabs: Vec<Entity<terminal::TerminalView>>,
    /// Active terminal tab index into `term_tabs`.
    #[cfg(feature = "terminal")]
    term_active: usize,
    /// Whether the bottom terminal panel is visible.
    #[cfg(feature = "terminal")]
    term_open: bool,
    /// Height (px) of the terminal panel, drag-resizable via its top divider.
    #[cfg(feature = "terminal")]
    term_height: f32,
    /// When true the terminal fills the whole right column (tree + editor hidden).
    #[cfg(feature = "terminal")]
    term_maximized: bool,
    /// Set when a tab closes (e.g. ^D) so the next paint re-focuses the new active tab —
    /// the close happens in a window-less subscription, so focus is deferred to `render`.
    #[cfg(feature = "terminal")]
    term_focus_pending: bool,
}

/// Which native modal a `ModalWindow` is showing. Each delegates its body back into `Kyde`
/// (the data + actions live there); the window is just a host with a native titlebar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ModalKind {
    Rollback,
    Push,
    Diff,
    NewBranch,
    Plugins,
    Fonts,
    ClearData,
    Settings,
}

/// Which category the Settings window's sidebar has selected. Drives `render_settings_body`'s
/// content pane.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsSection {
    Appearance,
    Keymap,
    LanguagePacks,
}

impl SettingsSection {
    /// Sidebar order + labels.
    pub(crate) const ALL: &'static [(SettingsSection, &'static str)] = &[
        (SettingsSection::Appearance, "Appearance"),
        (SettingsSection::Keymap, "Keymap"),
        (SettingsSection::LanguagePacks, "Language Packs"),
    ];
}

/// A separate native OS window hosting one of Kyde's modals (Rollback / Push / Diff). It holds
/// the `Kyde` entity, observes it (so checkbox/refresh changes repaint), and builds its body
/// by delegating into `Kyde` — `kyde.update(..)` is safe here because the window is opened
/// from a spawned task (never during a `Kyde` update), so there's no re-entrant lease.
struct ModalWindow {
    kyde: Entity<Kyde>,
    kind: ModalKind,
    focus: FocusHandle,
}

impl ModalWindow {
    fn new(kyde: Entity<Kyde>, kind: ModalKind, cx: &mut Context<Self>) -> Self {
        cx.observe(&kyde, |_, _, cx| cx.notify()).detach();
        // The Diff modal renders the SAME `diff_left`/`diff_right` editors as the inline diff;
        // rendering one editor entity in two windows desyncs scroll + garbles layout. So while
        // this window lives, the main view suppresses its inline diff (`diff_modal_open`). Clear
        // the flag when the window closes (manual or programmatic) so the inline diff returns.
        if kind == ModalKind::Diff {
            let kyde = kyde.clone();
            cx.on_release(move |_, cx| {
                kyde.update(cx, |k, kcx| {
                    k.diff_modal_open = false;
                    kcx.notify();
                });
            })
            .detach();
        }
        Self {
            kyde,
            kind,
            focus: cx.focus_handle(),
        }
    }
}

impl Focusable for ModalWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ModalWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kyde = self.kyde.clone();
        let kind = self.kind;
        let body = kyde.update(cx, |k, kcx| match kind {
            ModalKind::Rollback => k.render_rollback_body(kcx),
            ModalKind::Push => k.render_push_body(kcx),
            ModalKind::Diff => k.render_diff_body(kcx),
            ModalKind::NewBranch => k.render_new_branch_body(kcx),
            ModalKind::Plugins => k.render_plugins_body(kcx),
            ModalKind::Fonts => k.render_fonts_body(kcx),
            ModalKind::ClearData => k.render_clear_data_body(kcx),
            ModalKind::Settings => k.render_settings_body(kcx),
        });
        div()
            .track_focus(&self.focus)
            .key_context("Modal")
            .size_full()
            .bg(theme::get().panel_bg)
            .text_color(theme::get().text)
            .font_family(theme::font::UI_FAMILY)
            .text_size(px(theme::get().ui_font_size))
            // Escape closes the window; Enter on the New Branch dialog confirms.
            .on_key_down(
                cx.listener(move |this, ev: &gpui::KeyDownEvent, window, cx| {
                    match ev.keystroke.key.as_str() {
                        "escape" => window.remove_window(),
                        "enter" if kind == ModalKind::NewBranch => {
                            this.kyde.update(cx, |k, kcx| k.do_create_branch(kcx));
                        }
                        _ => {}
                    }
                }),
            )
            .child(body)
    }
}

/// Make a user-typed branch name git-safe (`git check-ref-format` rules), so typing a commit
/// subject like `fix: thing here` yields the valid `fix-thing-here` instead of being rejected.
/// Whitespace and every character git forbids in a ref (`~ ^ : ? * [ \` + control chars)
/// become single hyphens (runs collapsed); the `@{` and `..` sequences are removed; and
/// leading/trailing `/ . -` plus a trailing `.lock` are trimmed. Internal `/` is preserved so
/// namespaced names (`feat/x`) still work.
fn slugify_branch(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.trim().chars() {
        let forbidden = ch.is_whitespace()
            || matches!(ch, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
            || (ch as u32) < 0x20
            || ch == '\x7f';
        if forbidden {
            // Collapse any run of forbidden chars to one hyphen.
            if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        } else {
            out.push(ch);
            prev_dash = false;
        }
    }
    // git also forbids the `@{` and `..` sequences anywhere in a ref, and `//`.
    out = out.replace("@{", "-");
    while out.contains("..") {
        out = out.replace("..", ".");
    }
    while out.contains("//") {
        out = out.replace("//", "/");
    }
    // No leading/trailing `/ . -`; strip a trailing `.lock` (also disallowed).
    let mut s = out.trim_matches(['/', '.', '-']).to_string();
    if let Some(stripped) = s.strip_suffix(".lock") {
        s = stripped.trim_end_matches(['/', '.', '-']).to_string();
    }
    s
}

fn status_color(s: FileStatus) -> gpui::Rgba {
    match s {
        FileStatus::Added => theme::get().status_added,
        FileStatus::Modified | FileStatus::Renamed => theme::get().status_modified,
        FileStatus::Deleted => theme::get().status_deleted,
        FileStatus::Untracked => theme::get().status_untracked,
        FileStatus::Conflict => theme::get().status_conflict,
    }
}

impl Focusable for Kyde {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// A flattened row of the branch tree.
struct BranchRow {
    label: String,
    depth: usize,
    node: BranchNode,
}
enum BranchNode {
    /// A section root ("Recent"/"Local") or a `/`-segment folder.
    Folder {
        key: String,
        expanded: bool,
        section: bool,
    },
    /// A checkout-able branch; `full` is the complete ref name.
    Leaf { full: String },
}

/// Build the branch tree: "Recent" + "Local" sections as roots, with `/` in branch
/// names forming nested folders. `force_open` (search active) reveals everything.
fn branch_rows(
    recent: &[String],
    all: &[String],
    remotes: &[String],
    expanded: &std::collections::HashSet<String>,
    force_open: bool,
) -> Vec<BranchRow> {
    let mut rows = Vec::new();
    for (label, key, list) in [
        ("Recent", "sec:recent", recent),
        ("Local", "sec:local", all),
        ("Remote", "sec:remote", remotes),
    ] {
        if list.is_empty() {
            continue;
        }
        let open = force_open || expanded.contains(key);
        rows.push(BranchRow {
            label: label.into(),
            depth: 0,
            node: BranchNode::Folder {
                key: key.into(),
                expanded: open,
                section: true,
            },
        });
        if open {
            let items: Vec<(String, String)> =
                list.iter().map(|b| (b.clone(), b.clone())).collect();
            emit_branch_level(&mut rows, items, key, 1, expanded, force_open);
        }
    }
    rows
}

/// Recursively emit one level: `items` are (remaining-suffix, full-name) pairs.
fn emit_branch_level(
    rows: &mut Vec<BranchRow>,
    items: Vec<(String, String)>,
    key_prefix: &str,
    depth: usize,
    expanded: &std::collections::HashSet<String>,
    force_open: bool,
) {
    use std::collections::BTreeMap;
    let mut folders: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut leaves: Vec<(String, String)> = Vec::new();
    for (suffix, full) in items {
        match suffix.split_once('/') {
            Some((head, rest)) => folders
                .entry(head.to_string())
                .or_default()
                .push((rest.to_string(), full)),
            None => leaves.push((suffix, full)),
        }
    }
    // Folders first (sorted by BTreeMap), then leaves.
    for (seg, kids) in folders {
        let key = format!("{key_prefix}/{seg}");
        let open = force_open || expanded.contains(&key);
        rows.push(BranchRow {
            label: seg,
            depth,
            node: BranchNode::Folder {
                key: key.clone(),
                expanded: open,
                section: false,
            },
        });
        if open {
            emit_branch_level(rows, kids, &key, depth + 1, expanded, force_open);
        }
    }
    leaves.sort_by_key(|a| a.0.to_lowercase());
    for (suffix, full) in leaves {
        rows.push(BranchRow {
            label: suffix,
            depth,
            node: BranchNode::Leaf { full },
        });
    }
}

/// File-type badge for the Browse tree (approximates IntelliJ's icons). Known types get a
/// colored monogram; everything else gets the generic lines/document icon.
/// Raster image types we preview inline (rendered with `img()` instead of the
/// text editor). SVG stays a text/icon file — it has its own vector path.
fn is_image(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" | "tiff" | "tif"
    )
}

/// Font files preview in their own typeface (gated by the "font" plugin) rather than loading
/// binary bytes into the text editor.
fn is_font_file(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "ttf" | "otf" | "ttc" | "otc"
    )
}

/// Extract a font's display family name from its bytes (Typographic Family, id 16, preferred;
/// else Family, id 1) so the preview can register + render it. `None` if it won't parse.
fn font_family_name(bytes: &[u8]) -> Option<String> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    let mut fallback = None;
    for name in face.names() {
        match name.name_id {
            16 => {
                if let Some(s) = name.to_string() {
                    return Some(s);
                }
            }
            1 if fallback.is_none() => fallback = name.to_string(),
            _ => {}
        }
    }
    fallback
}

/// Sentinel path for the virtual "Scratches" tree folder. The leading control char keeps
/// it from ever matching a real file path (used only for tree grouping + expand state).
fn scratch_group_path() -> PathBuf {
    PathBuf::from("\u{1}Scratches")
}

/// The Kyde config directory (`~/.config/kyde`, XDG-respecting). Holds every persisted
/// file: plugins.json, keymap.json, theme.json, projects.json, ui.json. Removing it is the
/// full "clear data" reset (uninstalls all plugins + drops all cached settings).
fn config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config")
        });
    base.join("kyde")
}

/// Tiny persisted UI prefs (`~/.config/kyde/ui.json`), e.g. the FPS-monitor toggle.
fn ui_settings_path() -> PathBuf {
    config_dir().join("ui.json")
}
/// Read one boolean key from `ui.json` (missing file/key → `default`).
fn load_ui_bool(key: &str, default: bool) -> bool {
    std::fs::read_to_string(ui_settings_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get(key).and_then(|b| b.as_bool()))
        .unwrap_or(default)
}
/// Set one boolean key in `ui.json`, preserving the file's other keys (read-modify-write so
/// e.g. saving the terminal pref never clobbers `show_fps`).
fn save_ui_bool(key: &str, val: bool) {
    let p = ui_settings_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut v = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert(key.to_string(), serde_json::Value::Bool(val));
    }
    let _ = std::fs::write(&p, v.to_string());
}
fn load_show_fps() -> bool {
    load_ui_bool("show_fps", false)
}
fn save_show_fps(v: bool) {
    save_ui_bool("show_fps", v);
}

/// One visual row of the aligned side-by-side diff. `old`/`new` index into each
/// side's lines (`None` = filler/blank). `hunk` tags rows belonging to a change;
/// `hunk_start` marks the first such row (where the gutter controls render).
struct DiffRow {
    old: Option<usize>,
    new: Option<usize>,
    hunk: Option<usize>,
    hunk_start: bool,
}

/// Flatten a `FileDiff` into aligned rows. Equal regions advance both sides
/// together; each hunk pairs its old/new lines and pads the shorter side so the
/// two panes stay vertically in sync (and the center gutter lines up).
fn aligned_rows(d: &FileDiff) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let (mut o, mut n) = (0usize, 0usize);
    for (hi, h) in d.hunks.iter().enumerate() {
        while o < h.old_range.start && n < h.new_range.start {
            rows.push(DiffRow {
                old: Some(o),
                new: Some(n),
                hunk: None,
                hunk_start: false,
            });
            o += 1;
            n += 1;
        }
        let (ol, nl) = (h.old_range.len(), h.new_range.len());
        for i in 0..ol.max(nl) {
            rows.push(DiffRow {
                old: (i < ol).then(|| h.old_range.start + i),
                new: (i < nl).then(|| h.new_range.start + i),
                hunk: Some(hi),
                hunk_start: i == 0,
            });
        }
        o = h.old_range.end;
        n = h.new_range.end;
    }
    while o < d.old.len() && n < d.new.len() {
        rows.push(DiffRow {
            old: Some(o),
            new: Some(n),
            hunk: None,
            hunk_start: false,
        });
        o += 1;
        n += 1;
    }
    rows
}

/// Filler (blank alignment rows) for the two diff panes. `(left_map, left_end, right_map,
/// right_end)` — `map[b] = N` blank rows before buffer line `b`; `end` = trailing blanks.
/// Computed from the aligned rows so both panes end up the same number of display rows.
fn diff_fillers(
    d: &FileDiff,
) -> (
    std::collections::HashMap<usize, usize>,
    usize,
    std::collections::HashMap<usize, usize>,
    usize,
) {
    let (mut left, mut right) = (
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    );
    let (mut lblank, mut rblank) = (0usize, 0usize);
    for r in aligned_rows(d) {
        match r.old {
            Some(o) => {
                if lblank > 0 {
                    left.insert(o, lblank);
                    lblank = 0;
                }
            }
            None => lblank += 1,
        }
        match r.new {
            Some(n) => {
                if rblank > 0 {
                    right.insert(n, rblank);
                    rblank = 0;
                }
            }
            None => rblank += 1,
        }
    }
    (left, lblank, right, rblank)
}

/// Per-line hunk backgrounds for the two diff panes: `(old_side, new_side)`,
/// keyed by buffer line index.
fn diff_line_bgs(
    d: &FileDiff,
) -> (
    std::collections::HashMap<usize, gpui::Rgba>,
    std::collections::HashMap<usize, gpui::Rgba>,
) {
    let t = theme::get();
    let (mut old, mut new) = (
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    );
    for h in &d.hunks {
        match h.kind {
            HunkKind::Deleted => {
                for l in h.old_range.clone() {
                    old.insert(l, t.diff_deleted_bg);
                }
            }
            HunkKind::Added => {
                for l in h.new_range.clone() {
                    new.insert(l, t.diff_inserted_bg);
                }
            }
            HunkKind::Modified => {
                for l in h.old_range.clone() {
                    old.insert(l, t.diff_modified_bg);
                }
                for l in h.new_range.clone() {
                    new.insert(l, t.diff_modified_bg);
                }
            }
        }
    }
    (old, new)
}

/// Buffer line index → byte ranges within that line that changed (one diff side).
type LineWordBgs = std::collections::HashMap<usize, Vec<std::ops::Range<usize>>>;

/// Per-line word-level highlight ranges for the two diff panes: `(old_side, new_side)`,
/// keyed by buffer line index → byte ranges within that line that actually changed.
fn diff_word_bgs(d: &FileDiff) -> (LineWordBgs, LineWordBgs) {
    let (mut old, mut new) = (
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
    );
    for h in &d.hunks {
        for (line, range) in &h.old_word_ranges {
            old.entry(*line)
                .or_insert_with(Vec::new)
                .push(range.clone());
        }
        for (line, range) in &h.new_word_ranges {
            new.entry(*line)
                .or_insert_with(Vec::new)
                .push(range.clone());
        }
    }
    (old, new)
}

/// Turn "cmd-shift-o" into "⌘⇧O" for display.
fn pretty_key(k: &str) -> String {
    k.split('-')
        .map(|part| match part {
            "cmd" => "⌘".to_string(),
            "shift" => "⇧".to_string(),
            "ctrl" => "⌃".to_string(),
            "alt" | "opt" => "⌥".to_string(),
            "enter" => "⏎".to_string(),
            other => other.to_uppercase(),
        })
        .collect()
}

/// Embedded asset source for `svg()` icons (Lucide, MIT). Paths are relative, e.g.
/// `"icons/folder.svg"`.
struct Assets;

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            "icons/folder.svg" => include_bytes!("../assets/icons/folder.svg"),
            "icons/git-branch.svg" => include_bytes!("../assets/icons/git-branch.svg"),
            "icons/history.svg" => include_bytes!("../assets/icons/history.svg"),
            #[cfg(feature = "terminal")]
            "icons/terminal.svg" => include_bytes!("../assets/icons/terminal.svg"),
            "icons/file-lines.svg" => include_bytes!("../assets/icons/file-lines.svg"),
            "icons/image.svg" => include_bytes!("../assets/icons/image.svg"),
            "icons/ban.svg" => include_bytes!("../assets/icons/ban.svg"),
            "icons/check.svg" => include_bytes!("../assets/icons/check.svg"),
            "icons/search.svg" => include_bytes!("../assets/icons/search.svg"),
            "icons/chevron-down.svg" => include_bytes!("../assets/icons/chevron-down.svg"),
            "icons/chevrons-up.svg" => include_bytes!("../assets/icons/chevrons-up.svg"),
            // Context-menu action icons.
            "icons/git-commit.svg" => include_bytes!("../assets/icons/git-commit.svg"),
            "icons/rotate-ccw.svg" => include_bytes!("../assets/icons/rotate-ccw.svg"),
            "icons/arrow-down-to-line.svg" => {
                include_bytes!("../assets/icons/arrow-down-to-line.svg")
            }
            "icons/arrow-down.svg" => include_bytes!("../assets/icons/arrow-down.svg"),
            "icons/arrow-up.svg" => include_bytes!("../assets/icons/arrow-up.svg"),
            "icons/file-plus.svg" => include_bytes!("../assets/icons/file-plus.svg"),
            "icons/pencil.svg" => include_bytes!("../assets/icons/pencil.svg"),
            "icons/trash.svg" => include_bytes!("../assets/icons/trash.svg"),
            "icons/x.svg" => include_bytes!("../assets/icons/x.svg"),
            "icons/maximize-2.svg" => include_bytes!("../assets/icons/maximize-2.svg"),
            "icons/minimize-2.svg" => include_bytes!("../assets/icons/minimize-2.svg"),
            "logo.png" => include_bytes!("../assets/logo.png"),
            _ => return Ok(None),
        };
        Ok(Some(std::borrow::Cow::Borrowed(bytes)))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![
            "icons/folder.svg".into(),
            "icons/git-branch.svg".into(),
            "icons/file-lines.svg".into(),
        ])
    }
}

/// Register the bundled Inter (UI) + JetBrains Mono (code) faces so `font_family` resolves
/// to them instead of silently falling back to a system font. Both are OFL-licensed.
fn load_fonts(cx: &mut App) {
    let fonts: Vec<std::borrow::Cow<'static, [u8]>> = vec![
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Regular.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Medium.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-SemiBold.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Bold.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-SemiBold.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf")),
    ];
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        eprintln!("kyde: failed to load bundled fonts: {e}");
    }
}

/// Path of the crash log (`~/.config/kyde/crash.log`).
fn crash_log_path() -> Option<PathBuf> {
    keymap::Keymap::config_path()
        .parent()
        .map(|d| d.join("crash.log"))
}

/// Percent-encode a string for a URL query value (RFC 3986 unreserved kept).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Pre-filled "New issue" URL for the kyde repo from a crash-log entry.
fn crash_issue_url(crash: &str) -> String {
    let title = crash
        .lines()
        .find(|l| !l.trim().is_empty() && !l.starts_with("==="))
        .unwrap_or("Crash report")
        .chars()
        .take(120)
        .collect::<String>();
    let trimmed: String = crash.chars().take(5000).collect();
    let body = format!(
        "**Crash report** (kyde {}, {})\n\n```\n{}\n```",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        trimmed
    );
    format!(
        "https://github.com/kyle-ssg/Kyde/issues/new?title={}&body={}",
        url_encode(&format!("Crash: {title}")),
        url_encode(&body)
    )
}

/// Append panics to `~/.config/kyde/crash.log` (with location + backtrace) and stderr,
/// so a crash leaves a trace even when launched from Finder/`gs` without a terminal.
fn install_crash_logger() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".into());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic>".into());
        let entry = format!("\n=== panic at {loc} ===\n{msg}\n{bt}\n");
        let path = keymap::Keymap::config_path()
            .parent()
            .map(|d| d.join("crash.log"));
        if let Some(path) = path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = f.write_all(entry.as_bytes());
            }
        }
        eprintln!("{entry}");
        prev(info);
    }));
}

/// macOS names an unbundled app's dock tile / menu after the **executable path's basename**.
/// The binary is `kyde` (lowercase, run via the `ky` shell function), so the dock would read
/// "kyde". macOS filesystems are case-insensitive by default, so we re-exec ourselves once via
/// the same file under the path `…/Kyde` — same binary, but now the basename is "Kyde", so the
/// dock tile reads "Kyde". No change needed to the user's shell function. The re-exec'd process
/// is already `Kyde`, so it doesn't loop; on a case-sensitive volume (no `Kyde` file) the exec
/// fails and we just continue as `kyde`. No-op off macOS.
#[cfg(target_os = "macos")]
fn reexec_with_proper_name() {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if exe.file_name().and_then(|s| s.to_str()) == Some("Kyde") {
        return; // already running under the capitalised name
    }
    let kyde = exe.with_file_name("Kyde");
    if kyde.exists() {
        // exec() only returns on failure — fall through and run normally as `kyde`.
        let _ = std::process::Command::new(&kyde)
            .arg0("Kyde")
            .args(std::env::args_os().skip(1))
            .exec();
    }
}

#[cfg(not(target_os = "macos"))]
fn reexec_with_proper_name() {}

/// Name the running process "Kyde" so the macOS dock tile / menu-bar app menu read "Kyde"
/// instead of the lowercase executable name. Must run before NSApplication checks in with
/// LaunchServices, so it's called at the very top of `main`. No-op off macOS.
#[cfg(target_os = "macos")]
fn set_app_name() {
    use objc2_foundation::{NSProcessInfo, NSString};
    NSProcessInfo::processInfo().setProcessName(&NSString::from_str("Kyde"));
}

#[cfg(not(target_os = "macos"))]
fn set_app_name() {}

/// Set the macOS Dock icon from the bundled logo (no `.app` bundle needed). Runs on the
/// main thread during app launch; silently no-ops if the image can't be built.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(include_bytes!("../assets/logo.png"));
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    unsafe { app.setApplicationIconImage(Some(&image)) };
}

#[cfg(not(target_os = "macos"))]
fn set_dock_icon() {}

/// Drive `Kyde` into one named screenshot state. Called only when `KYDE_SHOT` is set
/// (see the call site in `main`). Each arm assumes the open project is this repo and sets
/// the exact plugin install set it needs so highlighting / the install banner are
/// deterministic regardless of any pre-existing config.
fn apply_shot(view: &mut Kyde, name: &str, window: &mut Window, cx: &mut Context<Kyde>) {
    // Screenshots hide the visible FPS counter — only the dedicated `fps` shot turns it on, in
    // its own arm.
    // Force the install list to exactly `on` (everything else uninstalled), then persist.
    let set_packs = |view: &mut Kyde, on: &[&str]| {
        for id in ["rust", "json", "markdown", "typescript", "javascript"] {
            if on.contains(&id) {
                view.plugins.install(id);
            } else {
                view.plugins.uninstall(id);
            }
        }
        view.plugins.save();
    };
    match name {
        // Commit view, a changed Rust file selected → side-by-side coloured diff.
        "git-diff" => {
            set_packs(view, &["rust"]);
            // The live repo may be clean (nothing to diff). screenshots.sh seeds a tiny
            // fixture repo with a guaranteed working-tree change and passes its path here,
            // so the diff pane always has content to show.
            if let Ok(repo) = std::env::var("KYDE_SHOT_REPO") {
                view.open_project(PathBuf::from(repo), cx);
            }
            view.enter_commit(cx);
            // Click a changed file so the main content shows a side-by-side diff: prefer
            // README.md, else fall back to the first changed file.
            let pick = view
                .files
                .iter()
                .position(|f| f.path.as_path() == std::path::Path::new("README.md"))
                .or(if view.files.is_empty() { None } else { Some(0) });
            if let Some(i) = pick {
                view.select_with(i, Some(cx));
            }
        }
        // Browse a Rust file with the pack uninstalled → "Install Rust support?" banner.
        "plugins" => {
            set_packs(view, &[]);
            view.open_file(PathBuf::from("src/main.rs"), cx);
        }
        // The Language Plugins native modal window (lists every language pack with toggles).
        // A few installed so both on/off states show.
        "plugins-window" => {
            set_packs(view, &["rust", "json", "markdown", "typescript"]);
            view.act_open_plugins(&OpenPlugins, window, cx);
        }
        // Browse a Markdown file with the pack installed → live rendered preview pane.
        "markdown-support" => {
            set_packs(view, &["markdown"]);
            view.open_file(PathBuf::from("README.md"), cx);
        }
        // Go to File (Cmd+Shift+O): the fuzzy file finder open over a file, with a query
        // typed so matched files are listed. Setting the query fires the editor's Changed
        // event → recompute_finder runs the fuzzy match.
        "go-to-file" => {
            set_packs(view, &["rust"]);
            view.open_file(PathBuf::from("src/main.rs"), cx);
            view.act_go_to_file(&GoToFile, window, cx);
            view.finder_query.update(cx, |e, cx| {
                e.set_content("render".to_string(), Lang::PlainText, cx)
            });
            cx.notify();
        }
        // Find in Files (Cmd+Shift+F): the content-search finder open over a file, with the
        // query "kyde" typed in so `git grep` results are showing. Setting the query content
        // fires the editor's Changed event → recompute_finder runs the grep.
        "find-in-files" => {
            set_packs(view, &["rust"]);
            view.open_file(PathBuf::from("src/main.rs"), cx);
            view.act_find_in_files(&FindInFiles, window, cx);
            view.finder_query.update(cx, |e, cx| {
                e.set_content("kyde".to_string(), Lang::PlainText, cx)
            });
            cx.notify();
        }
        // Commit view (diff behind) + the Rollback native modal window open over it.
        "rollback" => {
            set_packs(view, &["rust"]);
            view.enter_commit(cx);
            view.open_rollback_path(PathBuf::new(), cx);
        }
        // Browse a large file with the FPS monitor on, scrolled partway down.
        "fps" => {
            set_packs(view, &["json"]);
            view.show_fps = true;
            if let Ok(f) = std::env::var("KYDE_SHOT_FILE") {
                view.open_file(PathBuf::from(f), cx);
            }
            // Negative Y offset = scrolled down into the file.
            view.file_scroll
                .set_offset(gpui::point(px(0.0), px(-600.0 * editor::line_height_px())));
            cx.notify();
        }
        // History view: the commit log for the current branch, first commit selected so the
        // changed-files list + read-only diff are populated.
        "history" => {
            set_packs(view, &["rust"]);
            view.enter_history(cx);
        }
        // Browse view with the bottom terminal panel open, seeded with a couple of commands
        // so the shot shows a live shell (prompt + output), not a bare box.
        #[cfg(feature = "terminal")]
        "terminal" => {
            set_packs(view, &["rust"]);
            view.open_file(PathBuf::from("src/terminal.rs"), cx);
            view.term_open = true;
            view.new_terminal_tab(cx);
            if let Some(t) = view.term_tabs.last() {
                t.read(cx).send_input("git status && ls src\n");
            }
            view.focus_active_terminal(window, cx);
            cx.notify();
        }
        other => eprintln!("KYDE_SHOT: unknown state {other:?}"),
    }
}

fn main() {
    reexec_with_proper_name();
    set_app_name();
    install_crash_logger();
    // A path arg opens that project directly; no arg → the Projects landing view.
    // The path may be `.`, a relative dir, or any subdirectory of a repo — resolve it to
    // the repo's top level (`git rev-parse --show-toplevel`, via Repo::discover). If it
    // isn't inside a git repo, fall back to the Projects view rather than a broken state.
    let initial = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .and_then(|p| Repo::discover(&p).ok())
        .map(|repo| repo.root().to_path_buf());

    let (km, first_run) = Keymap::load();

    let app = Application::new().with_assets(Assets);
    // Remote markdown-preview images need a real HttpClient (gpui's default bails);
    // only wired when the `remote-images` feature is built in. See remote_img.rs.
    #[cfg(feature = "remote-images")]
    let app = app.with_http_client(std::sync::Arc::new(remote_img::UreqClient::new()));
    app.run(move |cx: &mut App| {
        load_fonts(cx);
        set_dock_icon();
        apply_keymap(cx, &km);

        // Native macOS menu bar: app menu + File (Open / Recent Projects).
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.set_menus(app_menus(&Recents::load()));
        // Dock right-click → "Recent Projects" submenu (refreshed on each open).
        cx.set_dock_menu(dock_menu(&Recents::load()));

        let bounds = Bounds::centered(None, gpui::size(px(1280.0), px(820.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // Transparent titlebar so our chrome blends into the native window bar;
                    // traffic lights nudged down to center in our 40px header strip.
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Kyde".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(gpui::point(px(16.0), px(16.0))),
                    }),
                    ..Default::default()
                },
                {
                    let km = km.clone();
                    let initial = initial.clone();
                    move |_, cx| cx.new(|cx| Kyde::new(initial.clone(), km.clone(), first_run, cx))
                },
            )
            .expect("failed to open main window");
        // Focus the root so global keybindings (Go to File, etc.) dispatch immediately.
        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle(cx));
                cx.activate(true);

                // Launched straight to an empty Projects screen (no repo arg, no recents,
                // and not mid first-run onboarding) → jump straight to the folder picker.
                if view.repo_root.is_none()
                    && view.recents.paths.is_empty()
                    && !view.onboarding_open
                {
                    view.pick_folder(cx);
                }

                // TEMP debug: KYDE_OPEN=<rel path> auto-opens a file on launch (for
                // deterministic screenshot verification without sending keystrokes).
                if let Ok(f) = std::env::var("KYDE_OPEN") {
                    view.open_file(std::path::PathBuf::from(f), cx);
                }

                // KYDE_SHOT=<name> drives the app into one fixed UI state for the
                // screenshot suite (scripts/screenshots.sh). Env-gated → zero cost on a
                // normal launch. Assumes the open project is this repo. The suite points
                // XDG_CONFIG_HOME at a throwaway dir, so the per-shot plugin install state
                // set here never touches the user's real ~/.config/kyde.
                if let Ok(shot) = std::env::var("KYDE_SHOT") {
                    apply_shot(view, &shot, window, cx);
                }

                // Re-sync git + open-file state whenever the window regains focus —
                // external tools may have edited files while we were in the background.
                // Fires on every activation change, so gate on becoming active.
                cx.observe_window_activation(window, |this, window, cx| {
                    if window.is_window_active() {
                        this.reload_external(cx);
                    }
                })
                .detach();
            })
            .ok();
    });
}

#[cfg(test)]
mod branch_tree_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn slash_becomes_folder_under_sections() {
        let all = vec![
            "feat/compare".to_string(),
            "feat/stats".to_string(),
            "main".to_string(),
        ];
        let mut exp = HashSet::new();
        exp.insert("sec:local".to_string());
        exp.insert("sec:local/feat".to_string());
        let rows = branch_rows(&[], &all, &[], &exp, false);

        // Section root present.
        assert!(matches!(
            rows[0].node,
            BranchNode::Folder { section: true, .. }
        ));
        assert_eq!(rows[0].label, "Local");
        // A "feat" folder exists with the two leaves nested deeper.
        assert!(rows
            .iter()
            .any(|r| r.label == "feat"
                && matches!(r.node, BranchNode::Folder { section: false, .. })));
        let compare = rows.iter().find(|r| r.label == "compare").unwrap();
        assert!(matches!(&compare.node, BranchNode::Leaf { full } if full == "feat/compare"));
        // "main" is a top-level leaf in the section (depth 1).
        let main = rows.iter().find(|r| r.label == "main").unwrap();
        assert_eq!(main.depth, 1);
    }

    #[test]
    fn collapsed_section_hides_children() {
        let all = vec!["main".to_string()];
        let rows = branch_rows(&[], &all, &[], &HashSet::new(), false);
        // Only the collapsed "Local" root, no leaves.
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Local");
    }
}

/// Live-gpui smoke tests: run the real app headlessly (`TestAppContext`) so runtime panics
/// — re-entrant entity updates, wrong-phase calls, etc. — fail the build. Pure-function and
/// `perf_*` tests can't see these; this is the only category that exercises windows +
/// entities + the render cycle. See README "Performance" / CLAUDE.md.
#[cfg(test)]
mod gpui_smoke_tests {
    use super::*;
    use gpui::TestAppContext;

    /// Build a `Kyde` window against a throwaway git repo, return its handle + a visual cx.
    fn boot(cx: &mut TestAppContext) -> (gpui::WindowHandle<Kyde>, std::path::PathBuf) {
        // A real temp git repo with one change, so the commit/diff/rollback screens populate.
        // Unique per boot() call: pid is shared across parallel test threads, so a bare-pid
        // dir races (tests remove_dir_all/create_dir_all the same path at once → flaky panic).
        static SMOKE_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let seq = SMOKE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kyde-smoke-{}-{}", std::process::id(), seq));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(dir.join("app.tsx"), "const a = 1;\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);
        std::fs::write(dir.join("app.tsx"), "const a = 2;\n").unwrap();
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();

        let km = Keymap::default();
        let root = Some(dir.clone());
        let handle = cx.add_window(move |_w, cx| Kyde::new(root.clone(), km.clone(), false, cx));
        cx.run_until_parked();
        (handle, dir)
    }

    /// The Create-New-Branch dialog (type a name → Create) must create + switch to the branch,
    /// and a typed space becomes a hyphen (git rejects spaces). Guards "New Branch does
    /// nothing" + the space-in-name error.
    #[gpui::test]
    fn new_branch_dialog_creates_and_slugifies(cx: &mut TestAppContext) {
        assert_eq!(slugify_branch("  new branch "), "new-branch");
        assert_eq!(
            slugify_branch("feat/compare-overrides"),
            "feat/compare-overrides"
        );
        // Pasting a commit subject: `:` and spaces are git-forbidden → hyphens, runs collapsed.
        assert_eq!(
            slugify_branch("fix: new branch shows whole repo"),
            "fix-new-branch-shows-whole-repo"
        );
        // Other forbidden chars + sequences are sanitised; namespacing slash preserved.
        assert_eq!(slugify_branch("wip/foo?*bar~baz"), "wip/foo-bar-baz");
        assert_eq!(slugify_branch("a..b@{c"), "a.b-c");
        assert_eq!(slugify_branch("///lead..trail///"), "lead.trail");
        assert_eq!(slugify_branch("hotfix.lock"), "hotfix");

        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.open_new_branch(cx);
                // Type a name with a space — it should be slugified on Create.
                k.branch_query.update(cx, |e, cx| {
                    e.set_content("new branch".into(), Lang::PlainText, cx)
                });
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| k.do_create_branch(cx))
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert_eq!(k.current_branch.as_deref(), Some("new-branch"));
                assert!(
                    k.new_branch_win.is_none(),
                    "dialog should close after Create"
                );
            })
            .unwrap();
    }

    /// Clicking "Rollback" in the rollback window must discard the changes AND close the
    /// window (the close is deferred because it fires from inside that window's own button).
    #[gpui::test]
    fn rollback_action_closes_window(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| k.open_rollback_path(PathBuf::new(), cx))
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| assert!(k.rollback_win.is_some()))
            .unwrap();
        // Simulate the Rollback button.
        handle.update(cx, |k, _w, cx| k.do_rollback(cx)).unwrap();
        cx.run_until_parked(); // let the deferred remove_window run
        handle
            .update(cx, |k, _w, _cx| {
                assert!(k.rollback_win.is_none(), "Rollback should close its window");
                // The modified file was discarded (untracked new.txt survives without "delete").
                assert!(
                    !k.files.iter().any(|f| f.path.ends_with("app.tsx")),
                    "the modified file should have been rolled back"
                );
            })
            .unwrap();
    }

    /// Opening + rendering the Rollback native window must not panic. (It previously crashed
    /// via a re-entrant `Entity<Kyde>` update when opened during a Kyde update; it's now opened
    /// from a spawned task. This guards the window opens, renders its body, and is tracked.)
    #[gpui::test]
    fn opening_rollback_window_does_not_panic(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        // Root path = roll back all changes (boot leaves app.tsx + new.txt changed).
        handle
            .update(cx, |k, _w, cx| k.open_rollback_path(PathBuf::new(), cx))
            .unwrap();
        cx.run_until_parked(); // the window opens + renders on the spawned task
        handle
            .update(cx, |k, _w, _cx| {
                assert!(k.rollback_win.is_some(), "rollback window should be open")
            })
            .unwrap();
    }

    /// Virtualization guard for big files — the regression that *did* slip through: a
    /// 37k-line `package-lock.json` scrolled at ~30fps because the editor shaped a line (and
    /// a fold chevron) for **every row in the file** each frame, not just the on-screen band.
    /// Pure `perf_*` tests couldn't catch it (the cost is in the windowed render path), and
    /// the panic-only screen test didn't open a big file. This opens a 15k-line file in the
    /// real Kyde layout (its scroll container clips the editor) and asserts the last frame
    /// shaped only ≈ the visible window, not the whole file. Revert the windowing and
    /// `shaped` jumps to the file's row count → this fails.
    #[gpui::test]
    fn big_file_editor_only_shapes_visible_rows(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        // 15k lines of foldable JSON (objects → a fold start on most lines, the exact shape
        // that made the chevron-shaping cost O(file)).
        let mut big = String::from("{\n");
        for i in 0..15000 {
            big.push_str(&format!("  \"key_{i}\": {{ \"n\": {i} }},\n"));
        }
        big.push_str("  \"end\": true\n}\n");
        std::fs::write(dir.join("big.json"), &big).unwrap();

        handle
            .update(cx, |k, _w, cx| {
                k.mode = Mode::Browse;
                k.open_file(PathBuf::from("big.json"), cx);
            })
            .unwrap();
        // Let async highlight/fold land, then force a painted frame.
        for _ in 0..3 {
            cx.refresh().unwrap();
            cx.run_until_parked();
        }

        handle
            .update(cx, |k, _w, cx| {
                let ed = k.file_editor.read(cx);
                let (shaped, rows) = (ed.shaped_row_count(), ed.display_row_count());
                assert!(rows > 5000, "expected a big file ({rows} display rows)");
                // Visible band + 12-row overscan each side is well under a few hundred; the
                // whole-file count would be ~15000. A loose ceiling catches the regression
                // without depending on the exact test window height.
                assert!(
                    shaped < 600,
                    "editor shaped {shaped} rows of a {rows}-row file — virtualization broke \
                     (should shape only the on-screen window)"
                );
            })
            .unwrap();
    }

    /// Render every screen — with the FPS monitor ON — and assert none panics. This is the
    /// project's broad runtime guard: it actually drives the render cycle (layout + prepaint
    /// + paint) for each view, the category that pure-function/`perf_*` tests can't reach.
    /// (Headless can't measure real GPU fps — virtual time, no Metal — so per-frame *cost* is
    /// guarded by the deterministic `perf_*` tests instead; this guards correctness/panics.)
    #[gpui::test]
    fn every_screen_renders_without_panic(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        let settle = |cx: &mut TestAppContext| {
            cx.refresh().unwrap(); // mark all windows dirty → repaint on park
            cx.run_until_parked();
        };

        // FPS monitor on for the whole sweep (exercises the request_animation_frame +
        // render-timing path on every screen too).
        handle.update(cx, |k, _w, _cx| k.show_fps = true).unwrap();
        settle(cx);

        // Browse with a file open in the editor.
        handle
            .update(cx, |k, _w, cx| {
                k.open_file(std::path::PathBuf::from("app.tsx"), cx)
            })
            .unwrap();
        settle(cx);

        // Find bar (cmd-F) over the open file.
        handle
            .update(cx, |k, w, cx| k.open_find(false, w, cx))
            .unwrap();
        settle(cx);

        // Go-to-File finder overlay.
        handle
            .update(cx, |k, w, cx| k.act_go_to_file(&GoToFile, w, cx))
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, _cx| k.finder_open = false)
            .unwrap();

        // Commit view with a changed file selected → the side-by-side diff renders.
        handle
            .update(cx, |k, _w, cx| {
                k.enter_commit(cx);
                k.select_with(0, Some(cx));
            })
            .unwrap();
        settle(cx);

        // Show-Diff native window.
        handle
            .update(cx, |k, _w, cx| k.menu_show_diff(0, cx))
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, cx| k.close_modal_window(ModalKind::Diff, cx))
            .unwrap();
        settle(cx);

        // Branch popup.
        handle
            .update(cx, |k, w, cx| k.toggle_branch_popup(w, cx))
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, _cx| k.branch_popup_open = false)
            .unwrap();

        // Delete-confirmation modal.
        handle
            .update(cx, |k, _w, cx| k.open_delete(dir.join("new.txt"), cx))
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, _cx| k.delete_target = None)
            .unwrap();

        // Onboarding / keymap picker overlay.
        handle
            .update(cx, |k, _w, _cx| k.onboarding_open = true)
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, _cx| k.onboarding_open = false)
            .unwrap();

        // Plugin manager (native modal window).
        handle
            .update(cx, |k, w, cx| k.act_open_plugins(&OpenPlugins, w, cx))
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, cx| k.close_modal_window(ModalKind::Plugins, cx))
            .unwrap();
        settle(cx);

        // Font preview (native modal window).
        handle
            .update(cx, |k, w, cx| k.run_palette(PaletteAction::Fonts, w, cx))
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, cx| k.close_modal_window(ModalKind::Fonts, cx))
            .unwrap();
        settle(cx);

        // Clear-data confirmation (native modal window; render only — never click confirm).
        handle
            .update(cx, |k, w, cx| k.act_clear_data(&ClearData, w, cx))
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.close_modal_window(ModalKind::ClearData, cx)
            })
            .unwrap();
        settle(cx);

        // Rollback native window.
        handle
            .update(cx, |k, _w, cx| k.open_rollback_path(PathBuf::new(), cx))
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.close_modal_window(ModalKind::Rollback, cx)
            })
            .unwrap();
        settle(cx);

        // Push confirmation native window.
        handle
            .update(cx, |k, _w, cx| k.open_push_modal(cx))
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, cx| k.close_modal_window(ModalKind::Push, cx))
            .unwrap();
        settle(cx);

        // Projects landing view (no repo open).
        handle.update(cx, |k, _w, _cx| k.repo_root = None).unwrap();
        settle(cx);

        // No panic across any screen = pass.
    }

    #[test]
    fn url_encodes_query() {
        assert_eq!(url_encode("a b/c?d"), "a%20b%2Fc%3Fd");
        assert_eq!(url_encode("safe-_.~"), "safe-_.~");
    }

    #[test]
    fn issue_url_targets_repo_and_has_title_body() {
        let url = crash_issue_url("=== panic at src/x.rs:1 ===\nboom\nbacktrace…");
        assert!(url.starts_with("https://github.com/kyle-ssg/Kyde/issues/new?title="));
        assert!(url.contains("&body="));
        assert!(url.contains("boom") || url.contains("Crash"));
    }
}

/// A full-window dimmed overlay that centers its child. When `dismissable`, clicking the
/// backdrop closes the open overlays; otherwise the backdrop swallows the click (modal).
pub(crate) fn overlay(cx: &mut Context<Kyde>, dismissable: bool) -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        // A dim scrim, not a blackout — the app stays visible behind the modal.
        .bg(gpui::rgba(0x00000099))
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _e, window, cx| {
                if dismissable {
                    this.finder_open = false;
                    this.onboarding_open = false;
                    this.delete_target = None;
                    window.focus(&this.focus_handle);
                    cx.notify();
                }
            }),
        )
}
