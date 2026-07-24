//! Kyde — fast native macOS git commit/diff tool, `IntelliJ` "Commit Changes" style.
//!
//! Features: changed-files tree + side-by-side diff, per-hunk stage/revert chevrons,
//! editable commit message, folder browse with a real syntax-highlighted editor,
//! a fuzzy "Go to File" finder (Cmd+Shift+O), a configurable keymap with `WebStorm`
//! and `VSCode` presets, and a re-accessible onboarding / keymap picker.
//!
//! Lints (unwrap/expect deny, `clippy::pedantic`) are configured centrally in
//! `[workspace.lints]` (Cargo.toml) and opted into via `[lints] workspace = true`.

// ── core shell ──
mod app;
mod divider;
// Always compiled (so its unit tests run in any feature set), but only *used* behind the
// `terminal` feature — allow dead code when that's off.
#[cfg_attr(not(feature = "terminal"), allow(dead_code))]
mod term_panel;
// `TermPanel` is ungated — `TermState.panel` is always compiled (see `TermState`), so e.g.
// `switch_mode` can reset it without a cfg. `ToggleAction` is only used by the gated glue.
use term_panel::TermPanel;
#[cfg(feature = "terminal")]
use term_panel::ToggleAction;
mod render;
pub(crate) use divider::{full_island_w, Divider, DIFF_GUTTER_W};
// Reusable, app-agnostic UI toolkit (its own crate). Aliased to `ui` (so `ui::tree::item`
// resolves) and glob-re-exported so call sites use `btn_primary`/`file_badge`/… unqualified.
pub(crate) use app::{
    CONTENT_MIN_QUERY, CONTENT_SEARCH_DEBOUNCE, FINDER_RESULT_CAP, SCROLL_CONTEXT_ROWS,
    STATUS_REFRESH_DEBOUNCE,
};
pub(crate) use kyde_ui as ui;
pub(crate) use kyde_ui::{
    badge_inner, btn_primary, btn_primary_state, btn_secondary, checkbox_box, file_badge, lerp_rgb,
    menu_icon, scrollbar_thumb, tab_pill,
};

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
        SortLines,
        SortKeys,
        NavBack,
        NavForward,
        EscapeKey,
        ConfirmKey,
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
    bind_app(cx, km, "sort_lines", SortLines);
    bind_app(cx, km, "sort_keys", SortKeys);
    bind_app(cx, km, "nav_back", NavBack);
    bind_app(cx, km, "nav_forward", NavForward);
    // Escape: close any open modal, else cancel the Commit view (fixed key).
    cx.bind_keys([KeyBinding::new("escape", EscapeKey, Some("Kyde"))]);
    // Enter: confirm the open confirmation dialog (currently the Delete overlay) —
    // IDE default-button behavior. No dialog open → no-op. "Kyde" context, so the
    // editors/finder/terminal keep their own Enter.
    cx.bind_keys([KeyBinding::new("enter", ConfirmKey, Some("Kyde"))]);
    // Backspace: delete the selected Browse-tree file/folder (fixed key). Bound to the
    // "Kyde" context, NOT globally, so the deeper editor/commit-box/terminal Backspace
    // bindings win whenever one of those is focused — this only fires at the app root.
    // ⌘⌫ deletes too (issue #61 — Finder's delete shortcut; muscle memory says both work).
    // Safe at the root: the editor binds cmd-backspace to DeleteToHome in its own deeper
    // context, so this only fires when no editor/terminal is focused.
    cx.bind_keys([
        KeyBinding::new("backspace", DeleteFile, Some("Kyde")),
        KeyBinding::new("cmd-backspace", DeleteFile, Some("Kyde")),
    ]);
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
        // ⌘⌫ is DeleteFile in "Kyde" (issue #61); in the terminal it must act as a plain
        // backspace, not delete the tree-selected file out from under a typing user.
        KeyBinding::new("cmd-backspace", TerminalBackspace, Some("Terminal")),
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
    SortLines,
    SortKeys,
    NavBack,
    NavForward,
    LocalHistory,
    ClearLocalHistory,
}

/// Action-finder entries: (label, action, keymap-action name for the shortcut
/// chip — `""` when the action has no bindable key and so shows none).
const PALETTE: &[(&str, PaletteAction, &str)] = &[
    ("Select in File View", PaletteAction::SelectInTree, ""),
    ("Reveal in Finder", PaletteAction::RevealInFinder, ""),
    ("Reveal in Terminal", PaletteAction::RevealInTerminal, ""),
    ("Sort Lines", PaletteAction::SortLines, "sort_lines"),
    ("Sort Object Keys", PaletteAction::SortKeys, "sort_keys"),
    ("Back (previous file)", PaletteAction::NavBack, "nav_back"),
    (
        "Forward (next file)",
        PaletteAction::NavForward,
        "nav_forward",
    ),
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
    ("Local History", PaletteAction::LocalHistory, ""),
    ("Clear Local History", PaletteAction::ClearLocalHistory, ""),
    ("Settings / Keymap", PaletteAction::Settings, "open_keymap"),
    ("Manage Plugins", PaletteAction::Plugins, ""),
    ("Preview Fonts", PaletteAction::Fonts, ""),
];

/// What a right-click context menu was opened on.
#[derive(Clone)]
enum MenuTarget {
    /// A path in the Browse tree (`bool` = `is_dir`), or the open editor file.
    BrowseFile(PathBuf, bool),
    /// Right-click inside the editor pane — git commands plus the buffer sort
    /// ops. The bools (computed at menu-open, so the menu never parses per
    /// frame) gate the items: `.0` = selection spans ≥2 lines (Sort Lines),
    /// `.1` = caret inside a JSON/JS/TS object (Sort Object Keys).
    EditorGit(PathBuf, bool, bool),
    /// A path (file or folder) in the Commit tree — `bool` = `is_dir`.
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
    /// A file row in the History changed-files tree (by index into `history.files`) — same
    /// compare modes, applied with that file kept selected.
    HistoryFile(usize),
    /// A timeline row in the Local History window — Revert This Change and After
    /// (acts on the selected row; right-click selects before opening the menu).
    LhRow,
    /// A row in the Local History changed-files panel (`bool` = `is_dir`) — revert just
    /// that file / that folder's files to their state at the selected snapshot.
    LhPath(PathBuf, bool),
    /// A branch leaf in the branch popup — offers Checkout + Merge into current.
    Branch(String),
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

/// The History (git log) view's state, grouped out of the `Kyde` god-struct into its own
/// sub-state. Defined at the crate root so its fields stay reachable from the `history`
/// feature module (exactly like `Kyde`'s own fields). Built via [`HistoryView::new`], which
/// also wires the three search boxes' live-filter subscriptions.
struct HistoryView {
    /// Revision being logged — a branch name, or "HEAD" for the current branch.
    rev: String,
    /// Path the log is scoped to (a folder/file), or `None` for the whole repo. Set when
    /// the history view is opened from a Browse-tree folder's right-click menu.
    path: Option<PathBuf>,
    /// Commits shown in the log list (newest first).
    commits: Vec<git::Commit>,
    /// Selected commit index into `commits`.
    selected: Option<usize>,
    /// Files changed by the selected commit under the current compare mode.
    files: Vec<ChangedFile>,
    /// Selected file index into `files`.
    file_selected: Option<usize>,
    /// Folder tree of `files` (right pane of the history panel).
    files_tree: tree::Tree,
    /// Expanded dirs in the history files tree.
    files_expanded: std::collections::HashSet<PathBuf>,
    /// Search box filtering the history files tree.
    files_query: Entity<CodeEditor>,
    /// Height (px) of the history bottom panel (drag the top edge to resize).
    panel_h: f32,
    /// When true the history bottom panel is minimised to just its toolbar (the header
    /// chevron toggles it), giving the diff the full height.
    panel_collapsed: bool,
    /// What the selected commit is diffed against.
    compare: CompareMode,
    /// Compare-mode dropdown open in the history view.
    compare_open: bool,
    /// Branch-picker dropdown open in the history view.
    branch_open: bool,
    /// Local branches for the history branch picker (loaded when the dropdown opens).
    locals: Vec<String>,
    /// Remote-tracking branches for the history branch picker.
    remotes: Vec<String>,
    /// Search box filtering the history branch dropdown.
    branch_query: Entity<CodeEditor>,
    /// Search box filtering the commit list (subject / author / hash).
    commit_query: Entity<CodeEditor>,
    /// Scroll position of the commit list.
    scroll: ScrollHandle,
    /// Fraction (0..1) of the history panel width given to the commit-list pane on the left
    /// (resizable); the changed-files pane fills the rest on the right. Defaults to 2/3.
    commit_frac: f32,
}

impl HistoryView {
    /// Build the initial history state and wire the three search boxes' live-filter
    /// subscriptions (each repaints `Kyde` when its query changes while the relevant view /
    /// dropdown is active).
    fn new(cx: &mut Context<Kyde>) -> Self {
        let branch_query = cx.new(|cx| CodeEditor::single_line(cx, "Search branches…"));
        cx.subscribe(&branch_query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.history.branch_open {
                cx.notify();
            }
        })
        .detach();
        let commit_query = cx.new(|cx| CodeEditor::single_line(cx, "Search commits…"));
        cx.subscribe(&commit_query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.mode == Mode::History {
                cx.notify();
            }
        })
        .detach();
        let files_query = cx.new(|cx| CodeEditor::single_line(cx, "Search files…"));
        cx.subscribe(&files_query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.mode == Mode::History {
                cx.notify();
            }
        })
        .detach();
        Self {
            rev: "HEAD".to_string(),
            path: None,
            commits: Vec::new(),
            selected: None,
            files: Vec::new(),
            file_selected: None,
            files_tree: tree::Tree::default(),
            files_expanded: std::collections::HashSet::new(),
            files_query,
            panel_h: 320.0,
            panel_collapsed: false,
            compare: CompareMode::Local,
            compare_open: false,
            branch_open: false,
            locals: Vec::new(),
            remotes: Vec::new(),
            branch_query,
            commit_query,
            scroll: ScrollHandle::new(),
            commit_frac: 2.0 / 3.0,
        }
    }
}

/// The branch-switcher popup's state (bottom-right status-bar chip → dropdown), grouped out
/// of the `Kyde` god-struct. Defined at the crate root so its fields stay reachable from the
/// feature modules. `current_branch` deliberately stays on `Kyde` — it's repo state read all
/// over (refresh/history/status bar), not popup UI.
struct BranchPopup {
    /// Local branch names, recency order.
    list: Vec<String>,
    /// Remote-only branches (short name, e.g. "feature-x") that have no local head yet —
    /// shown under a "Remote" section so freshly-fetched branches are checkout-able.
    remotes: Vec<String>,
    /// Whether the branch dropdown is open.
    popup_open: bool,
    /// Search / new-branch-name box (doubles as the create-branch name field).
    query: Entity<CodeEditor>,
    /// Expanded nodes in the branch tree (section keys like "sec:recent" and folder
    /// keys like "sec:local/feat").
    expanded: std::collections::HashSet<String>,
    /// Per-branch `(ahead, behind)` vs the current HEAD — the popup's count badges.
    /// Gathered in the background ON POPUP OPEN (one `rev-list` per branch), never on
    /// the render path (same pattern as the worktree popup's changed-file counts).
    counts: std::collections::HashMap<String, (usize, usize)>,
    /// Guards `counts` against a superseded gather (bumped per popup open).
    counts_gen: u64,
}

impl BranchPopup {
    /// Build initial branch-popup state + wire the search box's live-filter subscription
    /// (repaints `Kyde` while the popup is open).
    fn new(cx: &mut Context<Kyde>) -> Self {
        let query = cx.new(|cx| CodeEditor::single_line(cx, "Search / new branch name"));
        cx.subscribe(&query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.branch.popup_open {
                cx.notify();
            }
        })
        .detach();
        Self {
            list: Vec::new(),
            remotes: Vec::new(),
            popup_open: false,
            query,
            expanded: std::collections::HashSet::new(),
            counts: std::collections::HashMap::new(),
            counts_gen: 0,
        }
    }
}

/// The worktree-switcher popup's state (status-bar chip → dropdown, next to the branch
/// chip), grouped out of the `Kyde` god-struct like `BranchPopup`. The worktree list comes
/// from the refresh snapshot (one `git worktree list` per refresh, off the UI thread); the
/// per-worktree changed-file counts are gathered only when the popup opens — one `git
/// status` per linked worktree, never on the render path.
struct WorktreePopup {
    /// Whether the dropdown is open.
    popup_open: bool,
    /// All worktrees of the open repo, main first (from the last refresh snapshot).
    /// Length ≤ 1 ⇔ no linked worktrees ⇔ the chip is hidden.
    list: Vec<git::Worktree>,
    /// Changed-file count per worktree path, filled asynchronously on popup open
    /// (missing entry = still loading). Keyed by the worktree's root path.
    counts: std::collections::HashMap<PathBuf, usize>,
    /// Bumped on each popup open so a stale background count-gather can't overwrite
    /// a newer one.
    counts_gen: u64,
}

impl WorktreePopup {
    fn new() -> Self {
        Self {
            popup_open: false,
            list: Vec::new(),
            counts: std::collections::HashMap::new(),
            counts_gen: 0,
        }
    }
}

/// The in-editor find/replace bar's state, grouped out of the `Kyde` god-struct. Targets
/// whichever editor was focused when it opened (`target`). Built via [`FindBar::new`], which
/// wires the query box's live re-search subscription.
struct FindBar {
    /// Whether the find bar is open.
    open: bool,
    /// Whether the replace row is shown.
    replace: bool,
    /// Which editor the search acts on (Browse file editor vs a diff pane).
    target: FindTarget,
    /// Search box.
    query: Entity<CodeEditor>,
    /// Replace box.
    replace_query: Entity<CodeEditor>,
    /// Byte ranges of the current matches in the target editor.
    matches: Vec<std::ops::Range<usize>>,
    /// Index of the active match within `matches`.
    idx: usize,
}

impl FindBar {
    /// Build the find/replace bar (both single-line inputs use the `FindBar` key context for
    /// their enter/escape bindings) and wire the query box's live re-search subscription.
    fn new(cx: &mut Context<Kyde>) -> Self {
        let query = cx.new(|cx| {
            let mut e = CodeEditor::single_line(cx, "Find");
            e.ctx_override = Some("FindBar");
            e
        });
        let replace_query = cx.new(|cx| {
            let mut e = CodeEditor::single_line(cx, "Replace");
            e.ctx_override = Some("FindBar");
            e
        });
        cx.subscribe(&query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.find.open {
                this.recompute_find(cx);
            }
        })
        .detach();
        Self {
            open: false,
            replace: false,
            target: FindTarget::File,
            query,
            replace_query,
            matches: Vec::new(),
            idx: 0,
        }
    }
}

/// The Go-to-File / Find-in-Files / Actions overlay's state (one overlay, several modes),
/// grouped out of the `Kyde` god-struct. Built via [`Finder::new`], which wires the query
/// box's change subscription (in-memory fuzzy match inline, `git grep` content search
/// debounced on a background thread).
struct Finder {
    /// Whether the overlay is open.
    open: bool,
    /// Files (Go to File) vs Content (Find in Files) vs Actions palette — same overlay.
    mode: FinderMode,
    /// Search box.
    query: Entity<CodeEditor>,
    /// Fuzzy file-path results (Files mode).
    results: Vec<PathBuf>,
    /// Content-search hits (Content mode).
    content_results: Vec<ContentHit>,
    /// Matching palette-action indices (Actions mode).
    action_results: Vec<usize>,
    /// Highlighted result row.
    selected: usize,
    /// Bumped on every Find-in-Files keystroke; the debounced background `git grep` only
    /// applies its results when its captured generation still matches (drops stale searches).
    search_gen: u64,
}

impl Finder {
    /// Build the finder overlay + wire the query box's change subscription: Content mode
    /// debounces a background `git grep`, every other mode is an inline fuzzy match.
    fn new(cx: &mut Context<Kyde>) -> Self {
        let query = cx.new(|cx| CodeEditor::single_line(cx, "Type to search files…"));
        cx.subscribe(&query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.finder.open {
                if this.finder.mode == FinderMode::Content {
                    this.schedule_content_search(cx);
                } else {
                    this.recompute_finder(cx);
                    cx.notify();
                }
            }
        })
        .detach();
        Self {
            open: false,
            mode: FinderMode::Files,
            query,
            results: Vec::new(),
            content_results: Vec::new(),
            action_results: Vec::new(),
            selected: 0,
            search_gen: 0,
        }
    }
}

/// The first-run / reopened keymap-picker overlay's state, grouped out of the `Kyde`
/// god-struct (see `render_onboarding`).
struct Onboarding {
    /// Whether the picker overlay is open.
    open: bool,
    /// True until the user has picked a keymap — the picker can't be dismissed while set.
    forced: bool,
    /// The preset currently highlighted in the picker (confirmed via Continue).
    choice: Preset,
    /// Pending state of the "Install shell command" checkbox in the picker;
    /// applied (symlink created) when the user confirms with Continue.
    install_cmd: bool,
    /// Last shell-command install error, shown under the checkbox.
    shell_cmd_error: Option<String>,
}

/// The bottom terminal panel's state, grouped out of the `Kyde` god-struct. The control
/// state (`panel`) is always compiled — it's the pure, unit-tested open/close/toggle/focus
/// state machine (see `src/term_panel.rs`) — so always-compiled code (e.g. `switch_mode`'s
/// un-maximize) can touch it without a `cfg` gate. Only the PTY-backed tab views + the
/// panel height live behind the `terminal` feature, centralising the gates here instead of
/// scattering them across the `Kyde` struct + constructor.
struct TermState {
    /// Control state of the panel — open / active / maximized / focus-pending.
    panel: TermPanel,
    /// One PTY-backed `TerminalView` entity per tab, left→right in open order.
    #[cfg(feature = "terminal")]
    tabs: Vec<Entity<terminal::TerminalView>>,
    /// Height (px) of the terminal panel, drag-resizable via its top divider.
    #[cfg(feature = "terminal")]
    height: f32,
}

impl TermState {
    fn new() -> Self {
        Self {
            // Starts closed; the persisted "maximized" preference is restored when it opens
            // (act_toggle_terminal), so the default (all-false) is correct here.
            panel: TermPanel::default(),
            #[cfg(feature = "terminal")]
            tabs: Vec::new(),
            #[cfg(feature = "terminal")]
            height: 260.0,
        }
    }
}

/// The Browse (code) view's state, grouped out of the `Kyde` god-struct: the folder tree,
/// the editor tabs (incl. the VS Code-style preview tab), the file editor entity, and the
/// markdown split. Built via [`BrowseView::new`], which wires the editor's autosave
/// subscription.
struct BrowseView {
    /// All tracked+untracked files (git) or the filesystem walk (non-git) — the tree's data.
    all_files: Vec<PathBuf>,
    /// The lazy dir→children folder-tree model built from `all_files`.
    tree: tree::Tree,
    /// Directories currently expanded in the tree.
    expanded: std::collections::HashSet<PathBuf>,
    /// Width of the file-tree pane, drag-resizable via the divider.
    tree_width: f32,
    /// True when the file tree is minimized to a thin strip (the `−` button).
    tree_collapsed: bool,
    /// The active editor tab (`None` = nothing open).
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
    /// Highlighted row in the tree (file OR folder); drives the breadcrumb.
    /// Distinct from `open_path` so selecting a folder doesn't change the editor.
    selected_path: Option<PathBuf>,
    /// Cmd-clicked FILE rows (ordered). Exactly two → the right-click menu
    /// offers "Compare Selected" (issue #42). Cleared by any plain click.
    multi_selected: Vec<PathBuf>,
    /// Visited-file history for ⌘⌥←/⌘⌥→ back/forward navigation.
    nav_history: Vec<PathBuf>,
    /// Current position in `nav_history` (points at the open file's entry).
    nav_index: usize,
    /// True while back/forward itself opens a file, so the open doesn't record
    /// a new history entry (which would truncate the forward branch).
    nav_suppress: bool,
    /// Import targets the external-defs index was last computed for — the
    /// cheap change gate (recompute only when the buffer's import set changes).
    ext_defs_targets: Vec<String>,
    /// Generation counter for the external-defs background job (stale results
    /// from a superseded compute are dropped).
    ext_defs_gen: u64,
    /// Scroll position of the tree, so "Select Opened File in Tree" can scroll an
    /// off-screen row into view.
    tree_scroll: ScrollHandle,
    /// The file editor entity.
    editor: Entity<CodeEditor>,
    /// Scroll handle for the editor pane — drives the hover scrollbars.
    editor_scroll: ScrollHandle,
    /// Vertical scroll handle for the markdown split's code (left) pane.
    md_editor_scroll: ScrollHandle,
    /// Vertical scroll handle for the markdown split's rendered preview (right) pane.
    md_preview_scroll: ScrollHandle,
    /// Persistent selectable rendered-markdown view (holds the preview's text selection).
    md_view: Option<gpui::Entity<mdview::MarkdownView>>,
    /// Editor pane width (px) of the markdown side-by-side split (drag-resizable).
    md_editor_w: f32,
}

impl BrowseView {
    /// Build the file editor + wire its autosave subscription: every real edit persists to
    /// disk immediately (no Save button). Gated on `dirty` so loading a file (`set_content`
    /// emits `Changed` with `dirty=false`) doesn't rewrite it.
    fn new(cx: &mut Context<Kyde>) -> Self {
        // No placeholder: an empty open file should read as empty, not show prompt text.
        let editor = cx.new(|cx| CodeEditor::new(cx, String::new(), Lang::PlainText, ""));
        cx.subscribe(&editor, |this, _e, ev, cx| match ev {
            EditorEvent::Changed if this.browse.editor.read(cx).dirty => {
                // Editing a preview (temporary) tab promotes it to a permanent tab — VS Code
                // behaviour, so the edit survives the next single-click elsewhere.
                if this.browse.preview_tab.is_some()
                    && this.browse.preview_tab == this.browse.open_path
                {
                    this.browse.preview_tab = None;
                }
                this.autosave(cx);
                // Import lines changed → re-index the imported files' definitions
                // (cheap target-set comparison; the compute itself is background).
                this.refresh_external_defs(cx);
            }
            // ⌘-click on an import link → resolve against the project file list
            // and open the target (issue #26).
            EditorEvent::OpenImport(link) => this.open_import_link(link.clone(), cx),
            // ⌘-click on a USE of an imported symbol → open its file and land
            // on the definition.
            EditorEvent::OpenSymbol { link, name } => {
                this.open_import_symbol(link.clone(), name.clone(), cx);
            }
            // ⌘-click on a pre-resolved external definition (imported-files
            // index) → open the file at the definition.
            EditorEvent::OpenDefinition { path, range } => {
                this.open_definition_at(path.clone(), range.clone(), cx);
            }
            EditorEvent::Changed => {}
        })
        .detach();
        Self {
            all_files: Vec::new(),
            tree: tree::Tree::default(),
            // Root folder starts expanded so the tree shows on open.
            expanded: std::collections::HashSet::from([PathBuf::new()]),
            tree_width: 320.0,
            tree_collapsed: false,
            open_path: None,
            open_tabs: Vec::new(),
            preview_tab: None,
            scratches: Vec::new(),
            tab_scroll: ScrollHandle::new(),
            selected_path: None,
            multi_selected: Vec::new(),
            nav_history: Vec::new(),
            nav_index: 0,
            nav_suppress: false,
            ext_defs_targets: Vec::new(),
            ext_defs_gen: 0,
            tree_scroll: ScrollHandle::new(),
            editor,
            editor_scroll: ScrollHandle::new(),
            md_editor_scroll: ScrollHandle::new(),
            md_preview_scroll: ScrollHandle::new(),
            md_view: None,
            md_editor_w: 480.0,
        }
    }
}

/// The Commit view's state, grouped out of the `Kyde` god-struct: the changed-files
/// checkbox tree, the commit-message editor, and the view chrome. The changed-files *data*
/// (`files`/`selected`) stays on `Kyde` — it's repo status written by `refresh` and read
/// by Browse (tab colors) and rollback too, not commit-view-private UI.
struct CommitView {
    /// Changed files highlighted as a group in the commit list (e.g. after a folder
    /// "Commit" picks every change under it). Cleared on a plain single-file click.
    focus: std::collections::HashSet<PathBuf>,
    /// Changed files as a folder tree.
    tree: tree::Tree,
    /// Expanded dirs in that tree.
    expanded: std::collections::HashSet<PathBuf>,
    /// Which changed files are checked-to-commit.
    checked: std::collections::HashSet<PathBuf>,
    /// Per-file hunks unticked in the diff gutter (by hunk index) — those changes stay out
    /// of the commit (partial commit). Absent/empty = the whole file commits. Cleared per
    /// file on re-diff (indices shift with edits) and wholesale after a commit.
    excluded_hunks: std::collections::HashMap<PathBuf, std::collections::HashSet<usize>>,
    /// The commit-message editor.
    editor: Entity<CodeEditor>,
    /// Set by `enter_commit`; `render_commit` consumes it to focus the commit-message input
    /// on the next frame (deferred so the editor element is in the tree first), so opening
    /// the Commit view drops the caret straight into the message box.
    focus_msg: bool,
    /// Changed-files filter (single-line search above the file list).
    search: Entity<CodeEditor>,
    /// True when the changed-files panel is minimized to a thin strip (its `−` button),
    /// giving the side-by-side diff the full width.
    collapsed: bool,
    /// True while a `git commit` is in flight (disables the button, shows "Committing…").
    committing: bool,
}

impl CommitView {
    /// Build the commit-message editor + the changed-files search box (with its live-filter
    /// subscription).
    fn new(cx: &mut Context<Kyde>) -> Self {
        let editor = cx.new(|cx| {
            let mut e = CodeEditor::new(cx, String::new(), Lang::PlainText, "Commit message…");
            e.fill_height = true; // fill the box so the whole area is clickable
            e.soft_wrap = true; // wrap long commit messages instead of running off the box
            e
        });
        let search = cx.new(|cx| CodeEditor::single_line(cx, "Search files…"));
        cx.subscribe(&search, |_this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        Self {
            focus: std::collections::HashSet::new(),
            tree: tree::Tree::default(),
            expanded: std::collections::HashSet::new(),
            checked: std::collections::HashSet::new(),
            excluded_hunks: std::collections::HashMap::new(),
            editor,
            focus_msg: false,
            search,
            collapsed: false,
            committing: false,
        }
    }
}

/// The side-by-side diff panes' state, grouped out of the `Kyde` god-struct: the two
/// editor entities (left = base, read-only; right = working copy, editable + live-saving),
/// the diffed file, the computed diff model + cached syntax spans, and the pane geometry.
/// Shared by the commit view, the history view, the push modal, and the Show-Diff window.
struct DiffPanes {
    /// The computed line/word diff between `base` and the right pane.
    current: Option<FileDiff>,
    /// Syntax spans for the selected file's before/after content (cached on select,
    /// so the diff doesn't re-parse the whole file every render). Empty when the
    /// file's language pack isn't installed.
    old_spans: Vec<highlight::Span>,
    /// See `old_spans`.
    new_spans: Vec<highlight::Span>,
    /// Base (before) pane — read-only, line numbers on the right toward the gutter.
    left: Entity<CodeEditor>,
    /// Working (after) pane — editable; edits debounce into `diff_autosave`.
    right: Entity<CodeEditor>,
    /// The file the panes are showing (`None` disables the autosave — e.g. binary files).
    path: Option<PathBuf>,
    /// Selected file is an image → previewed as an image instead of a text diff. Kept
    /// separate from `path` (which stays `None` for binary files) so the diff autosave
    /// never fires and truncates the image to empty.
    image: Option<PathBuf>,
    /// Read-only diff (push/history views) — suppresses the gutter chevrons + autosave.
    readonly: bool,
    /// Base (HEAD/index) text of the diffed file, kept so we can re-diff live as the
    /// right (working) pane is edited without re-reading git each keystroke.
    base: String,
    /// Shared 2D scroll for BOTH panes (single element each → gpui axis-locks the wheel;
    /// both panes track it → aligned in both axes).
    scroll: ScrollHandle,
    /// Left pane's fraction of the diff island width (the draggable center divider sets it).
    split: f32,
    /// Bumped on every right-pane edit; the debounced autosave only fires when its captured
    /// generation still matches (so we don't spawn `git status` + re-diff per keystroke).
    edit_gen: u64,
}

impl DiffPanes {
    /// Build both diff editors and wire the right (working) pane's debounced autosave:
    /// typing fires `Changed` per keystroke, but the save + `git status` + full re-diff are
    /// expensive (subprocess!), so they only run after the last keystroke settles.
    fn new(cx: &mut Context<Kyde>) -> Self {
        let left = cx.new(|cx| {
            let mut e = CodeEditor::read_only(cx, String::new(), Lang::PlainText);
            e.gutter_right = true;
            e
        });
        let right = cx.new(|cx| CodeEditor::new(cx, String::new(), Lang::PlainText, ""));
        cx.subscribe(&right, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.diff.right.read(cx).dirty {
                this.diff.edit_gen = this.diff.edit_gen.wrapping_add(1);
                let gen = this.diff.edit_gen;
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(app::DIFF_EDIT_DEBOUNCE)
                        .await;
                    this.update(cx, |this, cx| {
                        if this.diff.edit_gen == gen {
                            this.diff_autosave(cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
        })
        .detach();
        Self {
            current: None,
            old_spans: Vec::new(),
            new_spans: Vec::new(),
            left,
            right,
            path: None,
            image: None,
            readonly: false,
            base: String::new(),
            scroll: ScrollHandle::new(),
            split: 0.5,
            edit_gen: 0,
        }
    }
}

/// Which pair of merge sides the resolve window is comparing. `MergeView3` = the default
/// 3-pane merge; the rest are `IntelliJ`'s "Compare Contents" 2-pane modes (Left = yours,
/// Middle = the live result, Right = theirs, Base = the common ancestor).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergeCompare {
    /// The 3-pane merge view (yours | result | theirs).
    MergeView3,
    LeftMiddle,
    RightMiddle,
    LeftRight,
    BaseLeft,
    BaseMiddle,
    BaseRight,
}

impl MergeCompare {
    /// Dropdown order + labels (the 3-pane view first, then the 2-pane pairs).
    pub(crate) const ALL: &'static [(MergeCompare, &'static str)] = &[
        (MergeCompare::MergeView3, "Merge (3-pane)"),
        (MergeCompare::LeftMiddle, "Left and Middle"),
        (MergeCompare::RightMiddle, "Right and Middle"),
        (MergeCompare::LeftRight, "Left and Right"),
        (MergeCompare::BaseLeft, "Base and Left"),
        (MergeCompare::BaseMiddle, "Base and Middle"),
        (MergeCompare::BaseRight, "Base and Right"),
    ];
}

/// Merge-conflict resolution state: the in-progress banner, the conflicts-list stage
/// (files + what each side did), and the 3-pane resolve stage (ours | result | theirs,
/// each its own read-only `CodeEditor` sharing one scroll).
struct MergeView {
    /// `MERGE_HEAD`'s friendly name from the last [`app::RepoSnapshot`] — `Some` = a merge
    /// is in progress (ours or an external `git merge`/`pull`), drives the banner.
    in_progress: Option<String>,
    /// Branch named when the merge was initiated from the branch popup — the label used
    /// before the next snapshot lands (and the friendlier of the two).
    source: Option<String>,
    /// Conflicted files of the in-progress merge + what each side did (the list columns).
    files: Vec<git::ConflictEntry>,
    /// Files already resolved + staged from the window this session.
    resolved: std::collections::HashSet<PathBuf>,
    /// Selected row of the conflicts LIST stage (drives Accept Yours/Theirs/Merge…).
    list_sel: Option<usize>,
    /// Index into `files` of the file open in the RESOLVE stage (`None` = list stage).
    selected: Option<usize>,
    /// The 3-way model for the selected file.
    model: Option<kyde_diff::merge::Merge3>,
    /// Per-chunk resolution state, indexed by chunk (every non-stable chunk consults it —
    /// clean chunks start pending; the "apply non-conflicting" toolbar bulk-applies them).
    res: Vec<kyde_diff::merge::Resolution>,
    /// Each chunk's line range in the CURRENT result text (recomputed with `res` — see
    /// `reload_merge_result`), so the render pass never rebuilds the result.
    res_ranges: Vec<std::ops::Range<usize>>,
    /// Cached index stages of the selected file (1/2/3) — feeds the model, the whole-file
    /// accepts, and the Compare Contents pairs without re-shelling git.
    base_text: String,
    /// See `base_text` (stage 2 — ours).
    ours_text: String,
    /// See `base_text` (stage 3 — theirs).
    theirs_text: String,
    /// How lines are compared (exact / trim / ignore-all-whitespace).
    ws: kyde_diff::merge::WhitespaceMode,
    /// Whether the whitespace dropdown is expanded.
    ws_open: bool,
    /// Which contents the resolve stage is showing (3-pane merge or a 2-pane pair).
    compare: MergeCompare,
    /// Whether the Compare Contents dropdown is expanded.
    compare_open: bool,
    /// Left pane: the current branch's version ("yours").
    ours: Entity<CodeEditor>,
    /// Center pane: the live merge result (rebuilt as conflicts are resolved).
    result: Entity<CodeEditor>,
    /// Right pane: the incoming branch's version ("theirs").
    theirs: Entity<CodeEditor>,
    /// The two panes of a Compare Contents pair (kept separate from the merge panes so
    /// switching back to the 3-pane view never reloads/clobbers them).
    cmp_l: Entity<CodeEditor>,
    /// See `cmp_l`.
    cmp_r: Entity<CodeEditor>,
    /// Shared 2D scroll for the three merge panes (keeps the aligned rows in sync).
    scroll: ScrollHandle,
    /// Shared scroll for the two compare panes.
    cmp_scroll: ScrollHandle,
    /// A merge / abort / commit git op in flight (disables the window's buttons).
    busy: bool,
    /// Transient success note ("Merged X into Y", "Already up to date") — a neutral
    /// banner (the op-error banner is for failures), dismissed by its × button.
    note: Option<String>,
}

impl MergeView {
    fn new(cx: &mut Context<Kyde>) -> Self {
        let mk = |cx: &mut Context<Kyde>| {
            cx.new(|cx| {
                let mut e = CodeEditor::read_only(cx, String::new(), Lang::PlainText);
                e.line_numbers = true;
                e
            })
        };
        Self {
            in_progress: None,
            source: None,
            files: Vec::new(),
            resolved: std::collections::HashSet::new(),
            list_sel: None,
            selected: None,
            model: None,
            res: Vec::new(),
            res_ranges: Vec::new(),
            base_text: String::new(),
            ours_text: String::new(),
            theirs_text: String::new(),
            // Default to ignoring whitespace: formatting-only divergence shouldn't read
            // as a conflict (switchable per-file from the toolbar dropdown).
            ws: kyde_diff::merge::WhitespaceMode::IgnoreAll,
            ws_open: false,
            compare: MergeCompare::MergeView3,
            compare_open: false,
            ours: mk(cx),
            result: mk(cx),
            theirs: mk(cx),
            cmp_l: mk(cx),
            cmp_r: mk(cx),
            scroll: ScrollHandle::new(),
            cmp_scroll: ScrollHandle::new(),
            busy: false,
            note: None,
        }
    }
}

/// Remote-sync state (push/pull/fetch vs `origin`), grouped out of the `Kyde` god-struct:
/// the ahead/behind counts, the in-flight operation flags, and what a push would send.
/// Named `SyncState` (not `Sync`) so it doesn't shadow the `std::marker::Sync` trait.
struct SyncState {
    /// Commits ahead of the push base — the status-bar Push badge. `None` = unborn HEAD.
    ahead: Option<usize>,
    /// Commits behind the upstream — the status-bar Pull badge. `None` = no upstream.
    behind: Option<usize>,
    /// True while a push is in flight (disables the button, shows "Pushing…").
    pushing: bool,
    /// True while a pull is in flight.
    pulling: bool,
    /// True while a fetch is in flight.
    fetching: bool,
    /// Last push/pull failure message (also carried into the op-error banner).
    push_msg: Option<String>,
    /// Files a push would send (the Push tab's list + count badge), kept live by `refresh`.
    push_files: Vec<ChangedFile>,
    /// The revision a push is measured against (see `Repo::push_base`).
    push_base: String,
    /// Selected file index in the Push tab's list.
    push_selected: Option<usize>,
}

impl SyncState {
    fn new() -> Self {
        Self {
            ahead: None,
            behind: None,
            pushing: false,
            pulling: false,
            fetching: false,
            push_msg: None,
            push_files: Vec::new(),
            push_base: String::new(),
            push_selected: None,
        }
    }
}

/// FPS monitor state (toggled from the Kyde menu), grouped out of the `Kyde` god-struct:
/// smoothed frames-per-second + the throttled snapshot the overlay displays.
struct Fps {
    /// Whether the FPS overlay is shown (persisted across launches).
    show: bool,
    /// Smoothed frames-per-second, updated every frame.
    value: f32,
    /// Throttled snapshot of `value` — the number the overlay displays, held steady for a
    /// readable beat rather than re-rendering a blurred number every frame.
    shown: f32,
    /// Time of the previous frame (drives the per-frame delta).
    last: Option<std::time::Instant>,
    /// Throttle timer for the `shown` snapshot (~5/sec).
    file_last: Option<std::time::Instant>,
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
    /// Per-file `(added, removed)` line counts (numstat), refreshed with `files`. Files
    /// absent here (binaries) just render without the count.
    stats: std::collections::HashMap<PathBuf, (usize, usize)>,
    selected: Option<usize>,
    /// Commit view (checkbox tree, message editor, chrome) — grouped into one sub-struct
    /// (see `CommitView`).
    commit: CommitView,
    /// Side-by-side diff panes (both editors, the diff model + cached spans, geometry) —
    /// grouped into one sub-struct (see `DiffPanes`).
    diff: DiffPanes,
    /// The single in-flight divider drag, if any: which divider + the grab offset captured at
    /// mouse-down (cursor coord minus the divider's position). One field for every divider —
    /// see `Divider` and `Kyde::drag_divider`.
    divider_drag: Option<(Divider, f32)>,
    /// Active scrollbar-thumb drag (which scroll handle, axis, grab cursor, grab offset).
    /// Carries the `ScrollHandle` so the shared scrollbar works on any scrollable view.
    sb_drag: Option<SbDrag>,
    /// Per-view snapshot of the dims the scrollbars were last drawn with, so a layout change
    /// schedules exactly one follow-up frame (scroll metrics are only known *after* a paint, so
    /// the first frame after open/resize is stale). Keyed by view; converges, no redraw loop.
    scroll_dims: std::collections::HashMap<SbView, ScrollDims>,
    /// One-shot: has the Projects search box been auto-focused since the landing appeared?
    /// Reset while a project is open, so returning to the landing re-focuses search.
    projects_search_focused: bool,

    // Browse mode — folder tree + editor tabs + file editor + markdown split, all grouped
    // into one sub-struct (see `BrowseView`).
    browse: BrowseView,

    // In-editor find / replace bar — all its state grouped into one sub-struct (see `FindBar`).
    // Targets whichever editor was focused when it opened (`find.target`).
    find: FindBar,
    /// FPS monitor state — grouped into one sub-struct (see `Fps`).
    fps: Fps,

    // Overlays
    /// Go-to-File / Find-in-Files / Actions overlay state — grouped into one sub-struct
    /// (see `Finder`).
    finder: Finder,
    /// First-run / reopened keymap-picker overlay — grouped into one sub-struct
    /// (see `Onboarding`).
    onboarding: Onboarding,
    /// Language-pack manager: a native modal window (like Rollback/Push), + its search box.
    plugins_win: Option<gpui::WindowHandle<ModalWindow>>,
    plugins_query: Entity<CodeEditor>,
    /// Font specimen modal window: the bundled families at each weight, large preview lines.
    fonts_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// "Clear Data & Restart" confirmation — a native modal window (native-menu action).
    clear_data_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// "Clear Local History" confirmation — wipes the open project's snapshot store.
    clear_lh_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// Settings — a native modal window with a category sidebar (Appearance/Keymap/…).
    settings_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// Which Settings category the sidebar has selected.
    settings_section: SettingsSection,
    /// Whether the Appearance → Theme select dropdown is expanded.
    settings_theme_open: bool,
    /// Cached `(path, registered family name)` for the open font file's preview.
    font_preview: Option<(PathBuf, SharedString)>,
    /// Frame counter driving the welcome-screen ASCII shimmer (bumped each animation frame).
    welcome_frame: u32,
    /// Contents of `crash.log` if the previous run crashed — drives the report banner.
    pending_crash: Option<String>,
    /// Last failed git operation (commit/push/rollback/branch/checkout/status), surfaced
    /// in a dismissible banner so a silent failure never looks like success. Cleared on
    /// the next successful `refresh`. `None` = no outstanding error.
    op_error: Option<String>,
    /// An operation error raised alongside a `refresh` (rollback/pull/fetch/push/branch),
    /// stashed here instead of `op_error` because `refresh` is now asynchronous: the
    /// background status read clears `op_error` on success, which would wipe a message set
    /// synchronously right after. `apply_snapshot` re-applies this once, after the clear, so
    /// the message survives exactly that refresh cycle (matching the old synchronous order).
    pending_error: Option<String>,
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
    /// Delete-confirmation modal: the (path, `is_dir`) pending deletion.
    delete_target: Option<(PathBuf, bool)>,
    /// New-file / rename modal state + its single-line name input.
    name_prompt: Option<NamePrompt>,
    name_input: Entity<CodeEditor>,

    // Branch switcher (bottom-right status bar + popup)
    /// Current branch name (repo state — read by refresh/history/status bar, hence kept
    /// directly on `Kyde` rather than in `BranchPopup`).
    current_branch: Option<String>,
    /// Branch-switcher popup state (list/remotes/open/query/expanded — see `BranchPopup`).
    branch: BranchPopup,
    /// Worktree-switcher popup state (status-bar chip → dropdown — see `WorktreePopup`).
    worktree: WorktreePopup,
    /// Bumped on every edit; a debounced task only refreshes git status once this
    /// stops changing (so typing stays snappy but status/tab colors catch up).
    refresh_gen: u64,
    /// Remote-sync state (ahead/behind, in-flight push/pull/fetch flags, what a push would
    /// send) — grouped into one sub-struct (see `SyncState`).
    sync: SyncState,
    /// Push confirmation — its own native OS window (`None` closed).
    push_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// Which tab the git (Commit) view is showing — staging changes or pushing commits.
    git_tab: GitTab,

    // Self-update
    /// A newer release found on GitHub (drives the update banner); `None` = up to date / unknown.
    update_available: Option<update::Release>,
    /// True while a download-swap is in flight (disables the button, shows progress).
    updating: bool,

    // History (git log) view — all its state grouped into one sub-struct (see `HistoryView`).
    history: HistoryView,

    /// Merge-conflict resolution — its own native window (`ModalKind::Merge`).
    merge_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// Merge state (in-progress banner + the 3-pane resolve window — see `MergeView`).
    merge: MergeView,

    /// Compare-two-files — its own native window (`ModalKind::Compare`).
    compare_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// Compare state (paths + aligned panes — see `CompareView`).
    compare: CompareView,

    /// Local History — its own native window (`ModalKind::LocalHistory`).
    local_history_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// Local-history state: config + the project store + the window (see `LocalHistoryView`).
    lh: LocalHistoryView,

    // Bottom terminal panel — control state + (feature-gated) PTY tab views, grouped into
    // one sub-struct (see `TermState`).
    term: TermState,
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
    /// 3-pane merge-conflict resolution (yours | result | theirs).
    Merge,
    /// Two-file side-by-side compare with an apply gutter (issue #42).
    Compare,
    /// Per-file local-history timeline + snapshot ↔ current diff (issue #7).
    LocalHistory,
    /// "Clear Local History" confirmation (destructive — wipes the project's store).
    ClearLocalHistory,
}

/// Compare-two-files state (issue #42): the chosen paths, the computed diff, and
/// two read-only aligned panes sharing one scroll — the merge Compare Contents
/// pattern, plus a center gutter that applies hunks either direction.
struct CompareView {
    /// Left file (repo-relative, or absolute for scratches).
    left_path: Option<PathBuf>,
    /// Right file.
    right_path: Option<PathBuf>,
    /// The current diff (left = old, right = new). `None` until both sides load.
    diff: Option<FileDiff>,
    /// Left pane (read-only; applying writes files and reloads).
    left: Entity<CodeEditor>,
    /// Right pane.
    right: Entity<CodeEditor>,
    /// Shared scroll for both panes + the gutter.
    scroll: ScrollHandle,
}

impl CompareView {
    fn new(cx: &mut Context<Kyde>) -> Self {
        let mk = |cx: &mut Context<Kyde>| {
            cx.new(|cx| {
                let mut e = CodeEditor::read_only(cx, String::new(), Lang::PlainText);
                e.line_numbers = true;
                e
            })
        };
        Self {
            left_path: None,
            right_path: None,
            diff: None,
            left: mk(cx),
            right: mk(cx),
            scroll: ScrollHandle::new(),
        }
    }
}

/// Local-history state (issue #7): the persisted config, the open project's snapshot
/// store (shared with background record tasks), the save-burst flush state, and the
/// Local History window (timeline + snapshot ↔ current aligned panes — the compare
/// pattern). See `views/local_history.rs` for all the logic.
struct LocalHistoryView {
    /// Persisted settings (enabled / retention / throttle — `history.json`).
    cfg: kyde_config::history::HistoryCfg,
    /// The open project's snapshot store. `None` = disabled or no project. Shared with
    /// background record tasks (all disk writes happen off the UI thread).
    store: Option<std::sync::Arc<std::sync::Mutex<kyde_local_history::Store>>>,
    /// The project `store` belongs to — a mismatch with `repo_root` triggers a re-open.
    store_root: Option<PathBuf>,
    /// Paths saved since the last flush; snapshotted together when the throttle fires.
    pending: std::collections::HashSet<PathBuf>,
    /// Whether a throttle flush timer is already armed.
    flush_scheduled: bool,
    /// The file the Local History window is showing.
    path: Option<PathBuf>,
    /// Its timeline, newest first.
    events: Vec<kyde_local_history::Event>,
    /// Selected timeline row.
    selected: usize,
    /// Files changed at or since the selected snapshot (distinct paths of
    /// `events[0..=selected]`, sorted) — the bottom "changed files" panel.
    files: Vec<PathBuf>,
    /// `files` as a tree (folders + files, the Browse-tree model).
    files_tree: tree::Tree,
    /// Expanded dirs in the changed-files panel (reset to all-expanded on recompute).
    files_expanded: std::collections::HashSet<PathBuf>,
    /// The changed-files panel's selection — the file the diff panes + restores target.
    /// `None` falls back to the selected event's own file.
    file_selected: Option<PathBuf>,
    /// Snapshot (old) → current (new) diff for the selected row.
    diff: Option<FileDiff>,
    /// Left pane — the snapshot (read-only).
    left: Entity<CodeEditor>,
    /// Right pane — the current file (read-only; restores write the file and re-diff).
    right: Entity<CodeEditor>,
    /// Shared scroll for both panes + the gutter.
    scroll: ScrollHandle,
}

impl LocalHistoryView {
    fn new(cx: &mut Context<Kyde>) -> Self {
        let mk = |cx: &mut Context<Kyde>| {
            cx.new(|cx| {
                let mut e = CodeEditor::read_only(cx, String::new(), Lang::PlainText);
                e.line_numbers = true;
                e
            })
        };
        Self {
            cfg: kyde_config::history::HistoryCfg::load(),
            store: None,
            store_root: None,
            pending: std::collections::HashSet::new(),
            flush_scheduled: false,
            path: None,
            events: Vec::new(),
            selected: 0,
            files: Vec::new(),
            files_tree: tree::Tree::default(),
            files_expanded: std::collections::HashSet::new(),
            file_selected: None,
            diff: None,
            left: mk(cx),
            right: mk(cx),
            scroll: ScrollHandle::new(),
        }
    }
}

/// Which category the Settings window's sidebar has selected. Drives `render_settings_body`'s
/// content pane.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsSection {
    Appearance,
    Keymap,
    LanguagePacks,
    LocalHistory,
}

impl SettingsSection {
    /// Sidebar order + labels.
    pub(crate) const ALL: &'static [(SettingsSection, &'static str)] = &[
        (SettingsSection::Appearance, "Appearance"),
        (SettingsSection::Keymap, "Keymap"),
        (SettingsSection::LanguagePacks, "Language Packs"),
        (SettingsSection::LocalHistory, "Local History"),
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
            ModalKind::Merge => k.render_merge_body(kcx),
            ModalKind::Compare => k.render_compare_body(kcx),
            ModalKind::LocalHistory => k.render_local_history_body(kcx),
            ModalKind::ClearLocalHistory => k.render_clear_local_history_body(kcx),
        });
        div()
            .track_focus(&self.focus)
            .key_context("Modal")
            .size_full()
            .bg(theme::get().panel_bg)
            .text_color(theme::get().text)
            .font_family(theme::font::UI_FAMILY)
            .text_size(px(theme::get().ui_font_size))
            // Escape closes (cancels) the window; Enter triggers the confirm-style
            // dialogs' primary action (New Branch / Rollback / Clear Data), IDE
            // default-button style. Non-confirm windows (Settings, Plugins, Merge, …)
            // deliberately ignore Enter.
            .on_key_down(
                cx.listener(move |this, ev: &gpui::KeyDownEvent, window, cx| {
                    match ev.keystroke.key.as_str() {
                        "escape" => window.remove_window(),
                        "enter" if kind == ModalKind::NewBranch => {
                            this.kyde.update(cx, Kyde::do_create_branch);
                        }
                        "enter" if kind == ModalKind::Rollback => {
                            this.kyde.update(cx, Kyde::do_rollback);
                        }
                        "enter" if kind == ModalKind::ClearData => {
                            this.kyde.update(cx, Kyde::do_clear_data);
                        }
                        "enter" if kind == ModalKind::ClearLocalHistory => {
                            this.kyde.update(cx, Kyde::do_clear_local_history);
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

fn status_color(s: FileStatus) -> kyde_color::Color {
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

/// File-type badge for the Browse tree (approximates `IntelliJ`'s icons). Known types get a
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
    let base = std::env::var("XDG_CONFIG_HOME").map_or_else(
        |_| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config"),
        PathBuf::from,
    );
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
        .and_then(|v| v.get(key).and_then(serde_json::Value::as_bool))
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
    std::collections::HashMap<usize, kyde_color::Color>,
    std::collections::HashMap<usize, kyde_color::Color>,
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
            "icons/layers.svg" => include_bytes!("../assets/icons/layers.svg"),
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

/// Register the bundled Inter (UI) + `JetBrains` Mono (code) faces so `font_family` resolves
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
        let loc = info.location().map_or_else(
            || "unknown".into(),
            |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
        );
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(std::string::ToString::to_string)
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
/// instead of the lowercase executable name. Must run before `NSApplication` checks in with
/// `LaunchServices`, so it's called at the very top of `main`. No-op off macOS.
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
    // SAFETY: `app` is the shared NSApplication obtained on the main thread (`mtm` proves it),
    // and `image` is a fully-initialized, objc2-retained NSImage (the `else` above bailed if
    // init failed). `setApplicationIconImage:` only reads the image and retains it itself, so
    // passing a valid `&NSImage` upholds every precondition of the AppKit call.
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
    // Pick the changed file to open in the diff pane: prefer the curated `color.rs` showcase
    // (the screenshot fixture edits it), then any Rust file, else the first change.
    fn diff_pick(view: &Kyde) -> Option<usize> {
        view.files
            .iter()
            .position(|f| f.path.ends_with("color.rs"))
            .or_else(|| {
                view.files
                    .iter()
                    .position(|f| f.path.extension().and_then(|e| e.to_str()) == Some("rs"))
            })
            .or(if view.files.is_empty() { None } else { Some(0) })
    }
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
            // Click a changed file so the main content shows a side-by-side diff.
            if let Some(i) = diff_pick(view) {
                view.select_with(i, Some(cx));
            }
        }
        // Same side-by-side diff as `git-diff`, but with the "Kyde Light" palette applied first
        // so the README can show the light theme. Switches the live theme before any render.
        "light" => {
            // Ephemeral (no save): the shots share one throwaway config dir, so persisting the
            // light palette here would make every later (dark) shot launch in light mode.
            theme::set_palette_ephemeral(theme::Theme::light());
            set_packs(view, &["rust"]);
            if let Ok(repo) = std::env::var("KYDE_SHOT_REPO") {
                view.open_project(PathBuf::from(repo), cx);
            }
            view.enter_commit(cx);
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
            view.finder.query.update(cx, |e, cx| {
                e.set_content("render".to_string(), Lang::PlainText, cx);
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
            view.finder.query.update(cx, |e, cx| {
                e.set_content("kyde".to_string(), Lang::PlainText, cx);
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
            view.fps.show = true;
            if let Ok(f) = std::env::var("KYDE_SHOT_FILE") {
                view.open_file(PathBuf::from(f), cx);
            }
            // Negative Y offset = scrolled down into the file.
            view.browse
                .editor_scroll
                .set_offset(gpui::point(px(0.0), px(-600.0 * editor::line_height_px())));
            cx.notify();
        }
        // Browse an invalid JSON file (KYDE_SHOT_FILE) with the JSON pack installed →
        // wavy red squiggles under the parse errors (error highlighting is on by
        // default for installed packs).
        "error-highlight" => {
            set_packs(view, &["json"]);
            if let Ok(f) = std::env::var("KYDE_SHOT_FILE") {
                view.open_file(PathBuf::from(f), cx);
            }
        }
        // Browse a JSON file (KYDE_SHOT_FILE) with a multi-line selection and the editor
        // context menu open → the Sort Lines / Sort Object Keys items above the git commands.
        "sort-menu" => {
            set_packs(view, &["json"]);
            if let Ok(f) = std::env::var("KYDE_SHOT_FILE") {
                view.open_file(PathBuf::from(f), cx);
            }
            view.browse
                .editor
                .update(cx, |e, cx| e.select_range(2..30, cx));
            if let Some(p) = view.browse.open_path.clone() {
                view.open_menu(
                    gpui::point(px(560.0), px(200.0)),
                    MenuTarget::EditorGit(p, true, true),
                    cx,
                );
            }
        }
        // The Local History window for KYDE_SHOT_FILE: seeds two snapshots (so the
        // timeline + snapshot ↔ current diff populate), then opens the window.
        "local-history" => {
            set_packs(view, &["json"]);
            if let Ok(f) = std::env::var("KYDE_SHOT_FILE") {
                let rel = PathBuf::from(f);
                // Open the store synchronously — the async `lh_sync_store` open would
                // race the shot's seeding below.
                if let Some(root) = view.repo_root.clone() {
                    if let Ok(s) = kyde_local_history::Store::for_project(&root) {
                        view.lh.store_root = Some(root);
                        view.lh.store = Some(std::sync::Arc::new(std::sync::Mutex::new(s)));
                    }
                }
                if let (Some(store), Some(root)) = (view.lh.store.clone(), view.repo_root.clone()) {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
                    let current = std::fs::read_to_string(root.join(&rel)).unwrap_or_default();
                    if let Ok(mut s) = store.lock() {
                        let _ = s.record(
                            &rel,
                            &format!("{current}\n\"an: \\\"older\\\" revision\"\n"),
                            kyde_local_history::EventKind::Change,
                            None,
                            now.saturating_sub(3_600_000),
                        );
                        let _ = s.record(
                            &rel,
                            &current,
                            kyde_local_history::EventKind::Label,
                            Some("Before rollback".into()),
                            now.saturating_sub(600_000),
                        );
                    }
                }
                view.open_local_history(rel, cx);
                // Show the older snapshot so the diff has visible hunks.
                view.lh_select(1, cx);
            }
        }
        // The Compare window over two fixture files (KYDE_SHOT_FILE ↔ KYDE_SHOT_FILE_B):
        // side-by-side aligned panes + the center apply gutter.
        "compare" => {
            set_packs(view, &["json"]);
            if let (Ok(a), Ok(b)) = (
                std::env::var("KYDE_SHOT_FILE"),
                std::env::var("KYDE_SHOT_FILE_B"),
            ) {
                view.open_compare(PathBuf::from(a), PathBuf::from(b), cx);
            }
        }
        // Browse a Rust file with ⌘ "held" over an import — the link underlines
        // (issue #26). Uses the debug hover forcer since shots can't hold keys.
        "imports" => {
            set_packs(view, &["rust"]);
            view.open_file(PathBuf::from("src/main.rs"), cx);
            view.browse.editor.update(cx, |e, cx| {
                e.force_link_hover(0);
                cx.notify();
            });
        }
        // The tab strip with more tabs than fit — the active tab must be
        // scrolled into view (strip overflows + scrolls; regression: the bar
        // sized itself to content and no scroll ever applied).
        "tab-scroll" => {
            set_packs(view, &["rust"]);
            for f in [
                "src/main.rs",
                "src/app.rs",
                "src/render.rs",
                "src/divider.rs",
                "src/views/browse.rs",
                "src/views/commit.rs",
                "src/views/diff_view.rs",
                "src/views/history.rs",
                "src/views/merge.rs",
                "src/views/compare.rs",
                "src/views/finder.rs",
                "src/views/tabs.rs",
            ] {
                view.open_file(PathBuf::from(f), cx);
            }
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
            view.term.panel.open = true;
            view.new_terminal_tab(cx);
            if let Some(t) = view.term.tabs.last() {
                t.read(cx).send_input("git status && ls src\n");
            }
            view.focus_active_terminal(window, cx);
            cx.notify();
        }
        // The merge-conflict window. KYDE_SHOT_REPO is a clone with an in-progress
        // conflicted merge (started by screenshots.sh). `merge-conflicts` shows the
        // conflicts-list stage; `merge` drills into the first file's 3-pane resolve
        // view with the non-conflicting changes applied (the toolbar's "All");
        // `merge-compare` (manual, debugging) shows the Left-and-Right compare pair.
        "merge" | "merge-conflicts" | "merge-compare" => {
            set_packs(view, &["rust"]);
            if let Ok(repo) = std::env::var("KYDE_SHOT_REPO") {
                view.open_project(PathBuf::from(repo), cx);
            }
            // The async refresh snapshot hasn't landed yet — seed the branch labels the
            // window's title/headers need (the same data the snapshot would deliver).
            if let Some(r) = view.repo() {
                view.current_branch = r.current_branch();
                view.merge.source = r.merging();
            }
            view.open_merge_window(cx);
            if name != "merge-conflicts" {
                view.select_merge_file(0, cx);
                view.merge_apply_clean(true, true, cx);
            }
            if name == "merge-compare" {
                // KYDE_SHOT_COMPARE picks the pair (debugging): left-middle /
                // right-middle (interactive) or the default read-only left-right.
                let mode = match std::env::var("KYDE_SHOT_COMPARE").as_deref() {
                    Ok("left-middle") => MergeCompare::LeftMiddle,
                    Ok("right-middle") => MergeCompare::RightMiddle,
                    _ => MergeCompare::LeftRight,
                };
                view.merge_set_compare(mode, cx);
                // For the default pair, leave the dropdown open too — this shot doubles
                // as the check that the deferred select panel paints ABOVE the panes.
                view.merge.compare_open = mode == MergeCompare::LeftRight;
            }
        }
        // Branch popup with a row's actions menu open (manual debug shot for the
        // click-a-branch → Checkout/Merge menu flow).
        "branch-menu" => {
            // Seed the current branch (the async refresh snapshot hasn't landed yet), so
            // the picked row — and the menu's "into X" label — match a real click.
            if let Some(r) = view.repo() {
                view.current_branch = r.current_branch();
            }
            view.toggle_branch_popup(window, cx);
            let target = view
                .branch
                .list
                .iter()
                .find(|b| view.current_branch.as_deref() != Some(b.as_str()))
                .cloned();
            if let Some(b) = target {
                view.open_menu(gpui::point(px(860.0), px(420.0)), MenuTarget::Branch(b), cx);
            }
        }
        // Projects landing welcome hero (the animated 3D KYDE logo + shimmer) — used by the
        // README welcome GIF. Force no project open so `render` takes the landing path; the
        // throwaway config has no recents, so it renders the animated hero (not the list).
        "welcome" => {
            view.repo_root = None;
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
        // Blessed expect: opening the one main window can only fail on a fatal startup condition
        // (no display / GPU surface), where panicking with a clear message is the right call.
        #[allow(clippy::expect_used)]
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
                    && !view.onboarding.open
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

    /// Run a git command in `dir`, scrubbing the repo-pointing env (see `boot`).
    fn git(dir: &std::path::Path, args: &[&str]) {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
    }

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
                // Scrub the repo-pointing env git exports to hooks — inherited (e.g. under
                // the pre-push hook) it would redirect these commands to the kyde repo
                // itself instead of the temp dir. Mirrors kyde-git's `git_cmd`.
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
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
                k.branch.query.update(cx, |e, cx| {
                    e.set_content("new branch".into(), Lang::PlainText, cx);
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

    /// Controller state machine: the (now asynchronous) `refresh` must, after the background
    /// snapshot lands, populate the changed-files list, the file tree, the branch, and
    /// auto-select the first change into a diff. Guards the whole `refresh` → `RepoSnapshot`
    /// → `apply_snapshot` → `select` pipeline (`boot` waits for it via `run_until_parked`).
    #[gpui::test]
    fn refresh_populates_changed_files_tree_and_selection(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, _w, _cx| {
                assert!(
                    k.files.iter().any(|f| f.path.ends_with("app.tsx")),
                    "the modified file should appear in the changed-files list"
                );
                assert!(
                    k.files.iter().any(|f| f.path.ends_with("new.txt")),
                    "the untracked file should appear in the changed-files list"
                );
                assert!(
                    k.browse.all_files.iter().any(|p| p.ends_with("app.tsx")),
                    "the file tree should be populated from ls-files"
                );
                assert!(k.current_branch.is_some(), "branch should be read");
                assert!(k.selected.is_some(), "a change should be auto-selected");
                assert!(
                    k.diff.current.is_some(),
                    "the selected change should load a diff model"
                );
                assert!(k.op_error.is_none(), "a clean status read shows no banner");
            })
            .unwrap();
    }

    /// The `pending_error` mechanism: because `refresh` is asynchronous, an operation error
    /// set alongside it (rollback/pull/push/branch) can't be written straight to `op_error`
    /// — the background status read would clear it. Stashing it in `pending_error` must
    /// survive exactly that refresh (re-applied by `apply_snapshot`), then a later clean
    /// refresh clears the banner.
    #[gpui::test]
    fn pending_error_survives_the_refresh_then_clears(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.pending_error = Some("Rollback failed for 1 file(s): x".into());
                k.refresh(cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert_eq!(
                    k.op_error.as_deref(),
                    Some("Rollback failed for 1 file(s): x"),
                    "the stashed error must be shown after the refresh clears the banner"
                );
                assert!(
                    k.pending_error.is_none(),
                    "pending_error is consumed exactly once"
                );
            })
            .unwrap();
        // A subsequent clean refresh clears the transient banner.
        handle.update(cx, |k, _w, cx| k.refresh(cx)).unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(
                    k.op_error.is_none(),
                    "the next successful refresh clears the banner"
                );
            })
            .unwrap();
    }

    /// Tab state machine: opening files appends unique permanent tabs and activates the last;
    /// closing a non-active tab leaves the active one; closing the active tab falls back to a
    /// neighbour; closing the last empties the editor.
    #[gpui::test]
    fn open_and_close_tabs_tracks_active_tab(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.open_file(PathBuf::from("app.tsx"), cx);
                k.open_file(PathBuf::from("new.txt"), cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                assert_eq!(k.browse.open_tabs.len(), 2, "two distinct files → two tabs");
                assert!(
                    k.browse
                        .open_path
                        .as_ref()
                        .is_some_and(|p| p.ends_with("new.txt")),
                    "the last-opened file is active"
                );
                // Close the non-active tab (app.tsx, index 0): active stays new.txt.
                k.close_tab(0, cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                assert_eq!(k.browse.open_tabs.len(), 1);
                assert!(k
                    .browse
                    .open_path
                    .as_ref()
                    .is_some_and(|p| p.ends_with("new.txt")));
                // Close the last (active) tab → editor cleared.
                k.close_tab(0, cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(
                    k.browse.open_tabs.is_empty(),
                    "closing the last tab empties the list"
                );
                assert!(k.browse.open_path.is_none(), "nothing is active");
            })
            .unwrap();
    }

    /// A snapshot landing while the finder is open must recompute its results: `refresh` is
    /// asynchronous, so a finder opened before the background read finishes would otherwise
    /// show empty/stale results (matched against the old `all_files`) until the next
    /// keystroke. Caught live — the go-to-file screenshot state rendered zero rows.
    #[gpui::test]
    fn snapshot_landing_recomputes_open_finder(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                // Simulate "opened before the first snapshot landed": stale-empty file list.
                k.browse.all_files.clear();
                k.finder.open = true;
                k.finder.mode = FinderMode::Files;
                k.finder
                    .query
                    .update(cx, |e, cx| e.set_content("app".into(), Lang::PlainText, cx));
                k.recompute_finder(cx);
                assert!(
                    k.finder.results.is_empty(),
                    "sanity: nothing to match before the snapshot"
                );
                k.refresh(cx);
            })
            .unwrap();
        cx.run_until_parked(); // let the background snapshot land
        handle
            .update(cx, |k, _w, _cx| {
                assert!(
                    k.finder.results.iter().any(|p| p.ends_with("app.tsx")),
                    "the landing snapshot must refresh the open finder's results"
                );
            })
            .unwrap();
    }

    /// IME composition: each marked-text update replaces the PREVIOUS marked region, and the
    /// IME's selection is relative to that region — the selection must be anchored at the
    /// region's START on both ends. (It was anchored at `range.end` on the end, overshooting
    /// by the old marked text's length on every update — walking the selection past the
    /// composed text and, eventually, the buffer.)
    #[gpui::test]
    fn ime_composition_keeps_selection_inside_marked_text(cx: &mut TestAppContext) {
        use gpui::EntityInputHandler;
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, window, cx| {
                k.open_file(PathBuf::from("app.tsx"), cx); // content: "const a = 2;\n"
                k.browse.editor.update(cx, |e, cx| {
                    e.select_range(3..3, cx); // caret inside the buffer
                                              // First composition update: no marked text yet → replaces the caret.
                    e.replace_and_mark_text_in_range(None, "ni", Some(0..2), window, cx);
                    assert_eq!(e.selection(), 3..5, "selection covers the marked text");
                    // Second update replaces the marked region ("ni" → "niho"). Selection
                    // must cover the new marked text — 3..7, not 3..9 (the old overshoot).
                    e.replace_and_mark_text_in_range(None, "niho", Some(0..4), window, cx);
                    assert_eq!(
                        e.selection(),
                        3..7,
                        "selection must be anchored at the marked region's start"
                    );
                    assert!(e.selection().end <= e.text().len());
                });
            })
            .unwrap();
    }

    /// The sort ops (issues #43/#41): Sort Lines rewrites the selected block
    /// alphabetically and keeps it selected; Sort Object Keys rewrites the object
    /// at the caret. Both go through `replace_range_text` (one undo step) and are
    /// gated to Browse with a file open.
    #[gpui::test]
    fn sort_ops_rewrite_selection_and_object(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.open_file(PathBuf::from("app.tsx"), cx); // sets open_path (the op gate)
                                                           // Sort Lines on a plain buffer.
                k.browse.editor.update(cx, |e, cx| {
                    e.set_content("b\nB\na\n".into(), Lang::PlainText, cx);
                    e.select_range(0..6, cx);
                });
                k.sort_selected_lines(cx);
                let ed = k.browse.editor.read(cx);
                assert_eq!(ed.text(), "a\nB\nb\n");
                assert_eq!(ed.selection(), 0..5, "sorted block stays selected");
                // Sort Object Keys at the caret (lang set explicitly — the op must
                // not depend on the machine's installed-plugins config).
                k.browse.editor.update(cx, |e, cx| {
                    e.set_content("{\n  \"b\": 1,\n  \"a\": 2\n}".into(), Lang::Json, cx);
                    e.select_range(1..1, cx);
                });
                k.sort_object_keys_at_caret(cx);
                assert_eq!(
                    k.browse.editor.read(cx).text(),
                    "{\n  \"a\": 2,\n  \"b\": 1\n}"
                );
                // "Sort Lines" with the selection inside a JSON object delegates to
                // the key sort (#43) — a textual line sort would strand the commas.
                k.browse.editor.update(cx, |e, cx| {
                    e.set_content("{\n  \"b\": 1,\n  \"a\": 2\n}".into(), Lang::Json, cx);
                    e.select_range(2..20, cx);
                });
                k.sort_selected_lines(cx);
                assert_eq!(
                    k.browse.editor.read(cx).text(),
                    "{\n  \"a\": 2,\n  \"b\": 1\n}",
                    "line sort inside an object must key-sort, not text-sort"
                );
                // Guard: outside Browse the ops are inert.
                k.mode = Mode::Commit;
                k.browse
                    .editor
                    .update(cx, |e, cx| e.select_range(0..10, cx));
                k.sort_selected_lines(cx);
                assert_eq!(
                    k.browse.editor.read(cx).text(),
                    "{\n  \"a\": 2,\n  \"b\": 1\n}",
                    "no sorting outside Browse"
                );
            })
            .unwrap();
    }

    /// Compare two files (issue #42): `open_compare` diffs them, the gutter's
    /// apply copies a hunk either direction (writing the TARGET file to disk and
    /// re-diffing in place), and the pair converges to zero differences.
    #[gpui::test]
    fn compare_applies_hunks_both_directions(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.join("b.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        handle
            .update(cx, |k, _w, cx| {
                k.open_compare(PathBuf::from("a.txt"), PathBuf::from("b.txt"), cx);
                let d = k.compare.diff.as_ref().unwrap();
                assert_eq!(d.hunks.len(), 2, "two→TWO + the trailing four");
                // « — the LEFT file takes the right side's hunk 0 (two → TWO).
                k.compare_apply_hunk(0, false, cx);
                assert_eq!(
                    std::fs::read_to_string(dir.join("a.txt")).unwrap(),
                    "one\nTWO\nthree\n"
                );
                assert_eq!(
                    k.compare.diff.as_ref().unwrap().hunks.len(),
                    1,
                    "re-diffed after the apply"
                );
                // » — the RIGHT file takes the left side for the remaining hunk
                // (drops its extra "four" line).
                k.compare_apply_hunk(0, true, cx);
                assert_eq!(
                    std::fs::read_to_string(dir.join("b.txt")).unwrap(),
                    "one\nTWO\nthree\n"
                );
                assert!(k.compare.diff.as_ref().unwrap().hunks.is_empty());
            })
            .unwrap();
    }

    /// ⌘-click import navigation (issue #26): a link resolved against the
    /// project file list opens the target; unresolvable specifiers (packages)
    /// are a silent no-op.
    #[gpui::test]
    fn cmd_click_import_opens_the_target_file(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        std::fs::write(dir.join("lib.ts"), "export const a = 1;\n").unwrap();
        std::fs::write(dir.join("main.ts"), "import { a } from './lib';\n").unwrap();
        handle.update(cx, |k, _w, cx| k.refresh(cx)).unwrap();
        cx.run_until_parked(); // let the file-list snapshot land
        handle
            .update(cx, |k, _w, cx| {
                k.open_file(PathBuf::from("main.ts"), cx);
                // Lang set explicitly — must not depend on the machine's installed packs.
                k.browse.editor.update(cx, |e, cx| {
                    e.set_link_navigation(true, cx);
                    e.set_content("import { a } from './lib';\n".into(), Lang::Ts, cx);
                });
                let links = highlight::import_links("import { a } from './lib';\n", Lang::Ts);
                assert_eq!(links.len(), 1);
                k.open_import_link(links[0].clone(), cx);
                assert_eq!(
                    k.browse.open_path,
                    Some(PathBuf::from("lib.ts")),
                    "relative TS import resolves + opens"
                );
                // A bare specifier is an npm package — no-op.
                let npm = highlight::import_links("import r from 'react';\n", Lang::Ts);
                k.open_import_link(npm[0].clone(), cx);
                assert_eq!(k.browse.open_path, Some(PathBuf::from("lib.ts")));
            })
            .unwrap();
    }

    /// ⌘-click on a USE of an imported symbol jumps THROUGH the import: opens
    /// the target file and lands the selection on the definition. The pack for
    /// the target must be installed (effective lang drives the definition scan).
    #[gpui::test]
    fn cmd_click_symbol_jumps_to_its_definition(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        let lib = "const pad = 1;\nexport const answer = 42;\n";
        std::fs::write(dir.join("lib.ts"), lib).unwrap();
        std::fs::write(
            dir.join("main.ts"),
            "import { answer } from './lib';\nconsole.log(answer);\n",
        )
        .unwrap();
        handle.update(cx, |k, _w, cx| k.refresh(cx)).unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                k.plugins.install("typescript"); // effective_lang must see Ts for lib.ts
                k.open_file(PathBuf::from("main.ts"), cx);
                let src = "import { answer } from './lib';\nconsole.log(answer);\n";
                k.browse.editor.update(cx, |e, cx| {
                    e.set_link_navigation(true, cx);
                    e.set_content(src.into(), Lang::Ts, cx);
                });
                let binds = highlight::import_bindings(src, Lang::Ts);
                let (name, link) = binds[0].clone();
                assert_eq!(name, "answer");
                k.open_import_symbol(link, name, cx);
                assert_eq!(k.browse.open_path, Some(PathBuf::from("lib.ts")));
                let sel = k.browse.editor.read(cx).selection();
                assert_eq!(&lib[sel], "answer", "selection lands on the definition");
            })
            .unwrap();
    }

    /// ⌘-click on a METHOD of an imported class resolves through the
    /// external-defs index: the imported file's definitions are indexed in the
    /// background, and clicking the method name opens that file at the method.
    #[gpui::test]
    fn cmd_click_method_resolves_via_imported_file(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        let lib = "export class Cat {\n  meow() { return 1; }\n}\n";
        std::fs::write(dir.join("lib.ts"), lib).unwrap();
        let main = "import { Cat } from './lib';\nnew Cat().meow();\n";
        std::fs::write(dir.join("main.ts"), main).unwrap();
        handle.update(cx, |k, _w, cx| k.refresh(cx)).unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                k.plugins.install("typescript");
                k.open_file(PathBuf::from("main.ts"), cx);
                k.browse.editor.update(cx, |e, cx| {
                    e.set_link_navigation(true, cx);
                });
                // open_file may have skipped the index (pack installed just
                // now) — force a fresh compute with links enabled.
                k.browse.ext_defs_targets.clear();
                k.refresh_external_defs(cx);
            })
            .unwrap();
        cx.run_until_parked(); // background index lands
        handle
            .update(cx, |k, _w, cx| {
                // The editor's external index must now know `meow` → lib.ts.
                let (path, range) = k
                    .browse
                    .editor
                    .read(cx)
                    .external_def_for("meow")
                    .expect("method of the imported class is indexed");
                assert_eq!(path, PathBuf::from("lib.ts"));
                k.open_definition_at(path, range, cx);
                assert_eq!(k.browse.open_path, Some(PathBuf::from("lib.ts")));
                let sel = k.browse.editor.read(cx).selection();
                assert_eq!(&lib[sel], "meow", "selection lands on the method");
            })
            .unwrap();
    }

    /// ⌘⌥←/⌘⌥→ file navigation: visits record, back/forward walk them without
    /// recording, and opening a new file after going back discards the forward
    /// branch.
    #[gpui::test]
    fn nav_back_and_forward_walk_the_visit_history(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        for n in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.join(n), n).unwrap();
        }
        handle.update(cx, |k, _w, cx| k.refresh(cx)).unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                for n in ["a.txt", "b.txt", "c.txt"] {
                    k.open_file(PathBuf::from(n), cx);
                }
                k.nav_back(cx);
                assert_eq!(k.browse.open_path, Some(PathBuf::from("b.txt")));
                k.nav_back(cx);
                assert_eq!(k.browse.open_path, Some(PathBuf::from("a.txt")));
                k.nav_back(cx); // start of history — no-op
                assert_eq!(k.browse.open_path, Some(PathBuf::from("a.txt")));
                k.nav_forward(cx);
                assert_eq!(k.browse.open_path, Some(PathBuf::from("b.txt")));
                // New open from the middle discards the forward branch (c.txt).
                k.open_file(PathBuf::from("app.tsx"), cx);
                k.nav_forward(cx); // nothing forward anymore
                assert_eq!(k.browse.open_path, Some(PathBuf::from("app.tsx")));
                k.nav_back(cx);
                assert_eq!(k.browse.open_path, Some(PathBuf::from("b.txt")));
                assert_eq!(k.browse.nav_history.len(), 3, "a, b, app.tsx (c discarded)");
            })
            .unwrap();
    }

    /// Window-refocus reload: a file changed on disk by an external tool must land in the
    /// open editor when `reload_external` runs (the activation observer's callback), as
    /// long as the buffer has no unsaved edits.
    #[gpui::test]
    fn reload_external_picks_up_disk_changes(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.open_file(PathBuf::from("app.tsx"), cx);
                assert_eq!(k.browse.editor.read(cx).text(), "const a = 2;\n");
            })
            .unwrap();
        cx.run_until_parked();
        // Simulate an external editor rewriting the file while Kyde is in the background.
        std::fs::write(dir.join("app.tsx"), "const a = 99; // external\n").unwrap();
        handle
            .update(cx, |k, _w, cx| k.reload_external(cx))
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                assert_eq!(
                    k.browse.editor.read(cx).text(),
                    "const a = 99; // external\n",
                    "refocus must reload the externally-changed file"
                );
            })
            .unwrap();
    }

    /// Window-refocus reload, Commit view: the side-by-side panes show a file's working
    /// copy, and `refresh` deliberately never touches the pane editors (cx=None re-select)
    /// — so an explicit refocus must reload them, or external edits stay invisible (and an
    /// editable stale right pane could then clobber them on the next keystroke).
    #[gpui::test]
    fn reload_external_refreshes_commit_diff_panes(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.enter_commit(cx);
                // Select the modified file WITH a context so the panes actually load.
                let i = k
                    .files
                    .iter()
                    .position(|f| f.path.ends_with("app.tsx"))
                    .expect("app.tsx is a change");
                k.select_with(i, Some(cx));
                assert_eq!(k.diff.right.read(cx).text(), "const a = 2;\n");
            })
            .unwrap();
        cx.run_until_parked();
        // External tool rewrites the file while Kyde is in the background.
        std::fs::write(dir.join("app.tsx"), "const a = 7; // external\n").unwrap();
        handle
            .update(cx, |k, _w, cx| k.reload_external(cx))
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                assert_eq!(
                    k.diff.right.read(cx).text(),
                    "const a = 7; // external\n",
                    "refocus must reload the commit view's working pane"
                );
            })
            .unwrap();
    }

    /// Refocus with a clean tree while a history diff is open (#39): the empty-status
    /// refresh must not clear the shared diff panes back to "Select a file".
    #[gpui::test]
    fn reload_external_keeps_history_diff_open(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        // Clean tree + 2 commits.
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "second"]);
        handle
            .update(cx, |k, _w, cx| {
                k.history.compare = CompareMode::Before;
                k.enter_history(cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(k.diff.current.is_some(), "sanity: history diff loaded");
                assert_eq!(k.history.file_selected, Some(0));
            })
            .unwrap();
        // Refocus = the activation observer's callback.
        handle
            .update(cx, |k, _w, cx| k.reload_external(cx))
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(
                    k.diff.current.is_some(),
                    "refocus must not blank the open history diff"
                );
                assert!(k.diff.path.is_some());
                assert_eq!(k.history.file_selected, Some(0));
            })
            .unwrap();
    }

    /// Refocus with a dirty tree while a history diff is open: the refresh must not
    /// overwrite the committed diff model with the working-tree diff.
    #[gpui::test]
    fn reload_external_keeps_history_diff_model(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "second"]);
        std::fs::write(dir.join("app.tsx"), "const a = 3;\n").unwrap(); // dirty again
        handle
            .update(cx, |k, _w, cx| {
                k.history.compare = CompareMode::Before;
                k.enter_history(cx);
            })
            .unwrap();
        cx.run_until_parked();
        let new_side = |k: &Kyde| {
            k.diff
                .current
                .as_ref()
                .expect("history diff loaded")
                .new
                .join("\n")
        };
        handle
            .update(cx, |k, _w, _cx| {
                // Newest commit vs parent: the new side is the committed "a = 2".
                assert!(new_side(k).contains("const a = 2;"));
            })
            .unwrap();
        handle
            .update(cx, |k, _w, cx| k.reload_external(cx))
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(
                    new_side(k).contains("const a = 2;"),
                    "refocus overwrote the history diff with the working-tree diff: {:?}",
                    new_side(k)
                );
            })
            .unwrap();
    }

    /// Refocus in History "Local" compare: the right pane shows the working copy, so
    /// external edits must reload it (same never-clobber rules as the Commit pane).
    #[gpui::test]
    fn reload_external_refreshes_history_local_pane(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "second"]);
        std::fs::write(dir.join("app.tsx"), "const a = 3;\n").unwrap(); // dirty again
        handle
            .update(cx, |k, _w, cx| {
                k.history.compare = CompareMode::Local;
                k.enter_history(cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                assert_eq!(k.history.file_selected, Some(0), "sanity: file selected");
                assert_eq!(k.diff.right.read(cx).text(), "const a = 3;\n");
            })
            .unwrap();
        // External tool rewrites the file while Kyde is in the background.
        std::fs::write(dir.join("app.tsx"), "const a = 4;\n").unwrap();
        handle
            .update(cx, |k, _w, cx| k.reload_external(cx))
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                assert_eq!(
                    k.diff.right.read(cx).text(),
                    "const a = 4;\n",
                    "refocus must reload the history Local-compare working pane"
                );
                assert!(k.diff.current.is_some(), "the diff must stay open");
                assert_eq!(k.history.file_selected, Some(0));
            })
            .unwrap();
    }

    /// Autosave state machine: a dirty editor buffer flushes to disk, the buffer is marked
    /// clean, and the changed-files list optimistically shows the file as Modified before
    /// the debounced `git status` lands (so tree/tab colors react instantly).
    #[gpui::test]
    fn autosave_flushes_to_disk_and_marks_modified(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.open_file(PathBuf::from("app.tsx"), cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                // Simulate a real edit: new buffer content + the dirty flag a keystroke sets.
                k.browse.editor.update(cx, |e, cx| {
                    e.set_content("const a = 3;\n".into(), Lang::PlainText, cx);
                    e.dirty = true;
                });
                k.autosave(cx);
                assert!(
                    !k.browse.editor.read(cx).dirty,
                    "a successful autosave marks the buffer clean"
                );
                assert!(
                    k.files.iter().any(|f| f.path.ends_with("app.tsx")
                        && f.status == kyde_git::FileStatus::Modified),
                    "autosave optimistically lists the file as Modified"
                );
            })
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("app.tsx")).unwrap(),
            "const a = 3;\n",
            "the edit must reach disk"
        );
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
        // Simulate the Rollback button, with "delete local copies" UNticked — the untracked
        // file must survive on disk (the pre-checked default is covered by
        // `rollback_deletes_newly_added_files`).
        handle
            .update(cx, |k, _w, cx| {
                k.rollback_delete_added = false;
                k.do_rollback(cx);
            })
            .unwrap();
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

    /// Rolling back a newly `git add`ed file must actually remove it (issue #59): the
    /// delete-local-copies toggle is pre-checked, so the default rollback unstages the
    /// Added file AND deletes it — instead of leaving it untracked (same green, still in
    /// the changes list) which read as "rollback did nothing".
    #[gpui::test]
    fn rollback_deletes_newly_added_files(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        // Stage the new file so it shows as Added, and pick up the new status.
        git(&dir, &["add", "new.txt"]);
        handle.update(cx, |k, _w, cx| k.refresh(cx)).unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                assert!(
                    k.files
                        .iter()
                        .any(|f| f.path.ends_with("new.txt") && f.status == FileStatus::Added),
                    "staged new file should be Added"
                );
                k.open_rollback_path(PathBuf::from("new.txt"), cx);
                assert!(
                    k.rollback_delete_added,
                    "delete-local-copies defaults to checked"
                );
                k.do_rollback(cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(
                    !dir.join("new.txt").exists(),
                    "the added file is deleted from disk"
                );
                assert!(
                    !k.files.iter().any(|f| f.path.ends_with("new.txt")),
                    "the added file left the changes list"
                );
            })
            .unwrap();
    }

    /// A folder (or project-root) scope shows every file's events under it, and the
    /// selected row's own file drives the diff/restore target.
    #[gpui::test]
    fn local_history_folder_scope_lists_files_under_it(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        handle
            .update(cx, |k, _w, cx| {
                lh_test_store(k, &dir);
                let store = k.lh.store.clone().unwrap();
                {
                    use kyde_local_history::EventKind;
                    let mut s = store.lock().unwrap();
                    s.record(
                        std::path::Path::new("app.tsx"),
                        "root",
                        EventKind::Change,
                        None,
                        1_000,
                    )
                    .unwrap();
                    s.record(
                        std::path::Path::new("sub/lib.rs"),
                        "nested",
                        EventKind::Change,
                        None,
                        2_000,
                    )
                    .unwrap();
                }
                // Folder scope: only the file under `sub` shows.
                k.lh.path = Some(PathBuf::from("sub"));
                k.lh.selected = 0;
                k.lh_reload(cx);
                assert_eq!(k.lh.events.len(), 1);
                assert_eq!(k.lh.events[0].path, PathBuf::from("sub/lib.rs"));
                // Project-root scope: everything shows, newest first.
                k.lh.path = Some(PathBuf::new());
                k.lh_reload(cx);
                assert_eq!(k.lh.events.len(), 2);
                assert_eq!(k.lh.events[0].path, PathBuf::from("sub/lib.rs"));
                assert_eq!(k.lh.events[1].path, PathBuf::from("app.tsx"));
            })
            .unwrap();
    }

    /// The Local History changed-files panel + its reverts (issue #7 follow-up): a
    /// selected snapshot lists every file changed at-or-since it; right-click reverts
    /// JUST that file (or folder), the timeline row's menu reverts them all; a file
    /// first seen after the snapshot has no base (nothing safe to restore); and every
    /// revert records a "Before revert" label that shows in the reloaded timeline.
    #[gpui::test]
    fn local_history_changed_since_panel_and_reverts(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("app.tsx"), "now-a\n").unwrap();
        std::fs::write(dir.join("sub/lib.rs"), "now-l\n").unwrap();
        handle
            .update(cx, |k, _w, cx| {
                lh_test_store(k, &dir);
                let store = k.lh.store.clone().unwrap();
                {
                    use kyde_local_history::EventKind;
                    let mut s = store.lock().unwrap();
                    let rec = |s: &mut kyde_local_history::Store, p: &str, c: &str, ts: u64| {
                        s.record(std::path::Path::new(p), c, EventKind::Change, None, ts)
                            .unwrap();
                    };
                    rec(&mut s, "app.tsx", "a1\n", 1_000);
                    rec(&mut s, "sub/lib.rs", "l1\n", 1_500);
                    rec(&mut s, "app.tsx", "a2\n", 2_000);
                    rec(&mut s, "sub/lib.rs", "l2\n", 2_500);
                }
                k.lh.path = Some(PathBuf::new()); // whole-project scope
                k.lh.selected = 0;
                k.lh_reload(cx);
                assert_eq!(k.lh.events.len(), 4);
                let row = |k: &Kyde, p: &str, ts: u64| {
                    k.lh.events
                        .iter()
                        .position(|e| e.path == std::path::Path::new(p) && e.ts_ms == ts)
                        .unwrap()
                };

                // Oldest snapshot selected → both files changed since; lib.rs was first
                // seen AFTER it, so it has no base to revert to.
                let i = row(k, "app.tsx", 1_000);
                k.lh_select(i, cx);
                assert_eq!(
                    k.lh.files,
                    vec![PathBuf::from("app.tsx"), PathBuf::from("sub/lib.rs")]
                );
                assert!(k.lh_has_base_under(std::path::Path::new("app.tsx")));
                assert!(!k.lh_has_base_under(std::path::Path::new("sub/lib.rs")));
                assert!(!k.lh_has_base_under(std::path::Path::new("sub")));

                // Right-click a FILE row → revert just that file: app.tsx returns to its
                // state at the selected point, lib.rs is untouched, and the timeline
                // immediately shows the "Before revert" marker.
                let i = row(k, "sub/lib.rs", 1_500);
                k.lh_select(i, cx);
                k.lh_revert_path(PathBuf::from("app.tsx"), cx);
                assert_eq!(
                    std::fs::read_to_string(dir.join("app.tsx")).unwrap(),
                    "a1\n"
                );
                assert_eq!(
                    std::fs::read_to_string(dir.join("sub/lib.rs")).unwrap(),
                    "now-l\n"
                );
                use kyde_local_history::EventKind;
                assert_eq!(k.lh.events[0].kind, EventKind::Label);
                assert_eq!(k.lh.events[0].label.as_deref(), Some("Before revert"));
                assert_eq!(k.lh.events[0].path, PathBuf::from("app.tsx"));

                // Right-click a FOLDER row → revert just that folder's files.
                let i = row(k, "sub/lib.rs", 1_500);
                k.lh_select(i, cx);
                k.lh_revert_path(PathBuf::from("sub"), cx);
                assert_eq!(
                    std::fs::read_to_string(dir.join("sub/lib.rs")).unwrap(),
                    "l1\n"
                );
                assert_eq!(
                    std::fs::read_to_string(dir.join("app.tsx")).unwrap(),
                    "a1\n"
                );

                // Timeline right-click → Revert This Change and After: every changed-
                // since file returns to its state at the selected snapshot.
                std::fs::write(dir.join("app.tsx"), "zz\n").unwrap();
                std::fs::write(dir.join("sub/lib.rs"), "zz\n").unwrap();
                let i = row(k, "sub/lib.rs", 1_500);
                k.lh_select(i, cx);
                k.lh_revert_since(cx);
                assert_eq!(
                    std::fs::read_to_string(dir.join("app.tsx")).unwrap(),
                    "a1\n"
                );
                assert_eq!(
                    std::fs::read_to_string(dir.join("sub/lib.rs")).unwrap(),
                    "l1\n"
                );
                // The pre-revert "zz" states were labeled — recoverable like anything else.
                let labels =
                    k.lh.events
                        .iter()
                        .filter(|e| e.label.as_deref() == Some("Before revert"))
                        .count();
                assert!(labels >= 4, "every revert target got a label, got {labels}");
            })
            .unwrap();
        cx.run_until_parked();
    }

    /// Clear Local History: the action opens a native confirmation window; confirming
    /// wipes the project's store (journal + blobs) and empties any open timeline.
    #[gpui::test]
    fn local_history_clear_confirms_and_wipes(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                lh_test_store(k, &dir);
                let store = k.lh.store.clone().unwrap();
                {
                    use kyde_local_history::EventKind;
                    let mut s = store.lock().unwrap();
                    s.record(
                        std::path::Path::new("app.tsx"),
                        "v1",
                        EventKind::Change,
                        None,
                        1_000,
                    )
                    .unwrap();
                }
                k.lh.path = Some(PathBuf::from("app.tsx"));
                k.lh_reload(cx);
                assert_eq!(k.lh.events.len(), 1);
                k.open_clear_local_history(cx);
            })
            .unwrap();
        cx.run_until_parked(); // the native window opens on a spawned task
        handle
            .update(cx, |k, _w, cx| {
                assert!(k.clear_lh_win.is_some(), "confirmation window opens");
                k.do_clear_local_history(cx);
                assert!(k.lh.events.is_empty(), "any open timeline empties");
                let store = k.lh.store.clone().unwrap();
                assert_eq!(store.lock().unwrap().event_count(), 0, "store wiped");
            })
            .unwrap();
    }

    /// A tracked file deleted from the working tree must leave the Browse tree (and so
    /// ⌘P): `git ls-files` still lists it, but showing a nonexistent file reads as a
    /// bug. The deletion itself still shows in the changed-files list.
    #[gpui::test]
    fn deleted_files_leave_the_browse_tree(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, _w, _cx| {
                assert!(k.browse.all_files.contains(&PathBuf::from("app.tsx")));
            })
            .unwrap();
        std::fs::remove_file(dir.join("app.tsx")).unwrap();
        handle.update(cx, |k, _w, cx| k.refresh(cx)).unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(
                    !k.browse.all_files.contains(&PathBuf::from("app.tsx")),
                    "the deleted file left the Browse tree"
                );
                assert!(
                    k.files
                        .iter()
                        .any(|f| f.path.as_os_str() == "app.tsx"
                            && f.status == FileStatus::Deleted),
                    "the deletion still shows as a change"
                );
            })
            .unwrap();
    }

    /// The Delete confirmation dialog answers the keyboard: Enter = the primary
    /// (destructive) action, Escape = cancel — IDE default-button behavior.
    #[gpui::test]
    fn delete_dialog_enter_confirms_and_escape_cancels(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, w, cx| {
                // Escape cancels — the file survives.
                k.open_delete(dir.join("new.txt"), cx);
                assert!(k.delete_target.is_some());
                k.act_escape(&EscapeKey, w, cx);
                assert!(k.delete_target.is_none(), "Escape cancels the dialog");
                assert!(dir.join("new.txt").exists(), "cancel leaves the file");
                // Enter confirms — the file is deleted.
                k.open_delete(dir.join("new.txt"), cx);
                k.act_confirm(&ConfirmKey, w, cx);
                assert!(k.delete_target.is_none(), "confirm consumes the dialog");
                assert!(!dir.join("new.txt").exists(), "Enter deletes the file");
                // No dialog open → Enter is a no-op (doesn't panic, deletes nothing).
                k.act_confirm(&ConfirmKey, w, cx);
                assert!(dir.join("app.tsx").exists());
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
                assert!(k.rollback_win.is_some(), "rollback window should be open");
            })
            .unwrap();
    }

    /// Point `k` at a private local-history store inside the smoke repo (never the real
    /// XDG data dir) with a fixed enabled config — tests must not depend on, or pollute,
    /// the developer's own history/config.
    fn lh_test_store(k: &mut Kyde, dir: &std::path::Path) {
        k.lh.cfg = kyde_config::history::HistoryCfg {
            enabled: true,
            retention_days: 7,
            throttle_secs: 1,
        };
        let store = kyde_local_history::Store::open(dir.join(".lh-test-store")).unwrap();
        k.lh.store = Some(std::sync::Arc::new(std::sync::Mutex::new(store)));
        k.lh.store_root = k.repo_root.clone();
    }

    /// Local history (issue #7), the record pipeline: opening a file records its baseline;
    /// a save marks the path pending and the flush snapshots the on-disk state (deduped —
    /// an unchanged flush adds nothing); the Local History window opens for the file.
    #[gpui::test]
    fn local_history_records_opens_and_saves(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                lh_test_store(k, &dir);
                k.open_file(PathBuf::from("app.tsx"), cx);
            })
            .unwrap();
        cx.run_until_parked(); // the background baseline record lands
        handle
            .update(cx, |k, _w, cx| {
                let store = k.lh.store.clone().unwrap();
                {
                    let s = store.lock().unwrap();
                    let ev = s.events_for(std::path::Path::new("app.tsx"));
                    assert_eq!(ev.len(), 1, "opening records the baseline once");
                    assert_eq!(s.content(&ev[0].hash).unwrap(), "const a = 2;\n");
                }
                // A save: the file changes on disk, the save path marks it pending…
                std::fs::write(dir.join("app.tsx"), "const a = 3;\n").unwrap();
                k.lh_note_save(std::path::Path::new("app.tsx"), cx);
                assert!(k.lh.pending.contains(std::path::Path::new("app.tsx")));
                // …and the flush snapshots the final on-disk state.
                k.lh_flush(cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                let store = k.lh.store.clone().unwrap();
                {
                    let s = store.lock().unwrap();
                    let ev = s.events_for(std::path::Path::new("app.tsx"));
                    assert_eq!(ev.len(), 2, "the save burst flushed one snapshot");
                    assert_eq!(s.content(&ev[0].hash).unwrap(), "const a = 3;\n");
                }
                // Re-opening the unchanged file records nothing (content dedup).
                k.open_file(PathBuf::from("app.tsx"), cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                let store = k.lh.store.clone().unwrap();
                assert_eq!(
                    store.lock().unwrap().event_count(),
                    2,
                    "unchanged reopen adds no event"
                );
                // The window opens for the file.
                k.open_local_history(PathBuf::from("app.tsx"), cx);
            })
            .unwrap();
        cx.run_until_parked(); // the native window opens on a spawned task
        handle
            .update(cx, |k, _w, _cx| {
                assert!(k.local_history_win.is_some(), "Local History window opens");
                assert_eq!(k.lh.events.len(), 2, "the timeline shows both snapshots");
            })
            .unwrap();
    }

    /// Local history revert: selecting an older snapshot and reverting rewrites the file
    /// on disk, stamps a "Before revert" label (the pre-revert state stays recoverable),
    /// and reloads the timeline.
    #[gpui::test]
    fn local_history_revert_restores_the_snapshot(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                lh_test_store(k, &dir);
                // Seed an older snapshot, then the current state.
                let store = k.lh.store.clone().unwrap();
                {
                    let mut s = store.lock().unwrap();
                    use kyde_local_history::EventKind;
                    s.record(
                        std::path::Path::new("app.tsx"),
                        "const a = 1;\n",
                        EventKind::Change,
                        None,
                        1_000,
                    )
                    .unwrap();
                    s.record(
                        std::path::Path::new("app.tsx"),
                        "const a = 2;\n",
                        EventKind::Change,
                        None,
                        2_000,
                    )
                    .unwrap();
                }
                k.lh.path = Some(PathBuf::from("app.tsx"));
                k.lh_reload(cx);
                assert_eq!(k.lh.events.len(), 2);
                // Select the OLDER snapshot (rows are newest-first) and revert to it.
                k.lh_select(1, cx);
                k.lh_revert_to_selected(cx);
            })
            .unwrap();
        cx.run_until_parked(); // background "Before revert" record + refresh land
        handle
            .update(cx, |k, _w, _cx| {
                assert_eq!(
                    std::fs::read_to_string(dir.join("app.tsx")).unwrap(),
                    "const a = 1;\n",
                    "the file becomes the selected snapshot"
                );
                let store = k.lh.store.clone().unwrap();
                let s = store.lock().unwrap();
                let ev = s.events_for(std::path::Path::new("app.tsx"));
                assert!(
                    ev.iter()
                        .any(|e| e.label.as_deref() == Some("Before revert")),
                    "the pre-revert state is stamped into the timeline"
                );
            })
            .unwrap();
    }

    /// The master switch: with local history disabled nothing is recorded — no store is
    /// opened, and every note/flush call is a no-op.
    #[gpui::test]
    fn local_history_disabled_records_nothing(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.lh.cfg = kyde_config::history::HistoryCfg {
                    enabled: false,
                    retention_days: 7,
                    throttle_secs: 1,
                };
                k.lh_sync_store(cx);
                assert!(k.lh.store.is_none(), "disabled → no store");
                k.open_file(PathBuf::from("app.tsx"), cx);
                k.lh_note_save(std::path::Path::new("app.tsx"), cx);
                assert!(k.lh.pending.is_empty(), "disabled → nothing pending");
                assert!(!k.lh.flush_scheduled);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(k.lh.store.is_none(), "still no store after settling");
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
                let ed = k.browse.editor.read(cx);
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
        handle.update(cx, |k, _w, _cx| k.fps.show = true).unwrap();
        settle(cx);

        // Browse with a file open in the editor.
        handle
            .update(cx, |k, _w, cx| {
                k.open_file(std::path::PathBuf::from("app.tsx"), cx);
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
            .update(cx, |k, _w, _cx| k.finder.open = false)
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
        handle.update(cx, super::Kyde::toggle_branch_popup).unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, _cx| k.branch.popup_open = false)
            .unwrap();

        // Worktree popup (seed a fake linked worktree so the rows render; the badge
        // gather runs its background statuses harmlessly against these paths).
        handle
            .update(cx, |k, w, cx| {
                k.worktree.list = vec![
                    git::Worktree {
                        path: dir.clone(),
                        branch: Some("main".into()),
                        head: "0000000000000000000000000000000000000000".into(),
                        is_main: true,
                    },
                    git::Worktree {
                        path: dir.join("linked-wt"),
                        branch: Some("agent/task".into()),
                        head: "1111111111111111111111111111111111111111".into(),
                        is_main: false,
                    },
                ];
                k.toggle_worktree_popup(w, cx);
            })
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, _cx| k.worktree.popup_open = false)
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
            .update(cx, |k, _w, _cx| k.onboarding.open = true)
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, _cx| k.onboarding.open = false)
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
                k.close_modal_window(ModalKind::ClearData, cx);
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
                k.close_modal_window(ModalKind::Rollback, cx);
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

    /// ⌃` opens the panel + spawns the first tab, and ⌘W (``act_close_terminal_tab``) closes the
    /// active tab — the last one hides the panel. Exercises the real gpui wiring (window +
    /// PTY-backed ``TerminalView``s + the ``TermPanel`` state), not just the pure state machine.
    #[cfg(feature = "terminal")]
    #[gpui::test]
    fn terminal_toggle_opens_and_cmd_w_closes(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, window, cx| {
                k.act_toggle_terminal(&ToggleTerminal, window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(k.term.panel.open, "⌃` opens the panel");
                assert_eq!(k.term.tabs.len(), 1, "first tab spawned on open");
            })
            .unwrap();
        // A second tab, then ⌘W closes the active one → one remains, panel stays open.
        handle
            .update(cx, |k, _w, cx| k.new_terminal_tab(cx))
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, window, cx| {
                assert_eq!(k.term.tabs.len(), 2);
                k.act_close_terminal_tab(&CloseTerminalTab, window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert_eq!(k.term.tabs.len(), 1, "⌘W closes the active tab");
                assert!(k.term.panel.open, "panel stays open while a tab remains");
            })
            .unwrap();
        // ⌘W the last tab → the panel hides.
        handle
            .update(cx, |k, window, cx| {
                k.act_close_terminal_tab(&CloseTerminalTab, window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(k.term.tabs.is_empty());
                assert!(!k.term.panel.open, "closing the last tab hides the panel");
            })
            .unwrap();
    }

    /// The shell exiting (`^D` / `exit`) makes the IO thread emit `CloseRequested`; the Kyde
    /// subscription must close that tab (the "stuck on exited" item). Simulate the event.
    #[cfg(feature = "terminal")]
    #[gpui::test]
    fn terminal_child_exit_closes_its_tab(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.new_terminal_tab(cx);
                k.new_terminal_tab(cx);
            })
            .unwrap();
        cx.run_until_parked();
        let view = handle
            .update(cx, |k, _w, _cx| k.term.tabs[k.term.panel.active].clone())
            .unwrap();
        view.update(cx, |_v, cx| {
            cx.emit(terminal::TerminalEvent::CloseRequested);
        });
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert_eq!(
                    k.term.tabs.len(),
                    1,
                    "child exit (CloseRequested) closes the tab"
                );
            })
            .unwrap();
    }

    /// Switching view (rail folder/git/history icon, or ⌘ shortcut) while the terminal is
    /// maximized must un-maximize it — otherwise the full-column terminal hides the chosen
    /// view and the click looks like a no-op (the original feedback item).
    #[cfg(feature = "terminal")]
    #[gpui::test]
    fn switching_view_unmaximizes_terminal(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, window, cx| {
                k.act_toggle_terminal(&ToggleTerminal, window, cx);
                k.term.panel.maximized = true;
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| k.switch_mode(Mode::Browse, cx))
            .unwrap();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(
                    !k.term.panel.maximized,
                    "switching view un-maximizes the terminal"
                );
                assert!(k.mode == Mode::Browse);
            })
            .unwrap();
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
                    this.finder.open = false;
                    this.onboarding.open = false;
                    this.delete_target = None;
                    window.focus(&this.focus_handle);
                    cx.notify();
                }
            }),
        )
}
