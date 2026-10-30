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
// The Agents view's pure model. Ungated so its tests always run, but only the
// `terminal`-gated view code uses it.
#[cfg(all(feature = "terminal", unix))]
mod agent_cli;
#[cfg(all(feature = "terminal", unix))]
mod agent_host;
#[cfg_attr(not(feature = "terminal"), allow(dead_code))]
mod agents;
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
pub(crate) use divider::{full_island_w, hist_split_w, Divider, DIFF_GUTTER_W, HIST_BRANCHES_W};
// Reusable, app-agnostic UI toolkit (its own crate). Aliased to `ui` (so `ui::tree::item`
// resolves) and glob-re-exported so call sites use `btn_primary`/`file_badge`/… unqualified.
pub(crate) use app::{
    CONTENT_MIN_QUERY, CONTENT_SEARCH_DEBOUNCE, FINDER_RESULT_CAP, SCROLL_CONTEXT_ROWS,
    STATUS_REFRESH_DEBOUNCE,
};
pub(crate) use kyde_ui as ui;
pub(crate) use kyde_ui::{
    badge_inner, btn_primary, btn_primary_state, btn_secondary, btn_secondary_state, checkbox_box,
    file_badge, lerp_rgb, menu_icon, scrollbar_thumb, tab_pill,
};

// ── per-feature views (impl Kyde blocks; see src/views/) ──
mod views;
pub(crate) use views::{nav_keys, MenuEntry, NavTarget};

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
#[cfg(feature = "terminal")]
use platform::alert;
#[cfg(unix)]
use platform::instance;
use platform::{clipboard as os_clipboard, scratch, shellcmd};

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
        TerminalEnter,
        TerminalBackspace,
        TerminalEscape,
        TerminalCopy,
        TerminalPaste,
        DeleteFile,
        CloseTab,
        DiffNextChange,
        DiffPrevChange,
        CopyFiles,
        CutFiles,
        PasteFiles,
        GitPush,
        GitPull
    ]
);
// Context-menu keyboard navigation (fixed keys, context "ContextMenu" — the open panel).
actions!(
    kyde_menu_nav,
    [
        MenuUp,
        MenuDown,
        MenuLeft,
        MenuRight,
        MenuConfirm,
        MenuDismiss
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
// The Agents view's shortcuts (configurable; bound in its "Agents" key context).
actions!(
    kyde_agents,
    [
        AgentNext,
        AgentPrev,
        AgentAttention,
        AgentTabNext,
        AgentTabPrev,
        AgentNewWorktree,
        AgentRename,
        AgentScrollback,
        AgentToggleSidebar
    ]
);

/// Jump to the Nth worktree in the Agents sidebar (⌃1…⌃9). Carries the number, so it's a
/// derived `Action`.
#[derive(Clone, PartialEq, Default, Debug, gpui::Action)]
#[action(namespace = kyde_agents, no_json)]
struct AgentJump(usize);

// Native menu bar actions.
actions!(
    kyde_menu,
    [
        Quit,
        ToggleFps,
        ClearData,
        OpenPlugins,
        OpenProject,
        OpenChangelog
    ]
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
                MenuItem::action("What's New…", OpenChangelog),
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
    // Lists, trees + popup lists: ↑↓←→ ⏎ space ⇧F10 ⎋ (issue #81).
    ui::nav::bind_nav_keys(cx);
    cx.bind_keys([
        KeyBinding::new("up", FinderUp, Some("FileFinder")),
        KeyBinding::new("down", FinderDown, Some("FileFinder")),
        KeyBinding::new("enter", FinderConfirm, Some("FileFinder")),
        KeyBinding::new("escape", FinderClose, Some("FileFinder")),
        KeyBinding::new("up", MenuUp, Some("ContextMenu")),
        KeyBinding::new("down", MenuDown, Some("ContextMenu")),
        KeyBinding::new("left", MenuLeft, Some("ContextMenu")),
        KeyBinding::new("right", MenuRight, Some("ContextMenu")),
        KeyBinding::new("enter", MenuConfirm, Some("ContextMenu")),
        KeyBinding::new("space", MenuConfirm, Some("ContextMenu")),
        KeyBinding::new("escape", MenuDismiss, Some("ContextMenu")),
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
    bind_app(cx, km, "push", GitPush);
    bind_app(cx, km, "pull", GitPull);
    bind_agents(cx, km, "agent_next", AgentNext);
    bind_agents(cx, km, "agent_prev", AgentPrev);
    bind_agents(cx, km, "agent_attention", AgentAttention);
    bind_agents(cx, km, "agent_tab_next", AgentTabNext);
    bind_agents(cx, km, "agent_tab_prev", AgentTabPrev);
    bind_agents(cx, km, "agent_new_worktree", AgentNewWorktree);
    bind_agents(cx, km, "agent_rename", AgentRename);
    bind_agents(cx, km, "agent_scrollback", AgentScrollback);
    bind_agents(cx, km, "agent_toggle_sidebar", AgentToggleSidebar);
    // ⌃1…⌃9: the Nth worktree in the sidebar (fixed keys).
    cx.bind_keys(
        (1..=9).map(|n| KeyBinding::new(&format!("ctrl-{n}"), AgentJump(n), Some("Agents"))),
    );
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
    // File-tree cut/copy/paste (issue #67). Scoped to the "Kyde" root, so the deeper
    // "CodeEditor" copy/cut/paste bindings win whenever an editor is focused — these only
    // fire at the app root (a tree row selected, no editor focused).
    cx.bind_keys([
        KeyBinding::new("cmd-c", CopyFiles, Some("Kyde")),
        KeyBinding::new("cmd-x", CutFiles, Some("Kyde")),
        KeyBinding::new("cmd-v", PasteFiles, Some("Kyde")),
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
        // Enter / backspace / escape are app shortcuts in "Kyde". gpui
        // dispatches binding ACTIONS *before* on_key_down and an action stops propagation by
        // default, so on_key_down never runs once a binding matches — meaning a no-op here would
        // swallow the key (nothing reaches the PTY). So these route to actions whose handlers
        // (on TerminalView) write the raw byte to the shell, exactly like the editor binds
        // backspace to its own buffer action. The "Terminal" context (depth) shadows "Kyde".
        KeyBinding::new("enter", TerminalEnter, Some("Terminal")),
        KeyBinding::new("backspace", TerminalBackspace, Some("Terminal")),
        // ⌘⌫ is DeleteFile in "Kyde" (issue #61); in the terminal it must act as a plain
        // backspace, not delete the tree-selected file out from under a typing user.
        KeyBinding::new("cmd-backspace", TerminalBackspace, Some("Terminal")),
        KeyBinding::new("escape", TerminalEscape, Some("Terminal")),
        // ⌘C / ⌘V copy and paste in the terminal — the root binds them to the file tree's
        // copy / paste, which would swallow them.
        KeyBinding::new("cmd-c", TerminalCopy, Some("Terminal")),
        KeyBinding::new("cmd-v", TerminalPaste, Some("Terminal")),
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

/// Like [`bind_app`], but only while the Agents view has focus (its "Agents" key context).
fn bind_agents<A: gpui::Action>(cx: &mut App, km: &Keymap, name: &str, action: A) {
    if let Some(k) = km.key_for(name) {
        cx.bind_keys([KeyBinding::new(&k, action, Some("Agents"))]);
    }
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
    /// The herdr-style AI workspace: spaces → worktrees → agent terminal tabs.
    Agents,
}

/// Which window hosts the find/replace bar: the main window or one of the native modals.
/// The bar renders only in its host, over the editor it searches.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FindHost {
    Main,
    Modal(ModalKind),
}

/// The two tabs of the git (Commit) view: staging working changes vs pushing committed ones.
#[derive(Clone, Copy, PartialEq)]
enum GitTab {
    Commit,
    Push,
}

/// How the history view diffs the selected commit.
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
    HistoryView,
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
    Changelog,
    Push,
    Pull,
    Fetch,
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
    ("Git History (log)", PaletteAction::HistoryView, ""),
    ("Push…", PaletteAction::Push, "push"),
    ("Pull (Update Project)", PaletteAction::Pull, "pull"),
    ("Fetch", PaletteAction::Fetch, ""),
    ("Rollback changes", PaletteAction::Rollback, ""),
    ("Local History", PaletteAction::LocalHistory, ""),
    ("Clear Local History", PaletteAction::ClearLocalHistory, ""),
    ("Settings / Keymap", PaletteAction::Settings, "open_keymap"),
    ("Manage Plugins", PaletteAction::Plugins, ""),
    ("Preview Fonts", PaletteAction::Fonts, ""),
    ("What's New (Changelog)", PaletteAction::Changelog, ""),
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
    /// A commit row in the History list (by index into `history.commits`) — its menu offers
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
    /// Which flyout submenu is expanded, if any (file/editor menu — New / Git). Only one
    /// opens at a time.
    submenu: Option<Submenu>,
    /// The rows, built when the menu opened (see `views/context_menu.rs`).
    entries: Vec<MenuEntry>,
    /// Highlighted top-level row (keyboard or hover).
    hl: Option<usize>,
    /// Highlighted row inside the open submenu — `Some` once the keyboard is in it.
    sub_hl: Option<usize>,
}

/// A context-menu flyout submenu (the "New ▸" / "Git ▸" rows).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Submenu {
    New,
    Git,
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
    DiffH,
}

/// An open "name this file" prompt (the small modal with a text input).
#[derive(Clone)]
enum NamePrompt {
    /// Create a new file inside this directory (rel path; `""` = repo root).
    NewFile(PathBuf),
    /// Create a new folder inside this directory (rel path; `""` = repo root).
    NewFolder(PathBuf),
    /// Rename this existing file (rel path) to the typed name in its own folder.
    Rename(PathBuf),
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
    /// The commit graph, one row per entry of `commits` (laid out when they load).
    graph: Vec<git::graph::GraphRow>,
    /// Graph column width in lanes — the widest row, capped (see `history::MAX_LANES`).
    graph_lanes: usize,
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
    /// Keyboard highlight in the compare-mode dropdown (index into `CompareMode::ALL`).
    compare_hl: Option<usize>,
    /// The open compare-mode dropdown's focus (it takes ↑↓ ⏎ ⎋).
    compare_focus: FocusHandle,
    /// Branch-picker dropdown open in the history view.
    branch_open: bool,
    /// Keyboard highlight in the branch dropdown (index into `history_rev_items`; reset as
    /// the search changes).
    rev_hl: Option<usize>,
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
    /// Keyboard focus of the commit list (↑↓ walk commits, → / ⏎ into the files pane).
    list_focus: FocusHandle,
    /// Keyboard focus of the changed-files tree (← on a top-level row returns to commits).
    files_focus: FocusHandle,
    /// The files tree's keyboard cursor when it's on a FOLDER (`None` = on the selected file).
    files_dir_cursor: Option<PathBuf>,
    /// Scroll position of the changed-files tree (keeps the keyboard cursor in view).
    files_scroll: ScrollHandle,
    /// Expanded folder keys in the branches sidebar (`branch_rows` keys). Every folder opens
    /// when the branches load, minus the ones in `branches_collapsed`.
    branches_expanded: std::collections::HashSet<String>,
    /// Folders the user collapsed in the branches sidebar — kept across reloads.
    branches_collapsed: std::collections::HashSet<String>,
}

impl HistoryView {
    /// Build the initial history state and wire the three search boxes' live-filter
    /// subscriptions (each repaints `Kyde` when its query changes while the relevant view /
    /// dropdown is active).
    fn new(cx: &mut Context<Kyde>) -> Self {
        let branch_query = cx.new(|cx| CodeEditor::single_line(cx, "Search branches…"));
        cx.subscribe(&branch_query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.history.branch_open {
                this.history.rev_hl = None;
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
            graph: Vec::new(),
            graph_lanes: 0,
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
            compare_hl: None,
            compare_focus: cx.focus_handle(),
            branch_open: false,
            rev_hl: None,
            locals: Vec::new(),
            remotes: Vec::new(),
            branch_query,
            commit_query,
            scroll: ScrollHandle::new(),
            commit_frac: 2.0 / 3.0,
            list_focus: cx.focus_handle(),
            files_focus: cx.focus_handle(),
            files_dir_cursor: None,
            files_scroll: ScrollHandle::new(),
            branches_expanded: std::collections::HashSet::new(),
            branches_collapsed: std::collections::HashSet::new(),
        }
    }
}

/// Settings → Keymap's shortcut editor, grouped out of the `Kyde` god-struct.
struct KeymapEditor {
    /// The action waiting for its new shortcut (`None` = not recording).
    recording: Option<&'static str>,
    /// Captures the next keystroke app-wide while recording; dropping it stops capturing.
    intercept: Option<gpui::Subscription>,
    /// One status line about the last change ("⌘⇧O moved from Go to File", …).
    note: Option<String>,
    /// Filters the shortcut table by action name or keystroke.
    query: Entity<CodeEditor>,
    /// A "Reset to <preset>" waiting for confirmation — nothing changes until it's confirmed.
    confirm: Option<Preset>,
    /// Focused while recording, so modifier presses reach the section's listener.
    focus: FocusHandle,
    /// Modifiers held right now while recording — previewed (translucent) in the row as the
    /// shortcut is built, IDE-style.
    held: gpui::Modifiers,
}

impl KeymapEditor {
    /// A closed editor; the search box repaints the Settings window as it changes.
    fn new(cx: &mut Context<Kyde>) -> Self {
        let query = cx.new(|cx| CodeEditor::single_line(cx, "Search shortcuts…"));
        cx.subscribe(&query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.settings_win.is_some() {
                cx.notify();
            }
        })
        .detach();
        Self {
            recording: None,
            intercept: None,
            note: None,
            query,
            confirm: None,
            focus: cx.focus_handle(),
            held: gpui::Modifiers::default(),
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
    /// Keyboard highlight over [New Branch, Pull, ...tree rows] (`None` until an arrow key;
    /// reset as the search changes).
    hl: Option<usize>,
    /// Scroll position of the popup's list (keeps the highlight in view).
    scroll: ScrollHandle,
}

impl BranchPopup {
    /// Build initial branch-popup state + wire the search box's live-filter subscription
    /// (repaints `Kyde` while the popup is open).
    fn new(cx: &mut Context<Kyde>) -> Self {
        let query = cx.new(|cx| CodeEditor::single_line(cx, "Search / new branch name"));
        cx.subscribe(&query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.branch.popup_open {
                this.branch.hl = None;
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
            hl: None,
            scroll: ScrollHandle::new(),
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
    /// Keyboard highlight (index into `list`).
    hl: Option<usize>,
    /// The open popup's focus — it takes the keyboard (↑↓ ⏎ ⎋).
    focus: FocusHandle,
}

impl WorktreePopup {
    fn new(cx: &mut App) -> Self {
        Self {
            popup_open: false,
            list: Vec::new(),
            counts: std::collections::HashMap::new(),
            counts_gen: 0,
            hl: None,
            focus: cx.focus_handle(),
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
    /// The editor being searched — the focused one when the bar opened (file editor, any
    /// diff/merge/compare/local-history pane). `None` until first opened.
    target: Option<Entity<CodeEditor>>,
    /// The window the bar (and `target`) lives in.
    host: FindHost,
    /// `target`'s `content_gen` at the last scan, so a burst of change events for text
    /// already scanned (Replace All emits one per replacement) re-scans nothing.
    scanned_gen: u64,
    /// Re-scans when `target`'s text changes (a file switch, an edit, a reload).
    target_sub: Option<gpui::Subscription>,
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
            target: None,
            host: FindHost::Main,
            scanned_gen: 0,
            target_sub: None,
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

/// The Agents view's state (spaces → worktrees → tabs; see `agents.rs` for the pure model and
/// `views/agents.rs` for the UI). The persisted layout is `cfg`; the PTY-backed tabs are
/// runtime-only and live behind the `terminal` feature. Global, NOT per project — switching
/// the open project keeps every agent running.
struct AgentsView {
    /// Spaces + their worktrees, start commands, cleanup policy (`agents.json`).
    cfg: kyde_config::agents::AgentsCfg,
    /// The worktree on screen: a space's own folder or one of its linked worktrees.
    selected: Option<PathBuf>,
    /// Checked-out branch per worktree, gathered off the UI thread on entering the view.
    branches: std::collections::HashMap<PathBuf, String>,
    /// Spaces inside a git repo (new worktrees are offered only for these).
    git_spaces: std::collections::HashSet<PathBuf>,
    /// Changed-files / unpushed counts per worktree, refreshed with `branches`.
    git: std::collections::HashMap<PathBuf, agents::WtGit>,
    /// A git refresh is in flight (the periodic one skips rather than piling up).
    git_refreshing: bool,
    /// The sidebar's list: Code (spaces) / Issues / Pull Requests.
    side_tab: SideTab,
    /// Assigned issues / open PRs per project.
    issues: std::collections::HashMap<PathBuf, GhList<kyde_git::github::Issue>>,
    /// Whose issues / which PRs those tabs list, and the search narrowing both.
    issue_filter: kyde_git::github::IssueFilter,
    pr_filter: kyde_git::github::PrFilter,
    gh_query: Entity<CodeEditor>,
    /// Debounce generation for re-searching as you type.
    gh_query_gen: u64,
    /// When each GitHub tab's lists were last (re)fetched — they go stale as PRs merge.
    gh_fetched: std::collections::HashMap<SideTab, std::time::Instant>,
    /// The Issues / Pull Requests filter dropdown.
    gh_filter_sel: ui::SelectState,
    /// The issue / PR the main panel shows on those tabs: (project, tab, number).
    gh_selected: Option<(PathBuf, SideTab, u64)>,
    /// Its full text — `None` while loading, `Err` = gh's reason.
    gh_detail: Option<Result<kyde_git::github::ItemDetail, String>>,
    /// Its body, rendered as markdown.
    gh_body: Entity<mdview::MarkdownView>,
    /// Which part of a PR the detail shows.
    gh_pr_tab: PrTab,
    /// The selected issue / PR's conversation, ready to render.
    gh_cards: Vec<GhCard>,
    /// The comment box under it, and the inline-thread reply box (with the thread it's for).
    gh_comment: Entity<CodeEditor>,
    gh_reply: Entity<CodeEditor>,
    gh_reply_to: Option<u64>,
    /// A comment / reply / close is being sent.
    gh_posting: bool,
    /// Kyde's window is frontmost (alerts fire for on-screen tabs too when it isn't).
    window_active: bool,
    /// When the last alert sound played — a burst of agents finishing plays one.
    #[cfg_attr(not(feature = "terminal"), allow(dead_code))]
    last_sound: Option<std::time::Instant>,

    /// A read of the agents' hook state files is in flight.
    #[cfg_attr(not(feature = "terminal"), allow(dead_code))]
    hooks_polling: bool,
    /// The Claude Code hook was installed / removed per the setting this launch.
    hooks_synced: bool,
    /// The space / worktree / tab being renamed in place, and the name field.
    renaming: Option<RenameTarget>,
    rename_input: Entity<CodeEditor>,
    /// The sidebar is hidden (just the agent on screen).
    sidebar_hidden: bool,
    /// Launch reattached the agent hosts still running (autostart waits for that).
    hosts_restored: bool,
    /// A scan for agent hosts to adopt is in flight.
    #[cfg_attr(not(all(feature = "terminal", unix)), allow(dead_code))]
    hosts_scanning: bool,
    /// Hosts whose tab was closed (killed) — not to be adopted again while they wind down.
    #[cfg_attr(not(feature = "terminal"), allow(dead_code))]
    closed_hosts: std::collections::HashSet<String>,
    /// The scrollback window's editor.
    scrollback: Entity<CodeEditor>,
    /// The sidebar row under the mouse: only it shows its action / remove button (which then
    /// takes room, pushing the git badges left).
    hovered_row: Option<SharedString>,
    /// That PR's diff, row by row — `None` while loading, `Err` = gh's reason.
    gh_diff: Option<Result<std::sync::Arc<Vec<kyde_git::github::DiffRow>>, String>>,
    prs: std::collections::HashMap<PathBuf, GhList<kyde_git::github::PullRequest>>,
    /// What each worktree's agent session is about (Claude Code's own title + prompts).
    contexts: std::collections::HashMap<PathBuf, agents::AgentContext>,
    /// The context panel beside the terminal is open.
    context_open: bool,
    /// The worktree the "Remove worktree?" window is asking about: (space, worktree).
    removing: Option<(PathBuf, PathBuf)>,
    /// Worktrees being removed in the background — hidden from the sidebar meanwhile.
    hiding: std::collections::HashSet<PathBuf>,
    /// Worktrees whose tabs were opened this launch (their saved tabs are now the live ones).
    #[cfg(feature = "terminal")]
    restored: std::collections::HashSet<PathBuf>,
    /// Each space's own checkout vs its remote mainline, from the last background sync.
    main_sync: std::collections::HashMap<PathBuf, agents::MainSync>,
    /// Per space: when its merged PRs were last fetched, and their head branches.
    #[cfg(feature = "terminal")]
    merged:
        std::collections::HashMap<PathBuf, (std::time::Instant, std::collections::HashSet<String>)>,
    /// A cleanup pass is gathering facts in the background.
    #[cfg(feature = "terminal")]
    sweeping: bool,
    /// The Clean Up window's ticked worktrees.
    #[cfg(feature = "terminal")]
    cleanup_checked: std::collections::HashSet<PathBuf>,
    /// A main sync is in flight / when the last one finished (throttle).
    main_syncing: bool,
    main_synced_at: Option<std::time::Instant>,
    /// Width (px) of the spaces sidebar, drag-resizable.
    sidebar_w: f32,
    /// Live tabs per worktree path.
    #[cfg(feature = "terminal")]
    tabs: std::collections::HashMap<PathBuf, AgentTabs>,
    /// Worktrees being checked out — each a pending row, and a setup screen when selected.
    creating: Vec<Creating>,
    /// A project / worktree row being dragged to a new place in the sidebar.
    drag: Option<SideDrag>,
    /// The issue / review / re-review prompt templates (Settings → Agents).
    issue_prompt_input: Entity<CodeEditor>,
    review_prompt_input: Entity<CodeEditor>,
    rereview_prompt_input: Entity<CodeEditor>,
    /// One-line status under the sidebar (cleanup result, git error, …).
    note: Option<String>,
    /// The start command input (Settings → Agents, and the first-run setup overlay).
    default_cmd_input: Entity<CodeEditor>,
    /// The setup overlay has taken keyboard focus (once per showing).
    setup_focused: bool,
    /// Debounce generation for persisting `cfg` while typing.
    save_gen: u64,
    /// The 1 s status/cleanup ticker is running (stops itself once no tabs remain).
    #[cfg(feature = "terminal")]
    ticking: bool,
    /// Focus the visible agent tab on the next paint (set where no `Window` is at hand).
    focus_pending: bool,
    /// This launch already auto-started the selected worktree's agent (once — closing its
    /// tabs later must not respawn them on every visit).
    autostarted: bool,
    /// `contexts` has been read at least once — so auto-start knows each worktree's latest
    /// Claude session and can resume it.
    contexts_loaded: bool,
}

/// Which list the Agents sidebar shows.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum SideTab {
    /// Spaces → worktrees (the agents).
    Code,
    /// GitHub issues assigned to you, for the chosen project.
    Issues,
    /// Open pull requests, for the chosen project.
    Prs,
}

/// A GitHub list for one project, fetched with `gh` in the background.
enum GhList<T> {
    Loading,
    Ready(Vec<T>),
    /// `gh`'s reason (not logged in, issues disabled, not a GitHub repo…).
    Failed(String),
}

/// What a new worktree is checked out from.
#[derive(Clone)]
enum WtSource {
    /// A new branch (named so) off the freshly fetched remote mainline.
    NewBranch(String),
    /// Pull request #n's branch (`gh pr checkout`).
    Pr(u64),
}

/// What's being dragged in the sidebar.
#[derive(Clone, PartialEq, Eq, Debug)]
enum DragItem {
    Space(PathBuf),
    /// A linked worktree (a project's own checkout stays first).
    Worktree {
        space: PathBuf,
        wt: PathBuf,
    },
}

/// A sidebar drag: pressed on a row, active once the mouse has moved, over a drop target
/// (a project for a project; a worktree — or the project's own checkout, meaning the top —
/// for a worktree).
struct SideDrag {
    item: DragItem,
    from: gpui::Point<gpui::Pixels>,
    active: bool,
    over: Option<PathBuf>,
}

/// Where the drop line goes.
#[derive(Clone, PartialEq, Eq, Debug)]
enum DropLine {
    BeforeSpace(PathBuf),
    AfterSpace(PathBuf),
    BeforeRow(PathBuf),
    AfterRow(PathBuf),
}

/// A worktree being checked out.
struct Creating {
    space: PathBuf,
    /// Its folder name (the pending row's label).
    name: String,
    path: PathBuf,
    /// What the setup screen says ("Setting up a review of #12").
    doing: String,
}

/// What a new worktree is for: its agent's first prompt and tab name, its title until the
/// agent titles it, and what the setup screen says meanwhile.
#[derive(Clone)]
struct WtStart {
    #[cfg_attr(not(feature = "terminal"), allow(dead_code))]
    prompt: String,
    #[cfg_attr(not(feature = "terminal"), allow(dead_code))]
    tab: String,
    origin: String,
    doing: String,
}

/// What's being renamed in the Agents sidebar / tab strip.
#[derive(Clone, PartialEq, Eq, Debug)]
enum RenameTarget {
    Space(PathBuf),
    Worktree(PathBuf),
    /// A worktree's tab, by its id.
    #[cfg_attr(not(feature = "terminal"), allow(dead_code))]
    Tab(PathBuf, String),
}

/// Which part of a pull request its detail shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PrTab {
    Conversation,
    Commits,
    Checks,
    Files,
}

/// One entry of an issue / PR conversation, ready to render.
enum GhCard {
    /// The description or a comment.
    Comment {
        author: String,
        at: String,
        verb: &'static str,
        body: Entity<mdview::MarkdownView>,
    },
    /// A submitted review, its summary and its inline threads.
    Review {
        author: String,
        at: String,
        state: String,
        body: Option<Entity<mdview::MarkdownView>>,
        threads: Vec<GhThread>,
    },
    /// A pushed commit.
    Commit {
        oid: String,
        headline: String,
        author: String,
        at: String,
    },
}

/// An inline review thread: the file, the last diff lines it's on, its comments.
struct GhThread {
    /// Its first comment's id — replies go to it.
    root: u64,
    path: String,
    hunk: Vec<kyde_git::github::DiffRow>,
    comments: Vec<(String, String, Entity<mdview::MarkdownView>)>,
}

/// One worktree's live tabs, left→right, and which one is shown.
#[cfg(feature = "terminal")]
struct AgentTabs {
    tabs: Vec<AgentTab>,
    active: usize,
}

/// One live agent tab: the PTY terminal plus the clock behind its status dot + cleanup.
#[cfg(feature = "terminal")]
struct AgentTab {
    view: Entity<terminal::TerminalView>,
    /// Its id — names its state file (written by the Claude Code hook) and agent host.
    id: String,
    /// A name you gave it.
    name: Option<String>,
    /// What its Claude Code hook last reported, and when (from the state file).
    hook: Option<(agents::Hook, std::time::Instant)>,
    /// The state file's mtime when last read — it's re-read only when it changes.
    hook_seen: Option<std::time::SystemTime>,
    /// Last output, for the working dot.
    last_output: Option<std::time::Instant>,
    /// Last output OR last time on screen — what the idle cleanup measures.
    last_active: std::time::Instant,
    /// Produced output while off screen (the "done, look at me" dot).
    unseen: bool,
    /// Started with the space's start command (vs a plain shell tab).
    agent: bool,
    /// The dot as last painted — repaint only when the state actually changes.
    shown: agents::AgentState,
    /// A prompt has been submitted here (the first one triggers a quick title read). Output
    /// before that (a banner, a resume's replay) isn't "unseen".
    prompted: bool,
    /// It holds real work — started on a task, resumed, or prompted — so the "never used"
    /// cleanup rule leaves it alone.
    used: bool,
    /// When it started — matched against new Claude Code session files to learn `session`.
    spawned_at: std::time::SystemTime,
    /// Its Claude Code session id, once known (saved, so a relaunch can `--resume` it).
    session: Option<String>,
    /// The cleanup's finding: why this tab can be closed (`None` = keep it).
    verdict: Option<String>,
    /// The last AI "is it done?" check: the output it had seen, and its answer.
    ai: Option<AiCheck>,
}

/// One AI "is this tab done?" answer, and what it was asked about.
#[cfg(feature = "terminal")]
struct AiCheck {
    /// The tab's last output when asked — it's asked again only after new output.
    after: Option<std::time::Instant>,
    /// `Some(why)` = done.
    done: Option<String>,
}

impl AgentsView {
    fn new(cx: &mut Context<Kyde>) -> Self {
        let cfg = kyde_config::agents::AgentsCfg::load();
        let default_cmd_input = cx.new(|cx| {
            let mut e = CodeEditor::single_line(cx, kyde_config::agents::DEFAULT_COMMAND);
            e.set_content(cfg.command.clone(), Lang::PlainText, cx);
            e
        });
        // The Issues / Pull Requests search: re-query GitHub once typing pauses.
        let gh_query =
            cx.new(|cx| CodeEditor::single_line(cx, "Search — words, label:bug, assignee:name…"));
        cx.subscribe(&gh_query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) {
                this.agents_query_changed(cx);
            }
        })
        .detach();
        // The prompt templates: an editor each, saved as you type (blank = the standard one).
        let prompt_editor = |text: &str, standard: &str, cx: &mut Context<Kyde>| {
            cx.new(|cx| {
                let mut e = CodeEditor::new(cx, String::new(), Lang::PlainText, standard);
                e.soft_wrap = true;
                e.fill_height = true;
                e.set_content(
                    if text.trim().is_empty() {
                        standard
                    } else {
                        text
                    }
                    .to_string(),
                    Lang::PlainText,
                    cx,
                );
                e
            })
        };
        let issue_prompt_input =
            prompt_editor(&cfg.issue_prompt, kyde_config::agents::ISSUE_PROMPT, cx);
        let review_prompt_input =
            prompt_editor(&cfg.review_prompt, kyde_config::agents::REVIEW_PROMPT, cx);
        let rereview_prompt_input = prompt_editor(
            &cfg.rereview_prompt,
            kyde_config::agents::REREVIEW_PROMPT,
            cx,
        );
        let save_prompt = |which: fn(&mut kyde_config::agents::AgentsCfg) -> &mut String,
                           standard: &'static str| {
            move |this: &mut Kyde,
                  e: Entity<CodeEditor>,
                  ev: &EditorEvent,
                  cx: &mut Context<Kyde>| {
                if !matches!(ev, EditorEvent::Changed) {
                    return;
                }
                let text = e.read(cx).text().trim().to_string();
                // The standard text unchanged is stored as "use the standard" (it may improve).
                let text = if text == standard {
                    String::new()
                } else {
                    text
                };
                let slot = which(&mut this.agents.cfg);
                if *slot != text {
                    *slot = text;
                    this.agents_save(cx);
                }
            }
        };
        cx.subscribe(
            &issue_prompt_input,
            save_prompt(|c| &mut c.issue_prompt, kyde_config::agents::ISSUE_PROMPT),
        )
        .detach();
        cx.subscribe(
            &review_prompt_input,
            save_prompt(|c| &mut c.review_prompt, kyde_config::agents::REVIEW_PROMPT),
        )
        .detach();
        cx.subscribe(
            &rereview_prompt_input,
            save_prompt(
                |c| &mut c.rereview_prompt,
                kyde_config::agents::REREVIEW_PROMPT,
            ),
        )
        .detach();
        // Persists as you type (debounced, off the UI thread).
        cx.subscribe(&default_cmd_input, |this, e, ev, cx| {
            if !matches!(ev, EditorEvent::Changed) {
                return;
            }
            let text = e.read(cx).text().trim().to_string();
            if this.agents.cfg.command != text {
                this.agents.cfg.command = text;
                this.agents_save(cx);
            }
        })
        .detach();
        Self {
            // Reopen where you left off; else the first space.
            selected: cfg
                .selected
                .clone()
                .filter(|w| cfg.space_of(w).is_some())
                .or_else(|| cfg.spaces.first().map(|s| s.path.clone())),
            cfg,
            branches: std::collections::HashMap::new(),
            git_spaces: std::collections::HashSet::new(),
            git: std::collections::HashMap::new(),
            git_refreshing: false,
            side_tab: SideTab::Code,
            issues: std::collections::HashMap::new(),
            issue_filter: kyde_git::github::IssueFilter::default(),
            pr_filter: kyde_git::github::PrFilter::default(),
            gh_query,
            gh_query_gen: 0,
            gh_fetched: std::collections::HashMap::new(),
            gh_filter_sel: ui::SelectState::new(cx),
            gh_selected: None,
            gh_detail: None,
            gh_body: cx.new(|cx| mdview::MarkdownView::new("", None, cx)),
            gh_pr_tab: PrTab::Conversation,
            gh_cards: Vec::new(),
            gh_comment: cx.new(|cx| {
                let mut e = CodeEditor::new(cx, String::new(), Lang::PlainText, "Leave a comment");
                e.soft_wrap = true;
                e.fill_height = true;
                e
            }),
            gh_reply: cx.new(|cx| CodeEditor::single_line(cx, "Reply…")),
            gh_reply_to: None,
            gh_posting: false,
            window_active: true,
            last_sound: None,

            hooks_polling: false,
            hooks_synced: false,
            renaming: None,
            rename_input: cx.new(|cx| CodeEditor::single_line(cx, "Name")),
            sidebar_hidden: false,
            hosts_restored: false,
            hosts_scanning: false,
            closed_hosts: std::collections::HashSet::new(),
            scrollback: cx.new(|cx| {
                let mut e = CodeEditor::new(cx, String::new(), Lang::PlainText, "");
                e.fill_height = true;
                e
            }),
            hovered_row: None,
            gh_diff: None,
            prs: std::collections::HashMap::new(),
            contexts: std::collections::HashMap::new(),
            context_open: false,
            removing: None,
            hiding: std::collections::HashSet::new(),
            #[cfg(feature = "terminal")]
            restored: std::collections::HashSet::new(),
            main_sync: std::collections::HashMap::new(),
            #[cfg(feature = "terminal")]
            merged: std::collections::HashMap::new(),
            #[cfg(feature = "terminal")]
            sweeping: false,
            #[cfg(feature = "terminal")]
            cleanup_checked: std::collections::HashSet::new(),
            main_syncing: false,
            main_synced_at: None,
            sidebar_w: 330.0,
            #[cfg(feature = "terminal")]
            tabs: std::collections::HashMap::new(),
            creating: Vec::new(),
            drag: None,
            issue_prompt_input,
            review_prompt_input,
            rereview_prompt_input,
            note: None,
            default_cmd_input,
            setup_focused: false,
            save_gen: 0,
            #[cfg(feature = "terminal")]
            ticking: false,
            focus_pending: false,
            autostarted: false,
            contexts_loaded: false,
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
    /// User-created folders still EMPTY on disk — file-derived trees can't see them,
    /// so they're merged into the tree explicitly (pruned once they gain a file or
    /// disappear; cleared on project switch).
    extra_dirs: Vec<PathBuf>,
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
    /// Cut/Copy clipboard for the file tree (issue #67). `None` = nothing copied; a paste
    /// then falls back to the OS clipboard (Finder file copies, image data).
    clipboard: Option<TreeClip>,
    /// Keyboard focus of the file tree (arrows walk/fold it, ⏎ opens — issue #81).
    tree_focus: FocusHandle,
}

/// A pending cut/copy of tree paths (issue #67). `cut` moves on paste; otherwise it copies.
#[derive(Clone)]
struct TreeClip {
    /// The rel paths that were cut/copied.
    paths: Vec<PathBuf>,
    /// True for a cut (paste moves + clears the clipboard); false for a copy.
    cut: bool,
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
            extra_dirs: Vec::new(),
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
            clipboard: None,
            tree_focus: cx.focus_handle(),
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
    /// Keyboard focus of the changed-files tree (arrows, space ticks, ⏎ — issue #81).
    tree_focus: FocusHandle,
    /// The tree's keyboard cursor when it's on a FOLDER (`None` = on the selected file).
    dir_cursor: Option<PathBuf>,
    /// Scroll position of the changed-files tree (keeps the keyboard cursor in view).
    tree_scroll: ScrollHandle,
    /// Keyboard focus of the Push tab's file list.
    push_focus: FocusHandle,
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
            tree_focus: cx.focus_handle(),
            dir_cursor: None,
            tree_scroll: ScrollHandle::new(),
            push_focus: cx.focus_handle(),
        }
    }
}

/// The side-by-side diff panes' state, grouped out of the `Kyde` god-struct: the two
/// editor entities (left = base, read-only; right = working copy, editable + live-saving),
/// the diffed file, the computed diff model, and the pane geometry. (Syntax colouring lives
/// in each editor, highlighted as its content is set.)
/// Shared by the commit view, the history view, the push modal, and the Show-Diff window.
struct DiffPanes {
    /// The computed line/word diff between `base` and the right pane.
    current: Option<FileDiff>,
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
    /// The whitespace dropdown.
    ws_sel: ui::SelectState,
    /// Which contents the resolve stage is showing (3-pane merge or a 2-pane pair).
    compare: MergeCompare,
    /// The Compare Contents dropdown.
    compare_sel: ui::SelectState,
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
            ws_sel: ui::SelectState::new(cx),
            compare: MergeCompare::MergeView3,
            compare_sel: ui::SelectState::new(cx),
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
    /// Folder tree of `push_files` (Push tab + Push window), rebuilt by `set_push_files`.
    push_tree: tree::Tree,
    /// Expanded dirs in the push tree (`""` = the root row).
    push_expanded: std::collections::HashSet<PathBuf>,
    /// The push tree's keyboard cursor when it's on a FOLDER (`None` = on the selected file).
    push_dir_cursor: Option<PathBuf>,
    /// Scroll position of the push tree (keeps the keyboard cursor in view).
    push_scroll: ScrollHandle,
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
            push_tree: tree::Tree::default(),
            push_expanded: std::collections::HashSet::new(),
            push_dir_cursor: None,
            push_scroll: ScrollHandle::new(),
        }
    }

    /// Replace the files a push would send. The tree is only rebuilt (fully expanded, like
    /// the Commit tree) when the set of paths changes, so a periodic refresh doesn't undo
    /// the user's folder collapses.
    fn set_push_files(&mut self, files: Vec<ChangedFile>) {
        let same = files.len() == self.push_files.len()
            && files
                .iter()
                .zip(&self.push_files)
                .all(|(a, b)| a.path == b.path);
        self.push_files = files;
        if same {
            return;
        }
        let paths: Vec<PathBuf> = self.push_files.iter().map(|f| f.path.clone()).collect();
        self.push_tree = tree::Tree::build(&paths);
        self.push_dir_cursor = None;
        self.push_expanded.clear();
        self.push_expanded.insert(PathBuf::new());
        for p in &paths {
            for anc in p.ancestors().skip(1) {
                self.push_expanded.insert(anc.to_path_buf());
            }
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
    shown_at: Option<std::time::Instant>,
}

struct Kyde {
    /// None = no project open → the Projects landing view.
    repo_root: Option<PathBuf>,
    /// `(project root, its git toplevel)` from the last status read — what `repo()` opens
    /// without re-running `git rev-parse`. The toplevel is `None` for a plain folder.
    repo_top: Option<(PathBuf, Option<PathBuf>)>,
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
    /// Keyboard highlight in the Projects landing list (index into the filtered recents).
    projects_hl: Option<usize>,

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
    /// Agents view: the "Remove worktree?" confirmation window.
    remove_wt_win: Option<gpui::WindowHandle<ModalWindow>>,
    scrollback_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// Agents view: the Clean Up window.
    cleanup_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// "What's New" changelog — a native modal window mirroring GitHub Releases (issue #71).
    changelog_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// Its state: the fetched release list, the selected row, and the markdown pane.
    changelog: ChangelogView,
    /// Settings — a native modal window with a category sidebar (Appearance/Keymap/…).
    settings_win: Option<gpui::WindowHandle<ModalWindow>>,
    /// Which Settings category the sidebar has selected.
    settings_section: SettingsSection,
    /// The Appearance → Theme select dropdown.
    settings_theme: ui::SelectState,
    /// The Keymap section's shortcut editor (recording state + last change's note).
    keymap_editor: KeymapEditor,
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
    /// The open menu panel's focus (it takes the keyboard while open — issue #81).
    menu_focus: FocusHandle,
    /// A menu just opened: focus it on the hosting window's next render.
    menu_focus_pending: bool,
    /// What had focus before the menu took it — restored when the menu closes.
    menu_return_focus: Option<FocusHandle>,
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
    /// Whether the open project is inside a git working tree. `false` for a plain folder —
    /// the UI hides every git action (branch chip, git menu items, commit/history) and the
    /// git views explain why instead of offering broken buttons (issue #66).
    is_git: bool,
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

    /// Filesystem watcher on the open project (`FSEvents` on macOS). Its callback forwards
    /// to a debounced `schedule_status_refresh`, so an external create/edit updates git
    /// status + feeds local history without a window refocus. Dropping it stops watching,
    /// so it's kept alive here for the project's lifetime (see `sync_fs_watcher`).
    fs_watcher: Option<notify::RecommendedWatcher>,
    /// The project `fs_watcher` is rooted at — a mismatch with `repo_root` re-arms it.
    watch_root: Option<PathBuf>,

    // Bottom terminal panel — control state + (feature-gated) PTY tab views, grouped into
    // one sub-struct (see `TermState`).
    term: TermState,
    /// The Agents view — spaces → worktrees → agent tabs (`views/agents.rs`).
    agents: AgentsView,
}

/// Which native modal a `ModalWindow` is showing. Each delegates its body back into `Kyde`
/// (the data + actions live there); the window is just a host with a native titlebar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ModalKind {
    Rollback,
    Push,
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
    /// "What's New": the published releases + their notes (issue #71).
    Changelog,
    /// "Remove worktree?" confirmation (Agents view) — says what would be lost.
    RemoveWorktree,
    /// Agents view: pick which tabs to close; finished ones come pre-ticked.
    #[cfg_attr(not(feature = "terminal"), allow(dead_code))]
    Cleanup,
    /// An agent tab's scrollback in an editor (search it, copy from it).
    #[cfg_attr(not(feature = "terminal"), allow(dead_code))]
    Scrollback,
}

/// Changelog-window state (issue #71): the GitHub release list, which one is showing, and
/// the markdown pane its notes render into. Fetched lazily on first open (see
/// `views/changelog.rs`); a failure leaves `error` set and the list empty.
struct ChangelogView {
    /// Published releases, newest first.
    notes: Vec<update::ReleaseNote>,
    /// Index into `notes` of the release being shown.
    selected: usize,
    /// A fetch is in flight (shows "Loading releases…").
    loading: bool,
    /// Why the last fetch failed / why there's nothing to show (shows Retry + a GitHub link).
    error: Option<String>,
    /// The selected release's notes, rendered as selectable markdown.
    body: Entity<mdview::MarkdownView>,
}

impl ChangelogView {
    fn new(cx: &mut Context<Kyde>) -> Self {
        Self {
            notes: Vec::new(),
            selected: 0,
            loading: false,
            error: None,
            body: cx.new(|cx| mdview::MarkdownView::new("", None, cx)),
        }
    }
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
    /// `events[0..=selected]` still differing from their state at it, sorted) — the
    /// bottom "changed files" panel. Every listed file has a working revert.
    files: Vec<PathBuf>,
    /// `files` as a tree (folders + files, the Browse-tree model).
    files_tree: tree::Tree,
    /// Expanded dirs in the changed-files panel (reset to all-expanded on recompute).
    files_expanded: std::collections::HashSet<PathBuf>,
    /// The changed-files panel's selection — the file the diff panes + restores target.
    /// `None` falls back to the selected event's own file.
    file_selected: Option<PathBuf>,
    /// The targeted file is missing from disk (deleted since the snapshot) — the
    /// header says "Deleted" instead of an empty "Current" pane.
    current_missing: bool,
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
            current_missing: false,
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
    Agents,
}

impl SettingsSection {
    /// Sidebar order + labels.
    pub(crate) const ALL: &'static [(SettingsSection, &'static str)] = &[
        (SettingsSection::Appearance, "Appearance"),
        (SettingsSection::Keymap, "Keymap"),
        (SettingsSection::LanguagePacks, "Language Packs"),
        (SettingsSection::LocalHistory, "Local History"),
        (SettingsSection::Agents, "Agents"),
    ];
}

/// A separate native OS window hosting one of Kyde's modals (Rollback / Push / …). It holds
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
        // A find bar hosted here goes with the window (it would otherwise linger "open"
        // with nowhere to render, eating the main window's Escape).
        let host_of = kyde.clone();
        cx.on_release(move |_, cx| {
            host_of.update(cx, |k, kcx| {
                k.find_host_closed(FindHost::Modal(kind), kcx);
                // Closing Settings mid-record must not leave the keystroke capture running.
                if kind == ModalKind::Settings {
                    k.stop_key_recording();
                }
            });
        })
        .detach();
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kyde = self.kyde.clone();
        let kind = self.kind;
        // A context menu opened in this window takes the keyboard (issue #81).
        kyde.update(cx, |k, kcx| {
            k.focus_menu_if_pending(Some(kind), window, kcx);
        });
        let body = kyde.update(cx, |k, kcx| match kind {
            ModalKind::Rollback => k.render_rollback_body(kcx),
            ModalKind::Push => k.render_push_body(kcx),
            ModalKind::NewBranch => k.render_new_branch_body(kcx),
            ModalKind::Plugins => k.render_plugins_body(kcx),
            ModalKind::Fonts => k.render_fonts_body(kcx),
            ModalKind::ClearData => k.render_clear_data_body(kcx),
            ModalKind::Settings => k.render_settings_body(kcx),
            ModalKind::Merge => k.render_merge_body(kcx),
            ModalKind::Compare => k.render_compare_body(kcx),
            ModalKind::LocalHistory => k.render_local_history_body(kcx),
            ModalKind::ClearLocalHistory => k.render_clear_local_history_body(kcx),
            ModalKind::Changelog => k.render_changelog_body(kcx),
            ModalKind::RemoveWorktree => k.render_remove_worktree_body(kcx),
            ModalKind::Cleanup => k.render_cleanup_body(kcx),
            ModalKind::Scrollback => k.render_scrollback_body(kcx),
        });
        let host = FindHost::Modal(kind);
        let find_bar = kyde.update(cx, |k, kcx| k.find_bar(host, theme::font::UI_FAMILY, kcx));
        let body = match find_bar {
            // The find bar (⌘F over a pane in this window) sits atop the body, which fills
            // the rest.
            Some(bar) => div()
                .size_full()
                .flex()
                .flex_col()
                .child(bar)
                .child(div().flex_1().min_h_0().child(body))
                .into_any_element(),
            None => body,
        };
        div()
            .track_focus(&self.focus)
            .key_context("Modal")
            .size_full()
            // ⌘F / ⌘R / ⌘G & co. resolve against this window's panes. The handlers are
            // `Kyde`'s; the bar's own Escape/Enter fire from its "FindBar" key context
            // (dispatched before `on_key_down`, so Escape closes the bar, not the window).
            .on_action(cx.listener(move |this, _: &FindInFile, w, cx| {
                this.kyde
                    .update(cx, |k, kcx| k.open_find(host, false, w, kcx));
            }))
            .on_action(cx.listener(move |this, _: &ReplaceInFile, w, cx| {
                this.kyde
                    .update(cx, |k, kcx| k.open_find(host, true, w, kcx));
            }))
            .on_action(cx.listener(|this, a: &FindNext, w, cx| {
                this.kyde.update(cx, |k, kcx| k.find_next(a, w, kcx));
            }))
            .on_action(cx.listener(|this, a: &FindPrev, w, cx| {
                this.kyde.update(cx, |k, kcx| k.find_prev(a, w, kcx));
            }))
            .on_action(cx.listener(|this, a: &CloseFind, w, cx| {
                this.kyde.update(cx, |k, kcx| k.close_find(a, w, kcx));
            }))
            .on_action(cx.listener(|this, a: &ReplaceOne, w, cx| {
                this.kyde.update(cx, |k, kcx| k.replace_one(a, w, kcx));
            }))
            .on_action(cx.listener(|this, a: &ReplaceAll, w, cx| {
                this.kyde.update(cx, |k, kcx| k.replace_all(a, w, kcx));
            }))
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

// The Kyde config directory (`~/.config/kyde`, XDG-respecting) holds every persisted file:
// plugins.json, keymap.json, theme.json, projects.json, ui.json. Removing it is the full
// "clear data" reset (uninstalls all plugins + drops all cached settings).
use kyde_config::store::config_dir;

/// Tiny persisted UI prefs (`~/.config/kyde/ui.json`), e.g. the FPS-monitor toggle.
fn ui_settings_path() -> PathBuf {
    kyde_config::store::config_path("ui.json")
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
/// side's lines (`None` = filler/blank).
struct DiffRow {
    old: Option<usize>,
    new: Option<usize>,
}

/// Flatten a `FileDiff` into aligned rows. Equal regions advance both sides
/// together; each hunk pairs its old/new lines and pads the shorter side so the
/// two panes stay vertically in sync (and the center gutter lines up).
fn aligned_rows(d: &FileDiff) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let (mut o, mut n) = (0usize, 0usize);
    for h in &d.hunks {
        while o < h.old_range.start && n < h.new_range.start {
            rows.push(DiffRow {
                old: Some(o),
                new: Some(n),
            });
            o += 1;
            n += 1;
        }
        let (ol, nl) = (h.old_range.len(), h.new_range.len());
        for i in 0..ol.max(nl) {
            rows.push(DiffRow {
                old: (i < ol).then(|| h.old_range.start + i),
                new: (i < nl).then(|| h.new_range.start + i),
            });
        }
        o = h.old_range.end;
        n = h.new_range.end;
    }
    while o < d.old.len() && n < d.new.len() {
        rows.push(DiffRow {
            old: Some(o),
            new: Some(n),
        });
        o += 1;
        n += 1;
    }
    rows
}

/// The aligned layout's shape without materialising its rows: `(total rows, (row, hunk) of
/// each hunk's first row)`. O(hunks), where [`aligned_rows`] is O(lines) — the render
/// paths only need where the gutter controls go, every frame.
fn hunk_starts(d: &FileDiff) -> (usize, Vec<(usize, usize)>) {
    let (mut o, mut n, mut row) = (0usize, 0usize, 0usize);
    let mut starts = Vec::with_capacity(d.hunks.len());
    for (hi, h) in d.hunks.iter().enumerate() {
        // The equal stretch before the hunk advances both sides together.
        row += h
            .old_range
            .start
            .saturating_sub(o)
            .min(h.new_range.start.saturating_sub(n));
        starts.push((row, hi));
        row += h.old_range.len().max(h.new_range.len());
        o = h.old_range.end;
        n = h.new_range.end;
    }
    row += d
        .old
        .len()
        .saturating_sub(o)
        .min(d.new.len().saturating_sub(n));
    (row, starts)
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
    key_parts(k).concat()
}

/// "cmd-shift-o" → `["⌘", "⇧", "O"]`: one glyph per key, for key-cap rendering.
fn key_parts(k: &str) -> Vec<String> {
    // The key itself may be `-` (`cmd--`): split it off before the modifiers.
    let (mods, key) = match k.strip_suffix("--") {
        Some(mods) => (mods, "-"),
        None => k.rsplit_once('-').unwrap_or(("", k)),
    };
    let glyph = |part: &str| match part {
        "cmd" => "⌘".to_string(),
        "shift" => "⇧".to_string(),
        "ctrl" => "⌃".to_string(),
        "alt" | "opt" => "⌥".to_string(),
        "enter" => "⏎".to_string(),
        "left" => "←".to_string(),
        "right" => "→".to_string(),
        "up" => "↑".to_string(),
        "down" => "↓".to_string(),
        "backspace" => "⌫".to_string(),
        "tab" => "⇥".to_string(),
        "space" => "Space".to_string(),
        other => other.to_uppercase(),
    };
    mods.split('-')
        .filter(|m| !m.is_empty())
        .chain(std::iter::once(key))
        .map(glyph)
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
            // Agents view (rail icon, add-space, clean-up-idle-tabs).
            "icons/bot.svg" => include_bytes!("../assets/icons/bot.svg"),
            "icons/eraser.svg" => include_bytes!("../assets/icons/eraser.svg"),
            "icons/info.svg" => include_bytes!("../assets/icons/info.svg"),
            "icons/plus.svg" => include_bytes!("../assets/icons/plus.svg"),

            "icons/chevrons-up-down.svg" => include_bytes!("../assets/icons/chevrons-up-down.svg"),
            "icons/chevrons-down-up.svg" => include_bytes!("../assets/icons/chevrons-down-up.svg"),
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
            "icons/file-clock.svg" => include_bytes!("../assets/icons/file-clock.svg"),
            "icons/pencil.svg" => include_bytes!("../assets/icons/pencil.svg"),
            "icons/trash.svg" => include_bytes!("../assets/icons/trash.svg"),
            "icons/scissors.svg" => include_bytes!("../assets/icons/scissors.svg"),
            "icons/copy.svg" => include_bytes!("../assets/icons/copy.svg"),
            "icons/clipboard.svg" => include_bytes!("../assets/icons/clipboard.svg"),
            "icons/chevron-right.svg" => include_bytes!("../assets/icons/chevron-right.svg"),
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

/// The most recent of `recents` that's still a folder on disk — its repo's top level when it's
/// in git, else the folder itself (non-git folders open too).
fn last_project(recents: &[PathBuf]) -> Option<PathBuf> {
    recents
        .iter()
        .find(|p| p.is_dir())
        .map(|p| Repo::discover(p).map_or_else(|_| p.clone(), |repo| repo.root().to_path_buf()))
}

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
        // Rollback window over the Commit view. screenshots.sh passes the git-diff fixture
        // (KYDE_SHOT_REPO, guaranteed working-tree changes): on a clean repo there's nothing
        // to roll back, the window never opens, and the capture waits on a bare Commit view.
        "rollback" => {
            set_packs(view, &["rust"]);
            if let Ok(repo) = std::env::var("KYDE_SHOT_REPO") {
                view.open_project(PathBuf::from(repo), cx);
            }
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
                // Select the newest change (row 0): under Model B its "before" side is the
                // older snapshot, so the panes show a real before→current diff with hunks.
                view.lh_select(0, cx);
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
        // Find (⌘F) over a diff pane (issue #86): `find-diff` = the commit view's working
        // pane, `find-compare` = the Compare window over KYDE_SHOT_FILE ↔ KYDE_SHOT_FILE_B.
        // KYDE_SHOT_QUERY sets the search (default "fn").
        "find-diff" | "find-compare" => {
            set_packs(view, &["rust", "json"]);
            let host = if name == "find-diff" {
                if let Ok(repo) = std::env::var("KYDE_SHOT_REPO") {
                    view.open_project(PathBuf::from(repo), cx);
                }
                view.enter_commit(cx);
                if let Some(i) = diff_pick(view) {
                    view.select_with(i, Some(cx));
                }
                FindHost::Main
            } else {
                if let (Ok(a), Ok(b)) = (
                    std::env::var("KYDE_SHOT_FILE"),
                    std::env::var("KYDE_SHOT_FILE_B"),
                ) {
                    view.open_compare(PathBuf::from(a), PathBuf::from(b), cx);
                }
                FindHost::Modal(ModalKind::Compare)
            };
            view.open_find(host, true, window, cx);
            let q = std::env::var("KYDE_SHOT_QUERY").unwrap_or_else(|_| "fn".into());
            view.find
                .query
                .update(cx, |e, cx| e.set_content(q, Lang::PlainText, cx));
        }
        // The "What's New" changelog window (issue #71). Seeded with fixed release notes so
        // the shot is deterministic and needs no network; `KYDE_SHOT_RELEASES=<file>` renders
        // a real GitHub feed dump instead.
        "changelog" => {
            let seeded = std::env::var("KYDE_SHOT_RELEASES")
                .ok()
                .and_then(|f| std::fs::read_to_string(f).ok())
                .map(|body| update::parse_releases(&body))
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| {
                    ["2.5.0", "2.4.0", "2.3.0"]
                        .iter()
                        .enumerate()
                        .map(|(i, v)| update::ReleaseNote {
                            version: (*v).to_string(),
                            tag: format!("kyde-v{v}"),
                            title: format!("Kyde {v}"),
                            body: String::from(
                                "## Features\n\n* **compare:** side-by-side compare of any \
                                 two files ([#42](https://github.com/kyle-ssg/kyde/issues/42))\n\
                                 * **local history:** per-file snapshots independent of git\n\n\
                                 ## Bug Fixes\n\n* **editor:** keep the caret visible when \
                                 folding a region\n* **tree:** drop deleted paths from the \
                                 file tree\n",
                            ),
                            date: format!("2026-0{}-14", 7 - i),
                            page_url: format!("https://github.com/kyle-ssg/kyde/releases/tag/v{v}"),
                            prerelease: false,
                        })
                        .collect()
                });
            // Seed BEFORE opening: `open_changelog` only fetches when the list is empty, so
            // this keeps the shot offline and deterministic (no live feed racing it in).
            view.set_changelog(Ok(seeded), cx);
            view.open_changelog(cx);
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
        // The History log. screenshots.sh passes a clone with a short feature branch merged
        // back `--no-ff` (KYDE_SHOT_REPO), so the graph has a real branch + merge to draw —
        // main's own history is squash-merged and draws as one line. Selects the branch's
        // first commit, diffed against its parent.
        "history" => {
            set_packs(view, &["rust"]);
            if let Ok(repo) = std::env::var("KYDE_SHOT_REPO") {
                view.open_project(PathBuf::from(repo), cx);
            }
            view.history.compare = CompareMode::Before;
            view.enter_history(cx);
            if let Some(i) = view
                .history
                .commits
                .iter()
                .position(|c| c.subject.starts_with("feat: ease the shimmer"))
            {
                view.select_history_commit(i, cx);
            }
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
        // The Agents view: the open project as a space (plus a linked worktree from
        // KYDE_SHOT_WORKTREE, if given — its tab finishes off screen, so it shows the unseen
        // dot). The start command is a harmless `git log`, and nothing is saved (shots share
        // one config dir).
        #[cfg(feature = "terminal")]
        // The Agents view: KYDE_SHOT_REPO as the project, KYDE_SHOT_WORKTREES (`:`-separated)
        // its worktrees, each running a harmless "agent" (a git log) — the first worktree on
        // screen.
        "agents" => {
            let root = std::env::var("KYDE_SHOT_REPO")
                .map(PathBuf::from)
                .ok()
                .or_else(|| view.repo_root.clone())
                .unwrap_or_default();
            let wts: Vec<PathBuf> = std::env::var("KYDE_SHOT_WORKTREES")
                .unwrap_or_default()
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
            view.agents.cfg.setup_done = true;
            view.agents.cfg.command = "git --no-pager log --oneline --graph --decorate -16".into();
            view.agents.cfg.spaces = vec![kyde_config::agents::Space {
                path: root.clone(),
                worktrees: wts.clone(),
                ..Default::default()
            }];
            for (wt, title) in wts.iter().zip([
                "Ease the shimmer with smoothstep",
                "Whole-word toggle in find",
            ]) {
                view.agents
                    .cfg
                    .origins
                    .insert(wt.clone(), title.to_string());
            }
            view.open_project(root.clone(), cx);
            view.switch_mode(Mode::Agents, cx);
            view.agents_select(root, cx);
            for wt in wts.iter().rev() {
                view.agents_select(wt.clone(), cx);
            }
            // The second worktree's agent has finished while you were on the first: "done".
            #[cfg(feature = "terminal")]
            if let Some(t) = wts
                .get(1)
                .and_then(|wt| view.agents.tabs.get_mut(wt))
                .and_then(|g| g.tabs.first_mut())
            {
                t.prompted = true;
                t.unseen = true;
            }
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
                if mode == MergeCompare::LeftRight {
                    view.merge.compare_sel.show();
                }
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
    // `kyde --agent-host <spec> <fd>`: an agent's host process (see agent_host.rs) — no UI.
    #[cfg(all(feature = "terminal", unix))]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).map(String::as_str) == Some("--agent-host") {
            agent_host::host_main(
                args.get(2).map_or("", String::as_str),
                args.get(3).map_or("", String::as_str),
            );
        }
        // `ky agent <command> …`: the agents CLI (`ky agent` alone still opens ./agent).
        if args.get(1).map(String::as_str) == Some("agent")
            && args
                .get(2)
                .is_some_and(|c| agent_cli::COMMANDS.contains(&c.as_str()) || c == "--help")
        {
            std::process::exit(agent_cli::run(args.get(2..).unwrap_or_default()));
        }
    }
    reexec_with_proper_name();
    set_app_name();
    install_crash_logger();
    // A path arg opens that project directly; no arg → the project you had open last (the
    // Projects landing view only when there's none to reopen). The path may be `.`, a
    // relative dir, or any subdirectory of a repo — resolve it to the repo's top level
    // (`git rev-parse --show-toplevel`, via Repo::discover). If it isn't inside a git repo,
    // fall back to the Projects view rather than a broken state.
    let initial = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .and_then(|p| Repo::discover(&p).ok())
        .map(|repo| repo.root().to_path_buf());

    // Single instance (issue #72): every open project is already a tab, so a second
    // `kyde <path>` hands the path to the running Kyde (which opens it as another tab and
    // comes forward) and exits here, before any window is created. Nothing listening → we
    // are the instance, and take the socket over below.
    #[cfg(unix)]
    let guarded = instance::enabled();
    #[cfg(unix)]
    if guarded {
        let req = initial
            .clone()
            .map_or(instance::Request::Activate, instance::Request::Open);
        if instance::try_send(&req) {
            return;
        }
    }

    // No path given (and not a screenshot run, which wants the landing view): reopen the
    // last project. After the single-instance hand-off, so a bare `ky` just brings a running
    // Kyde forward.
    let initial = initial.or_else(|| {
        std::env::var_os("KYDE_SHOT")
            .is_none()
            .then(|| last_project(&projects::Recents::load().paths))
            .flatten()
    });

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

        // Become the instance later launches talk to (issue #72). The socket thread can't
        // touch gpui entities, so it only forwards over a channel to this foreground pump —
        // the terminal `EventProxy` / fs-watcher pattern. The guard lives in the task, so the
        // socket is unlinked when the app goes away.
        #[cfg(unix)]
        if guarded {
            let (tx, mut rx) = futures::channel::mpsc::unbounded::<instance::Request>();
            if let Some((guard, _thread)) = instance::listen(move |req| {
                let _ = tx.unbounded_send(req);
            }) {
                cx.spawn(async move |cx| {
                    let _guard = guard;
                    while let Some(req) = futures::StreamExt::next(&mut rx).await {
                        let ok = window.update(cx, |view, window, cx| {
                            if let instance::Request::Open(path) = req {
                                view.open_project(path, cx);
                                window.focus(&view.focus_handle(cx));
                            }
                            // Either way the user asked for Kyde — raise it.
                            window.activate_window();
                            cx.activate(true);
                        });
                        if ok.is_err() {
                            break; // main window gone — stop pumping
                        }
                    }
                })
                .detach();
            }
        }
    });
}

#[cfg(test)]
mod diff_layout_tests {
    use super::*;

    /// `hunk_starts` (the per-frame gutter layout, O(hunks)) must agree with the full
    /// `aligned_rows` layout: same row count, each start on its hunk's first row.
    #[test]
    fn hunk_starts_match_the_aligned_rows() {
        let before = "a\nb\nc\nd\ne\nf\ng\n";
        let after = "a\nB\nc\nd\nX\nY\nZ\ne\ng\nh\n";
        let d = FileDiff::compute(before, after);
        let rows = aligned_rows(&d);
        let (n, starts) = hunk_starts(&d);
        assert_eq!(n, rows.len());
        assert_eq!(starts.len(), d.hunks.len());
        for &(row, hi) in &starts {
            let h = &d.hunks[hi];
            let r = &rows[row];
            assert!(
                r.old == (!h.old_range.is_empty()).then_some(h.old_range.start)
                    && r.new == (!h.new_range.is_empty()).then_some(h.new_range.start),
                "hunk {hi} starts on row {row}"
            );
        }
    }
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

    /// Point local history at a throwaway data dir for the whole test binary, so smoke
    /// tests never read or write the developer's real `~/.local/share/kyde` store — the
    /// `refresh` external-change scan (and the boot-time store open) would otherwise land
    /// there. Idempotent + serialized (`Once`), so parallel tests agree on one value that's
    /// in place before any `Kyde::new` reads it.
    fn isolate_history() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let dir = std::env::temp_dir().join(format!("kyde-test-xdg-{}", std::process::id()));
            std::env::set_var("XDG_DATA_HOME", &dir);
            // Config too: the keymap / plugin / theme stores would otherwise read — and a test
            // that saves would overwrite — the developer's real ~/.config/kyde (CI has none).
            std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
        });
    }

    /// Build a `Kyde` window against a throwaway git repo, return its handle + a visual cx.
    fn boot(cx: &mut TestAppContext) -> (gpui::WindowHandle<Kyde>, std::path::PathBuf) {
        isolate_history();
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

    /// Renaming a FOLDER (issue #68) moves its whole subtree on disk and repoints every
    /// piece of Browse UI state that pointed under it — open tabs, the active/selected
    /// path, and the expanded set — to the new location.
    #[gpui::test]
    fn rename_folder_moves_subtree_and_repoints_state(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        std::fs::create_dir_all(dir.join("src/inner")).unwrap();
        std::fs::write(dir.join("src/inner/mod.rs"), "// mod\n").unwrap();
        handle
            .update(cx, |k, _w, cx| {
                k.refresh(cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, w, cx| {
                let inner = PathBuf::from("src/inner/mod.rs");
                k.open_file(inner.clone(), cx);
                k.browse.expanded.insert(PathBuf::from("src"));
                k.browse.expanded.insert(PathBuf::from("src/inner"));
                k.browse.selected_path = Some(inner.clone());
                // Rename the top folder `src` → `lib`.
                k.name_prompt = Some(NamePrompt::Rename(PathBuf::from("src")));
                k.name_input
                    .update(cx, |e, cx| e.set_content("lib".into(), Lang::PlainText, cx));
                k.confirm_name_prompt(w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                // Moved on disk.
                assert!(dir.join("lib/inner/mod.rs").is_file());
                assert!(!dir.join("src").exists());
                // Tabs, selection, expansion all repointed under the new name.
                let moved = PathBuf::from("lib/inner/mod.rs");
                assert!(k.browse.open_tabs.contains(&moved));
                assert_eq!(k.browse.open_path.as_ref(), Some(&moved));
                assert_eq!(k.browse.selected_path.as_ref(), Some(&moved));
                assert!(k.browse.expanded.contains(&PathBuf::from("lib")));
                assert!(k.browse.expanded.contains(&PathBuf::from("lib/inner")));
                assert!(!k.browse.expanded.contains(&PathBuf::from("src")));
            })
            .unwrap();
    }

    /// Tree drag & drop (issue #65): dropping a file onto a folder moves it there; a
    /// drop that's a no-op (already in that folder) or into a folder's own subtree is
    /// refused so it can't destroy the source.
    #[gpui::test]
    fn drag_drop_moves_into_folder_and_guards_bad_drops(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        std::fs::create_dir_all(dir.join("src/sub")).unwrap();
        std::fs::write(dir.join("src/sub/keep.rs"), "// keep\n").unwrap();
        std::fs::write(dir.join("top.txt"), "top\n").unwrap();
        handle.update(cx, |k, _w, cx| k.refresh(cx)).unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                // Move top.txt into src/.
                k.drop_onto_dir(PathBuf::from("top.txt"), PathBuf::from("src"), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(dir.join("src/top.txt").is_file(), "file moved into folder");
        assert!(!dir.join("top.txt").exists(), "source is gone");

        handle
            .update(cx, |k, _w, cx| {
                // No-op: already inside src/.
                k.drop_onto_dir(PathBuf::from("src/top.txt"), PathBuf::from("src"), cx);
                // Refused: moving src into its own descendant would destroy it.
                k.drop_onto_dir(PathBuf::from("src"), PathBuf::from("src/sub"), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            dir.join("src/top.txt").is_file(),
            "no-op drop kept the file"
        );
        assert!(
            dir.join("src/sub/keep.rs").is_file(),
            "self-descendant drop refused"
        );
        assert!(
            !dir.join("src/sub/src").exists(),
            "src was not moved into its child"
        );
    }

    /// File-tree clipboard (issue #67): Copy duplicates (auto-renaming on a clash), Cut
    /// moves and clears the clipboard, and a folder copies recursively.
    #[gpui::test]
    fn clipboard_copy_cut_paste_move_and_duplicate(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        std::fs::write(dir.join("a/x.txt"), "x\n").unwrap();
        handle.update(cx, |k, _w, cx| k.refresh(cx)).unwrap();
        cx.run_until_parked();

        // Copy a/x.txt, paste into b/ → b/x.txt appears, source stays.
        handle
            .update(cx, |k, _w, cx| {
                k.clip_paths(vec![PathBuf::from("a/x.txt")], false, cx);
                k.paste_into(PathBuf::from("b"), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(dir.join("b/x.txt").is_file(), "copy pasted into b/");
        assert!(dir.join("a/x.txt").is_file(), "copy left the source");

        // Paste the same copy back into a/ (where it already lives) → auto-renamed.
        handle
            .update(cx, |k, _w, cx| {
                k.clip_paths(vec![PathBuf::from("a/x.txt")], false, cx);
                k.paste_into(PathBuf::from("a"), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            dir.join("a/x copy.txt").is_file(),
            "clash auto-renamed to ' copy'"
        );

        // Cut folder a/ into b/ → b/a/x.txt appears, a/ is gone, clipboard cleared.
        handle
            .update(cx, |k, _w, cx| {
                k.clip_paths(vec![PathBuf::from("a")], true, cx);
                k.paste_into(PathBuf::from("b"), cx);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            dir.join("b/a/x.txt").is_file(),
            "cut moved the folder recursively"
        );
        assert!(!dir.join("a").exists(), "cut removed the source folder");
        handle
            .update(cx, |k, _w, _cx| {
                assert!(k.browse.clipboard.is_none(), "cut cleared the clipboard");
            })
            .unwrap();
    }

    /// Renaming a tracked file records it clearly in local history (issue #67 QA): the new
    /// path gets a "Renamed" marker and the old path a deletion tombstone.
    #[gpui::test]
    fn rename_shows_clearly_in_local_history(cx: &mut TestAppContext) {
        use kyde_local_history::EventKind;
        let (handle, dir) = boot(cx);
        // app.tsx is a tracked file in the boot repo; rename it.
        handle
            .update(cx, |k, _w, cx| {
                k.move_path(
                    std::path::Path::new("app.tsx"),
                    std::path::Path::new("renamed.tsx"),
                    "Renaming",
                    cx,
                );
            })
            .unwrap();
        cx.run_until_parked();
        assert!(dir.join("renamed.tsx").is_file());
        assert!(!dir.join("app.tsx").exists());
        handle
            .update(cx, |k, _w, _cx| {
                let store = k.lh.store.clone().unwrap();
                let s = store.lock().unwrap();
                let new_ev = s.events_for(std::path::Path::new("renamed.tsx"));
                assert!(
                    new_ev.iter().any(|e| e.label.as_deref() == Some("Renamed")),
                    "new path has a Renamed marker: {new_ev:?}"
                );
                let old_ev = s.events_for(std::path::Path::new("app.tsx"));
                assert!(
                    old_ev.iter().any(|e| e.kind == EventKind::Deleted),
                    "old path is tombstoned: {old_ev:?}"
                );
            })
            .unwrap();
    }

    /// Pasting raw image data (e.g. a screenshot on the clipboard) writes it as an image
    /// file in the target folder — into the folder itself, and (regression) into a folder
    /// even when the paste target resolves to a FILE, without the "not a directory" error.
    #[gpui::test]
    fn paste_clipboard_image_writes_a_file(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        // A tiny valid-enough PNG byte blob — the paste writes bytes verbatim, it doesn't decode.
        let png = b"\x89PNG\r\n\x1a\n-fake-image-bytes".to_vec();
        handle
            .update(cx, |k, _w, cx| {
                let img = gpui::Image::from_bytes(gpui::ImageFormat::Png, png.clone());
                cx.write_to_clipboard(gpui::ClipboardItem::new_image(&img));
                // Nothing on the internal clipboard, so paste falls through to image data.
                assert!(k.browse.clipboard.is_none());
                k.paste_into(PathBuf::from("assets"), cx);
            })
            .unwrap();
        cx.run_until_parked();
        let into_folder = dir.join("assets/image.png");
        assert!(into_folder.is_file(), "image pasted into the folder");
        assert_eq!(std::fs::read(&into_folder).unwrap(), png);

        // Regression: a paste target that is a FILE must resolve to its parent folder, not
        // error with "not a directory".
        std::fs::write(dir.join("note.txt"), "hi\n").unwrap();
        handle
            .update(cx, |k, _w, cx| {
                let img = gpui::Image::from_bytes(gpui::ImageFormat::Png, png.clone());
                cx.write_to_clipboard(gpui::ClipboardItem::new_image(&img));
                k.paste_into(PathBuf::from("note.txt"), cx);
                assert!(k.op_error.is_none(), "no paste error: {:?}", k.op_error);
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            dir.join("image.png").is_file(),
            "file-target paste landed in the parent"
        );
    }

    /// New File / New Folder must work in a plain folder with NO git repo (they're pure
    /// filesystem ops), and a created-but-still-empty folder must show in the Browse
    /// tree (file-derived trees can't see empty dirs — `extra_dirs` carries them).
    #[gpui::test]
    fn new_file_and_folder_work_in_a_plain_non_git_folder(cx: &mut TestAppContext) {
        isolate_history();
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kyde-plain-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("readme.md"), "hi\n").unwrap();

        let km = Keymap::default();
        let root = Some(dir.clone());
        let handle = cx.add_window(move |_w, cx| Kyde::new(root.clone(), km.clone(), false, cx));
        cx.run_until_parked();
        handle
            .update(cx, |k, w, cx| {
                // The non-git filesystem walk populated the tree.
                assert!(k.browse.all_files.contains(&PathBuf::from("readme.md")));

                // New File at the project root.
                k.name_prompt = Some(NamePrompt::NewFile(PathBuf::new()));
                k.name_input.update(cx, |e, cx| {
                    e.set_content("hello.ts".into(), Lang::PlainText, cx);
                });
                k.confirm_name_prompt(w, cx);
                assert_eq!(std::fs::read_to_string(dir.join("hello.ts")).unwrap(), "");

                // New Folder at the project root.
                k.name_prompt = Some(NamePrompt::NewFolder(PathBuf::new()));
                k.name_input.update(cx, |e, cx| {
                    e.set_content("docs".into(), Lang::PlainText, cx);
                });
                k.confirm_name_prompt(w, cx);
                assert!(dir.join("docs").is_dir());
                assert!(k.browse.extra_dirs.contains(&PathBuf::from("docs")));
            })
            .unwrap();
        cx.run_until_parked(); // the refreshes' snapshots land
        handle
            .update(cx, |k, _w, _cx| {
                assert!(k.browse.all_files.contains(&PathBuf::from("hello.ts")));
                // The EMPTY folder is a visible tree row.
                let rows = k.browse.tree.visible(&k.browse.expanded);
                assert!(
                    rows.iter()
                        .any(|r| r.path.as_os_str() == "docs" && r.is_dir),
                    "empty new folder shows in the tree"
                );
            })
            .unwrap();
    }

    /// A plain (non-git) folder reports `is_git == false`, and `do_git_init` turns it into a
    /// real repo so the full git UI comes to life (issue #66).
    #[gpui::test]
    fn non_git_folder_flags_and_git_init_makes_a_repo(cx: &mut TestAppContext) {
        isolate_history();
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kyde-nogit-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("readme.md"), "hi\n").unwrap();

        let km = Keymap::default();
        let root = Some(dir.clone());
        let handle = cx.add_window(move |_w, cx| Kyde::new(root.clone(), km.clone(), false, cx));
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(!k.is_git, "a plain folder is not a git repo");
                // The Browse tree still works (filesystem walk).
                assert!(k.browse.all_files.contains(&PathBuf::from("readme.md")));
            })
            .unwrap();
        // git init here, then refresh flips the flag.
        handle.update(cx, |k, _w, cx| k.do_git_init(cx)).unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(dir.join(".git").is_dir(), "git init created .git");
                assert!(k.is_git, "is_git flips true after init");
            })
            .unwrap();
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
            .update(cx, |k, _w, cx| {
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
                assert!(k.op_error.is_none(), "a clean status read shows no banner");
                // The diff is only computed where it's shown — the Commit view.
                k.enter_commit(cx);
                assert!(
                    k.diff.current.is_some(),
                    "the selected change should load a diff model"
                );
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

    /// Find in every diff view (issue #86): ⌘R over the editable working pane opens
    /// replace, over the read-only base pane it downgrades to find; matches paint on the
    /// find layer without clobbering the diff's word tint; and a bar hosted by a modal
    /// (Compare) goes away with its window.
    #[gpui::test]
    fn find_targets_any_diff_pane_and_modal(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        let set_query = |k: &mut Kyde, q: &str, cx: &mut Context<Kyde>| {
            k.find
                .query
                .update(cx, |e, cx| e.set_content(q.into(), Lang::PlainText, cx));
        };
        handle
            .update(cx, |k, w, cx| {
                k.enter_commit(cx);
                let i = k.files.iter().position(|f| f.path.ends_with("app.tsx"));
                k.select_with(i.unwrap(), Some(cx));
                k.open_find(FindHost::Main, true, w, cx);
                assert_eq!(k.find.target.as_ref(), Some(&k.diff.right));
                assert!(k.find.replace, "working pane is editable → replace row");
                set_query(k, "const", cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, w, cx| {
                assert_eq!(k.find.matches.len(), 1);
                let right = k.diff.right.read(cx);
                assert!(right.find_bg.contains_key(&0), "match painted on line 0");
                assert_eq!(
                    right.find_current,
                    Some((0, 0..5)),
                    "current match outlined"
                );
                assert!(!right.word_bg.is_empty(), "diff word tint kept");
                // Focus the base pane → ⌘R there is find-only, and the old pane's
                // highlights are cleared.
                let left_focus = k.diff.left.read(cx).focus_handle.clone();
                w.focus(&left_focus);
                k.open_find(FindHost::Main, true, w, cx);
                assert_eq!(k.find.target.as_ref(), Some(&k.diff.left));
                assert!(!k.find.replace, "read-only pane → find only");
                assert!(k.diff.right.read(cx).find_bg.is_empty());
            })
            .unwrap();

        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("b.txt"), "one\nTWO\ntwo\n").unwrap();
        handle
            .update(cx, |k, _w, cx| {
                k.open_compare(PathBuf::from("a.txt"), PathBuf::from("b.txt"), cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, w, cx| {
                k.open_find(FindHost::Modal(ModalKind::Compare), false, w, cx);
                assert_eq!(k.find.target.as_ref(), Some(&k.compare.right));
                set_query(k, "two", cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                assert_eq!(k.find.matches.len(), 2, "case-insensitive: TWO + two");
                assert!(
                    !k.find_shown(FindHost::Main),
                    "the bar lives in the Compare window"
                );
                k.close_modal_window(ModalKind::Compare, cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                assert!(!k.find.open, "closing the host window closes its find bar");
                assert!(k.compare.right.read(cx).find_bg.is_empty());
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

    /// A diff whose language pack isn't installed offers the install banner — a Java change in
    /// History used to show as plain text with no hint why (#92). Installing re-highlights both
    /// diff panes in place, and the banner goes.
    #[gpui::test]
    fn diff_offers_a_missing_pack_and_installing_rehighlights_it(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle.update(cx, |k, _w, cx| k.refresh(cx)).unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                k.plugins.uninstall("typescript");
                k.enter_commit(cx);
                let i = k
                    .files
                    .iter()
                    .position(|f| f.path == std::path::Path::new("app.tsx"))
                    .unwrap();
                k.select_with(i, Some(cx));
                assert_eq!(k.diff_pending_pack().map(|p| p.id), Some("typescript"));
                assert_eq!(k.diff.right.read(cx).lang, Lang::PlainText);

                // The banner's Install, minus the save into the developer's real config.
                k.plugins.install("typescript");
                k.rehighlight_for(Lang::Tsx, cx);
                assert!(
                    k.diff_pending_pack().is_none(),
                    "banner gone once installed"
                );
                assert_eq!(k.diff.left.read(cx).lang, Lang::Tsx);
                assert_eq!(k.diff.right.read(cx).lang, Lang::Tsx);
            })
            .unwrap();
    }

    /// Settings → Keymap: recording a shortcut rebinds the action (taking the key from the
    /// action that had it, with a note saying so), a bare key is refused with recording left
    /// on, and a confirmed "Reset to `WebStorm` / `IntelliJ`" puts every default back.
    #[gpui::test]
    fn keymap_editor_records_rebinds_and_resets(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, w, cx| {
                k.choose_preset(Preset::WebStorm, cx);
                k.start_key_recording("push", w, cx);
                k.record_keystroke(&gpui::Keystroke::parse("cmd-shift-o").unwrap(), cx);
                assert_eq!(k.keymap.key_for("push").as_deref(), Some("cmd-shift-o"));
                assert_eq!(k.keymap.key_for("go_to_file"), None);
                assert_eq!(
                    k.keymap_editor.note.as_deref(),
                    Some("⌘⇧O moved from Go to File")
                );
                assert_eq!(k.keymap_editor.recording, None);

                k.start_key_recording("pull", w, cx);
                k.record_keystroke(&gpui::Keystroke::parse("p").unwrap(), cx);
                assert_eq!(k.keymap_editor.recording, Some("pull"), "bare key refused");
                assert_eq!(k.keymap.key_for("pull").as_deref(), Some("cmd-t"));

                k.stop_key_recording();
                k.request_preset_reset(Preset::WebStorm, cx);
                assert_eq!(
                    k.keymap.key_for("push").as_deref(),
                    Some("cmd-shift-o"),
                    "nothing changes until the reset is confirmed"
                );
                k.confirm_preset_reset(cx);
                assert_eq!(
                    k.keymap.key_for("go_to_file").as_deref(),
                    Some("cmd-shift-o")
                );
                assert_eq!(k.keymap.key_for("push").as_deref(), Some("cmd-shift-k"));
            })
            .unwrap();
    }

    /// History branches sidebar (#92): branches load off the UI thread when the view opens,
    /// `/` folders start expanded, and a folder the user collapsed stays collapsed on reload.
    #[gpui::test]
    fn history_branches_sidebar_loads_and_remembers_collapse(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        git(&dir, &["branch", "feature/greeting"]);
        handle.update(cx, |k, _w, cx| k.enter_history(cx)).unwrap();
        cx.run_until_parked();
        let folder = "sec:local/feature".to_string();
        handle
            .update(cx, |k, _w, cx| {
                assert!(k.history.locals.iter().any(|b| b == "feature/greeting"));
                assert!(k.history.branches_expanded.contains(&folder));
                k.toggle_history_branch_folder(folder.clone(), cx);
                k.enter_history(cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(
                    !k.history.branches_expanded.contains(&folder),
                    "a collapsed folder must stay collapsed when the branches reload"
                );
                assert!(k.history.branches_expanded.contains("sec:local"));
            })
            .unwrap();
    }

    /// Push tree (#92): a refresh with the same files keeps the user's folder collapses; a
    /// changed file set rebuilds the tree fully expanded.
    #[test]
    fn push_tree_keeps_collapses_until_files_change() {
        let f = |p: &str| ChangedFile {
            path: PathBuf::from(p),
            status: FileStatus::Modified,
        };
        let src = std::path::Path::new("src");
        let mut s = SyncState::new();
        s.set_push_files(vec![f("src/a.rs"), f("src/b/c.rs")]);
        assert!(s.push_expanded.contains(src));
        s.push_expanded.remove(src);
        s.set_push_files(vec![f("src/a.rs"), f("src/b/c.rs")]);
        assert!(
            !s.push_expanded.contains(src),
            "a refresh must not re-expand"
        );
        s.set_push_files(vec![f("src/a.rs")]);
        assert!(s.push_expanded.contains(src));
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

    /// The Local History changed-files panel + reverts under Model B ("this change &
    /// later"): selecting a change lists every file it (or a later change) touched vs the
    /// state before it; a file whose selected change is an EDIT reverts to the pre-edit
    /// content, one whose selected change is its CREATION reverts by DELETION; folder and
    /// timeline reverts do the same over their scope; a file changed BACK drops out of the
    /// panel; a recorded-but-never-on-disk file (both sides empty) is never listed; and
    /// every revert records a recoverable "Before revert" label.
    #[gpui::test]
    fn local_history_changed_panel_and_reverts(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("app.tsx"), "now-a\n").unwrap();
        std::fs::write(dir.join("sub/lib.rs"), "now-l\n").unwrap();
        std::fs::write(dir.join("new.txt"), "n1\n").unwrap();
        std::fs::write(dir.join("empty.txt"), "").unwrap();
        // ghost.txt is recorded below but never written to disk.
        handle
            .update(cx, |k, _w, cx| {
                lh_test_store(k, &dir);
                {
                    use kyde_local_history::EventKind;
                    let store = k.lh.store.clone().unwrap();
                    let mut s = store.lock().unwrap();
                    let rec = |s: &mut kyde_local_history::Store, p: &str, c: &str, ts: u64| {
                        s.record(std::path::Path::new(p), c, EventKind::Change, None, ts)
                            .unwrap();
                    };
                    rec(&mut s, "app.tsx", "a1\n", 1_000); // app created
                    rec(&mut s, "ghost.txt", "", 1_500); // empty, never on disk
                    rec(&mut s, "sub/lib.rs", "l1\n", 2_000); // lib created
                    rec(&mut s, "app.tsx", "a2\n", 3_000); // app edited
                    rec(&mut s, "sub/lib.rs", "l2\n", 4_000); // lib edited
                    rec(&mut s, "new.txt", "n1\n", 5_000); // new created, unchanged since
                    rec(&mut s, "empty.txt", "", 6_000); // empty created
                }
                k.lh.path = Some(PathBuf::new()); // whole-project scope
                k.lh.selected = 0;
                k.lh_reload(cx);
                assert_eq!(k.lh.events.len(), 7);
                let row = |k: &Kyde, p: &str, ts: u64| {
                    k.lh.events
                        .iter()
                        .position(|e| e.path == std::path::Path::new(p) && e.ts_ms == ts)
                        .unwrap()
                };

                // (1) Select app's CREATION (the oldest change) → undo everything from
                // here on: every file that exists now is listed, ghost.txt (recorded
                // empty, never on disk — both sides empty) is NOT.
                k.lh_select(row(k, "app.tsx", 1_000), cx);
                assert_eq!(
                    k.lh.files,
                    vec![
                        PathBuf::from("app.tsx"),
                        PathBuf::from("empty.txt"),
                        PathBuf::from("new.txt"),
                        PathBuf::from("sub/lib.rs"),
                    ]
                );

                // (2) Folder revert restores CONTENT when the selected change is an EDIT
                // (the file had a prior state). Select lib's edit (l1→l2); reverting "sub"
                // undoes it → l1.
                std::fs::write(dir.join("sub/lib.rs"), "zz\n").unwrap();
                k.lh_select(row(k, "sub/lib.rs", 4_000), cx);
                k.lh_revert_path(PathBuf::from("sub"), cx);
                assert_eq!(
                    std::fs::read_to_string(dir.join("sub/lib.rs")).unwrap(),
                    "l1\n",
                    "undoing an edit restores the pre-edit content"
                );

                // (3) File revert DELETES when the selected change is the file's CREATION.
                // Select new.txt's creation; reverting removes it (recoverable via label).
                k.lh_select(row(k, "new.txt", 5_000), cx);
                assert!(k.lh_has_base_under(std::path::Path::new("new.txt")));
                k.lh_revert_path(PathBuf::from("new.txt"), cx);
                assert!(
                    !dir.join("new.txt").exists(),
                    "undoing a creation deletes the file"
                );
                {
                    let store = k.lh.store.clone().unwrap();
                    let s = store.lock().unwrap();
                    let ev = s.events_for(std::path::Path::new("new.txt"));
                    assert_eq!(ev[0].label.as_deref(), Some("Before revert"));
                    assert_eq!(
                        s.content(&ev[0].hash).unwrap(),
                        "n1\n",
                        "pre-delete content is recoverable"
                    );
                }

                // (4) File revert restores CONTENT on an edit; the timeline immediately
                // shows the "Before revert" marker.
                std::fs::write(dir.join("app.tsx"), "zz\n").unwrap();
                k.lh_select(row(k, "app.tsx", 3_000), cx);
                k.lh_revert_path(PathBuf::from("app.tsx"), cx);
                assert_eq!(
                    std::fs::read_to_string(dir.join("app.tsx")).unwrap(),
                    "a1\n"
                );
                use kyde_local_history::EventKind;
                assert_eq!(k.lh.events[0].kind, EventKind::Label);
                assert_eq!(k.lh.events[0].label.as_deref(), Some("Before revert"));
                assert_eq!(k.lh.events[0].path, PathBuf::from("app.tsx"));

                // (5) app.tsx is now a1 = its state before the a2 edit (changed BACK), so
                // at that edit's row it drops out of the panel — "No differences" is noise.
                k.lh_select(row(k, "app.tsx", 3_000), cx);
                assert!(
                    !k.lh.files.contains(&PathBuf::from("app.tsx")),
                    "a file reverted to its pre-change state leaves the panel"
                );

                // (6) Timeline "Revert This Change and After" at lib's CREATION: deletes
                // lib (created here) and empty.txt (created later); labels every target.
                std::fs::write(dir.join("sub/lib.rs"), "l1\n").unwrap(); // restore fixture
                k.lh_select(row(k, "sub/lib.rs", 2_000), cx);
                k.lh_revert_since(cx);
                assert!(
                    !dir.join("sub/lib.rs").exists(),
                    "revert-since deletes files created at this change or later"
                );
                assert!(
                    !dir.join("empty.txt").exists(),
                    "an empty created file is deleted too — existence is the change"
                );
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

    /// Model B ("this change & later"): selecting a file's OWN creation event lists it as a
    /// change and reverts by DELETING it — even for an EMPTY new file (the user's `abc.txt`
    /// case that used to show a confusing "0 files / No differences"). The pre-revert state
    /// is labeled first, so it stays recoverable. The virtual "Start of history" row does
    /// the same (base `None` for everything).
    #[gpui::test]
    fn local_history_creation_event_lists_and_reverts_by_deletion(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                lh_test_store(k, &dir);
                {
                    use kyde_local_history::EventKind;
                    let store = k.lh.store.clone().unwrap();
                    let mut s = store.lock().unwrap();
                    s.record(
                        std::path::Path::new("solo.txt"),
                        "",
                        EventKind::Label,
                        Some("Created".into()),
                        1_000,
                    )
                    .unwrap();
                }
                std::fs::write(dir.join("solo.txt"), "").unwrap();
                k.lh.path = Some(PathBuf::from("solo.txt"));
                k.lh.selected = 0;
                k.lh.file_selected = None;
                k.lh_reload(cx);
                assert_eq!(k.lh.events.len(), 1);
                // The creation event itself now lists the file — it went from not-existing
                // to existing, which IS the change (even though empty). The old model
                // showed "0 files / No differences" here; this is the fix.
                assert_eq!(k.lh.files, vec![PathBuf::from("solo.txt")]);
                assert!(k.lh_has_base_under(std::path::Path::new("solo.txt")));
                // Reverting the creation deletes the file (undo "this change and after").
                k.lh_revert_since(cx);
                assert!(
                    !dir.join("solo.txt").exists(),
                    "undoing the creation deletes the file"
                );
                // Recoverable: the pre-revert (empty) content was labeled first.
                let store = k.lh.store.clone().unwrap();
                let s = store.lock().unwrap();
                let ev = s.events_for(std::path::Path::new("solo.txt"));
                assert_eq!(ev[0].label.as_deref(), Some("Before revert"));
            })
            .unwrap();
        cx.run_until_parked();
    }

    /// Delete → re-create reads as a deletion then an ADDITION, not one long edit: the
    /// tombstone (`EventKind::Deleted`) makes the re-creation diff against "did not exist"
    /// (so it's listed/added), and reverting the DELETION row undoes the deletion (restores
    /// the pre-delete content).
    #[gpui::test]
    fn local_history_delete_then_recreate_reads_as_deletion_then_addition(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                lh_test_store(k, &dir);
                {
                    use kyde_local_history::EventKind;
                    let store = k.lh.store.clone().unwrap();
                    let mut s = store.lock().unwrap();
                    let p = std::path::Path::new("note.txt");
                    s.record(p, "v1\n", EventKind::Change, None, 1_000).unwrap();
                    s.record_deletion(p, 2_000).unwrap();
                    s.record(p, "v2\n", EventKind::External, None, 3_000)
                        .unwrap();
                }
                std::fs::write(dir.join("note.txt"), "v2\n").unwrap(); // currently re-created
                k.lh.path = Some(PathBuf::from("note.txt"));
                k.lh.selected = 0;
                k.lh.file_selected = None;
                k.lh_reload(cx);
                use kyde_local_history::EventKind;
                assert_eq!(k.lh.events.len(), 3);
                assert_eq!(
                    k.lh.events[0].kind,
                    EventKind::External,
                    "row 0 = re-creation"
                );
                assert_eq!(k.lh.events[1].kind, EventKind::Deleted, "row 1 = deletion");
                // The re-creation is listed as a change (added back) — its "before" is the
                // deletion tombstone, i.e. "did not exist".
                assert_eq!(k.lh.files, vec![PathBuf::from("note.txt")]);
                // Select the deletion row: the file is listed, and undoing the deletion
                // restores the pre-delete content.
                let del =
                    k.lh.events
                        .iter()
                        .position(|e| e.kind == EventKind::Deleted)
                        .unwrap();
                k.lh_select(del, cx);
                assert_eq!(k.lh.files, vec![PathBuf::from("note.txt")]);
                k.lh_revert_to_selected(cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert_eq!(
                    std::fs::read_to_string(dir.join("note.txt")).unwrap(),
                    "v1\n",
                    "undoing the deletion restores the pre-delete content"
                );
                let _ = k;
            })
            .unwrap();
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

    /// Changelog window (issue #71): opening it spawns the native window; a fetch result
    /// populates the version list, selects the newest, and pushes its notes into the
    /// markdown pane; selecting an older release swaps the notes. A failed fetch keeps the
    /// window usable (error text + Retry), never a blank pane.
    #[gpui::test]
    fn changelog_lists_releases_and_shows_their_notes(cx: &mut TestAppContext) {
        let (handle, _dir) = boot(cx);
        handle.update(cx, |k, _w, cx| k.open_changelog(cx)).unwrap();
        cx.run_until_parked(); // the native window opens on a spawned task
        handle
            .update(cx, |k, _w, cx| {
                assert!(k.changelog_win.is_some(), "changelog window opens");
                let note = |v: &str, body: &str| update::ReleaseNote {
                    version: v.into(),
                    tag: format!("kyde-v{v}"),
                    title: format!("Kyde {v}"),
                    body: body.into(),
                    date: "2026-07-01".into(),
                    page_url: format!("https://example.com/{v}"),
                    prerelease: false,
                };
                k.set_changelog(
                    Ok(vec![note("2.5.0", "# new stuff"), note("2.4.0", "# old")]),
                    cx,
                );
                assert_eq!(k.changelog.notes.len(), 2);
                assert_eq!(k.changelog.selected, 0, "newest release shown first");
                assert!(k.changelog.error.is_none());
                assert_eq!(k.changelog.body.read(cx).source(), "# new stuff");

                k.select_changelog(1, cx);
                assert_eq!(k.changelog.body.read(cx).source(), "# old");

                // Fetch failure → an error to show, and the retry path is armed.
                k.set_changelog(Err("network is down".into()), cx);
                assert_eq!(k.changelog.error.as_deref(), Some("network is down"));
                assert!(!k.changelog.loading);
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
        // Keep the store OUTSIDE the repo (like production, which roots it under XDG) —
        // a store inside the working tree would show up in `git status` as untracked and
        // the external-change scan would try to record the store into itself.
        let store_dir = std::env::temp_dir().join(format!(
            "kyde-lh-store-{}",
            dir.file_name().unwrap().to_string_lossy()
        ));
        let _ = std::fs::remove_dir_all(&store_dir);
        let store = kyde_local_history::Store::open(store_dir).unwrap();
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

    /// External-change recording (the whole point of this fix): a file changed OUTSIDE
    /// Kyde — written straight to disk, never saved through the editor, never re-opened in
    /// Browse — still lands in local history. `apply_snapshot` runs `lh_scan_external` over
    /// the git-changed files on every refresh; here we drive that scan directly (a real
    /// refresh's debounce/watcher timing is exercised elsewhere) to check it records a
    /// baseline on first sight and an `External` snapshot when the disk later moves.
    #[gpui::test]
    fn local_history_records_external_changes_on_refresh(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                lh_test_store(k, &dir);
                // boot's refresh already populated the changed-files list (app.tsx
                // modified, new.txt untracked) — scan it into the fresh store.
                assert!(
                    !k.files.is_empty(),
                    "boot left the changed-files list ready"
                );
                k.lh_scan_external(cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                {
                    let store = k.lh.store.clone().unwrap();
                    let s = store.lock().unwrap();
                    let app_ev = s.events_for(std::path::Path::new("app.tsx"));
                    assert_eq!(
                        app_ev.len(),
                        1,
                        "a changed file's baseline is recorded from the scan, unopened"
                    );
                    assert_eq!(
                        app_ev[0].kind,
                        kyde_local_history::EventKind::External,
                        "anything the scan catches is an External change, never a plain Change"
                    );
                    assert_eq!(
                        s.events_for(std::path::Path::new("new.txt")).len(),
                        1,
                        "an externally-created (untracked) file records its baseline too"
                    );
                }
                // Now edit app.tsx entirely outside Kyde: straight to disk, no save path,
                // no reopen — then scan again, as a watcher-driven refresh would.
                std::fs::write(dir.join("app.tsx"), "const a = 99;\n").unwrap();
                k.lh_scan_external(cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                let store = k.lh.store.clone().unwrap();
                let s = store.lock().unwrap();
                let ev = s.events_for(std::path::Path::new("app.tsx"));
                assert_eq!(
                    ev.len(),
                    2,
                    "the external edit is captured as a new snapshot"
                );
                assert_eq!(
                    ev[0].kind,
                    kyde_local_history::EventKind::External,
                    "and it's labeled as an external change, not an in-app save"
                );
                assert_eq!(s.content(&ev[0].hash).unwrap(), "const a = 99;\n");
            })
            .unwrap();
    }

    /// Local history revert: selecting a change and undoing it rewrites the file to its
    /// pre-change content, stamps a "Before revert" label (the pre-revert state stays
    /// recoverable), and reloads the timeline. Model B: to get back to `v1` you undo the
    /// change that produced the current `v2` (its row), which reverts to the state before.
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
                // Undo the newest change (the one that produced current `v2`, row 0):
                // reverting it restores the state before = `v1`.
                k.lh_select(0, cx);
                k.lh_revert_to_selected(cx);
            })
            .unwrap();
        cx.run_until_parked(); // background "Before revert" record + refresh land
        handle
            .update(cx, |k, _w, _cx| {
                assert_eq!(
                    std::fs::read_to_string(dir.join("app.tsx")).unwrap(),
                    "const a = 1;\n",
                    "undoing the change restores the state before it"
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
            .update(cx, |k, w, cx| k.open_find(FindHost::Main, false, w, cx))
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

        // Full-screen Show-Diff view (Rollback → View Diff), then back out of it.
        handle
            .update(cx, |k, _w, cx| k.menu_show_diff(0, cx))
            .unwrap();
        settle(cx);
        handle
            .update(cx, |k, _w, cx| k.close_diff_view(cx))
            .unwrap();
        settle(cx);

        // History view: the commit log with its graph column + the selected commit's diff.
        handle.update(cx, |k, _w, cx| k.enter_history(cx)).unwrap();
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

    /// Entering the Agents view fills every space's branch + git badges off the UI thread.
    #[cfg(feature = "terminal")]
    #[gpui::test]
    fn agents_refresh_fills_branch_and_git(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.agents.cfg.spaces = vec![kyde_config::agents::Space {
                    path: dir.clone(),
                    ..Default::default()
                }];
                k.switch_mode(Mode::Agents, cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, _cx| {
                assert!(
                    k.agents.branches.contains_key(&dir),
                    "branch: {:?}",
                    k.agents.branches
                );
                assert!(k.agents.git_spaces.contains(&dir));
                assert_eq!(k.agents.git.get(&dir).map(|g| g.changed), Some(2));
            })
            .unwrap();
    }

    /// Launching with no path reopens the most recent project that still exists.
    #[test]
    fn launch_reopens_the_last_project_that_still_exists() {
        let gone = std::env::temp_dir().join("kyde-no-such-project-dir");
        let here = std::env::temp_dir();
        assert_eq!(last_project(&[gone.clone(), here.clone()]), Some(here));
        assert_eq!(last_project(&[gone]), None);
    }

    /// Dragging a worktree row onto another moves it there (and the project's own checkout
    /// stays first) — through real mouse events on the rendered sidebar.
    #[cfg(feature = "terminal")]
    #[gpui::test]
    fn agents_drag_reorders_worktrees(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .output()
                .unwrap();
        };
        let (a, b) = (dir.with_extension("wta"), dir.with_extension("wtb"));
        git(&["worktree", "add", "-q", "-b", "wta", a.to_str().unwrap()]);
        git(&["worktree", "add", "-q", "-b", "wtb", b.to_str().unwrap()]);
        // As git lists them (macOS temp dirs resolve under /private).
        let (a, b) = (a.canonicalize().unwrap(), b.canonicalize().unwrap());
        handle
            .update(cx, |k, _w, cx| {
                k.agents.cfg.setup_done = true;
                k.agents.cfg.command = "true".into();
                k.agents.cfg.spaces = vec![kyde_config::agents::Space {
                    path: dir.clone(),
                    worktrees: vec![a.clone(), b.clone()],
                    ..Default::default()
                }];
                k.switch_mode(Mode::Agents, cx);
            })
            .unwrap();
        cx.run_until_parked();
        let mut vcx = gpui::VisualTestContext::from_window(handle.into(), cx);
        vcx.run_until_parked();
        let name = |p: &std::path::Path| p.file_name().unwrap().to_string_lossy().into_owned();
        let sel_b: &'static str = Box::leak(format!("agents-wt-{}", name(&b)).into_boxed_str());
        let sel_a: &'static str = Box::leak(format!("agents-wt-{}", name(&a)).into_boxed_str());
        let rb = vcx
            .debug_bounds(sel_b)
            .expect("worktree b's row is on screen");
        let ra = vcx
            .debug_bounds(sel_a)
            .expect("worktree a's row is on screen");
        let none = gpui::Modifiers::none();
        vcx.simulate_mouse_down(rb.center(), gpui::MouseButton::Left, none);
        let step = gpui::point(rb.center().x, rb.center().y - gpui::px(8.0));
        vcx.simulate_mouse_move(step, gpui::MouseButton::Left, none);
        vcx.simulate_mouse_move(ra.center(), gpui::MouseButton::Left, none);
        let state = handle
            .update(&mut vcx, |k, _w, _cx| {
                k.agents.drag.as_ref().map(|d| (d.active, d.over.clone()))
            })
            .unwrap();
        assert_eq!(state, Some((true, Some(a.clone()))), "dragging b over a");
        vcx.simulate_mouse_up(ra.center(), gpui::MouseButton::Left, none);
        handle
            .update(&mut vcx, |k, _w, _cx| {
                assert_eq!(k.agents.cfg.spaces[0].worktrees, vec![b.clone(), a.clone()]);
            })
            .unwrap();
        // The project's own checkout drags too: down past both.
        vcx.run_until_parked();
        let sel_main: &'static str =
            Box::leak(format!("agents-wt-{}", name(&dir)).into_boxed_str());
        let rm = vcx.debug_bounds(sel_main).expect("the checkout's row");
        let ra = vcx.debug_bounds(sel_a).expect("a's row");
        vcx.simulate_mouse_down(rm.center(), gpui::MouseButton::Left, none);
        let step = gpui::point(rm.center().x, rm.center().y + gpui::px(8.0));
        vcx.simulate_mouse_move(step, gpui::MouseButton::Left, none);
        vcx.simulate_mouse_move(ra.center(), gpui::MouseButton::Left, none);
        vcx.simulate_mouse_up(ra.center(), gpui::MouseButton::Left, none);
        handle
            .update(&mut vcx, |k, _w, _cx| {
                assert_eq!(
                    k.agents.cfg.spaces[0].rows(),
                    vec![b.clone(), a.clone(), dir.clone()]
                );
            })
            .unwrap();
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    /// Agents view end-to-end: adding a space opens its default worktree (the folder itself)
    /// with a first agent tab; the cleanup closes an off-screen tab whose agent has quit —
    /// never the one on screen. Real PTYs.
    #[cfg(feature = "terminal")]
    #[gpui::test]
    fn agents_space_opens_tab_and_cleanup_spares_visible(cx: &mut TestAppContext) {
        let (handle, dir) = boot(cx);
        handle
            .update(cx, |k, _w, cx| {
                k.agents.cfg.command = "true".into(); // never launch a real agent in tests
                k.switch_mode(Mode::Agents, cx);
                k.agents_add_space(dir.clone(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                assert_eq!(k.agents.selected.as_deref(), Some(dir.as_path()));
                assert_eq!(
                    k.agents.tabs[&dir].tabs.len(),
                    1,
                    "first tab starts on select"
                );
                // A shell tab beside it — it becomes the visible one.
                k.agents_new_tab(dir.clone(), false, None, cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, _w, cx| {
                // Both tabs quiet for an hour, seen, with nothing running in their shells.
                let hour_ago = std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_hours(1))
                    .unwrap();
                let mut found = agents::SweepFound::default();
                for t in &mut k.agents.tabs.get_mut(&dir).unwrap().tabs {
                    t.unseen = false;
                    t.last_active = hour_ago;
                    found.at_prompt.push(((dir.clone(), t.spawned_at), true));
                }
                // Auto mode (the default): the off-screen one closes, the visible one is spared.
                assert_eq!(k.agents_apply_sweep(found, false, cx), 1);
                let g = &k.agents.tabs[&dir];
                assert_eq!(g.tabs.len(), 1);
                assert!(!g.tabs[0].agent, "the visible shell tab survived");
            })
            .unwrap();
    }

    /// Enter is also bound at the app root (`ConfirmKey`). The deeper Terminal binding must win
    /// and relay CR to the PTY instead of letting the root action consume the key.
    #[cfg(feature = "terminal")]
    #[gpui::test]
    fn terminal_enter_binding_sends_carriage_return(cx: &mut TestAppContext) {
        cx.update(|cx| apply_keymap(cx, &Keymap::default()));
        let routed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _observer = cx.update(|cx| {
            let routed = routed.clone();
            cx.observe_keystrokes(move |event, _window, _cx| {
                if event
                    .action
                    .as_ref()
                    .is_some_and(|action| action.partial_eq(&TerminalEnter))
                {
                    routed.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            })
        });
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, window, cx| {
                k.act_toggle_terminal(&ToggleTerminal, window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, window, cx| k.focus_active_terminal(window, cx))
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, window, cx| {
                let enter = gpui::Keystroke::parse("enter").expect("valid Enter keystroke");
                window.dispatch_keystroke(enter, cx);
                assert!(k.term.panel.open);
            })
            .unwrap();
        assert!(
            routed.load(std::sync::atomic::Ordering::Relaxed),
            "TerminalEnter must shadow the root ConfirmKey binding"
        );
    }

    /// ⌘C / ⌘V are the file tree's copy / paste at the app root; in a terminal they must be
    /// the terminal's, or copying text out of an agent does nothing.
    #[cfg(feature = "terminal")]
    #[gpui::test]
    fn terminal_copy_paste_beat_the_file_tree_bindings(cx: &mut TestAppContext) {
        cx.update(|cx| apply_keymap(cx, &Keymap::default()));
        let routed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let _observer = cx.update(|cx| {
            let routed = routed.clone();
            cx.observe_keystrokes(move |event, _window, _cx| {
                if let Some(a) = event.action.as_ref() {
                    routed.lock().unwrap().push(a.name().to_string());
                }
            })
        });
        let (handle, _dir) = boot(cx);
        handle
            .update(cx, |k, window, cx| {
                k.act_toggle_terminal(&ToggleTerminal, window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |k, window, cx| k.focus_active_terminal(window, cx))
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |_k, window, cx| {
                for key in ["cmd-c", "cmd-v"] {
                    let stroke = gpui::Keystroke::parse(key).expect("valid keystroke");
                    window.dispatch_keystroke(stroke, cx);
                }
            })
            .unwrap();
        let routed = routed.lock().unwrap().clone();
        assert!(
            routed.iter().any(|a| a.ends_with("TerminalCopy"))
                && routed.iter().any(|a| a.ends_with("TerminalPaste")),
            "⌘C / ⌘V in the terminal went to {routed:?}"
        );
    }

    /// Keyboard navigation end to end (issue #81), through the real bindings + key contexts:
    /// the Browse tree walks and opens with arrows/⏎, ⇧F10 opens the row's menu where →/←
    /// enter and leave the "New ▸" submenu and ⎋ closes it, space ticks a Commit-tree row,
    /// and the branch popup's ⏎ folds a section from its search box.
    #[gpui::test]
    fn keyboard_drives_trees_menus_and_popups(cx: &mut TestAppContext) {
        cx.update(|cx| apply_keymap(cx, &Keymap::default()));
        let (handle, _dir) = boot(cx);
        let press = |cx: &mut TestAppContext, keys: &str| {
            for k in keys.split(' ') {
                cx.update_window(handle.into(), |_, window, cx| {
                    let ks = gpui::Keystroke::parse(k).expect("valid keystroke");
                    window.dispatch_keystroke(ks, cx);
                })
                .unwrap();
                cx.refresh().unwrap(); // a just-opened menu takes focus on its first render
                cx.run_until_parked();
            }
        };

        // Browse tree: root, app.tsx, new.txt.
        handle
            .update(cx, |k, w, cx| k.act_mode_browse(&ModeBrowse, w, cx))
            .unwrap();
        press(cx, "down down enter");
        handle
            .update(cx, |k, _w, _cx| {
                assert_eq!(k.browse.open_path, Some(PathBuf::from("app.tsx")));
            })
            .unwrap();

        // ⇧F10 on the row → its menu, first entry ("New ▸") lit; → steps into the flyout.
        handle
            .update(cx, |k, w, cx| k.act_mode_browse(&ModeBrowse, w, cx))
            .unwrap();
        press(cx, "shift-f10 right");
        handle
            .update(cx, |k, _w, _cx| {
                let m = k.context_menu.as_ref().expect("menu open");
                assert!(matches!(&m.target, MenuTarget::BrowseFile(p, false) if p == &PathBuf::from("app.tsx")));
                assert!(m.submenu == Some(Submenu::New) && m.sub_hl == Some(0));
            })
            .unwrap();
        press(cx, "left escape");
        handle
            .update(cx, |k, w, _cx| {
                assert!(k.context_menu.is_none(), "⎋ after ← closes the menu");
                assert!(
                    k.browse.tree_focus.is_focused(w),
                    "focus returns to the tree"
                );
            })
            .unwrap();

        // Commit tree: cursor onto new.txt, space ticks it out of the commit.
        handle.update(cx, |k, _w, cx| k.enter_commit(cx)).unwrap();
        cx.refresh().unwrap();
        cx.run_until_parked();
        // Entering lands the caret in the message box; ⌘9 again hops to the tree.
        press(cx, "cmd-9");
        let new_txt = PathBuf::from("new.txt");
        let was = handle
            .update(cx, |k, _w, _cx| k.commit.checked.contains(&new_txt))
            .unwrap();
        press(cx, "down space"); // enter_commit selected the first change, app.tsx
        handle
            .update(cx, |k, _w, _cx| {
                assert_eq!(k.files[k.selected.unwrap()].path, new_txt);
                assert_ne!(k.commit.checked.contains(&new_txt), was, "space toggled it");
            })
            .unwrap();

        // Branch popup (focus stays in its search box): ↓↓↓ past New Branch + Pull onto the
        // Local section; ⏎ unfolds it.
        handle.update(cx, super::Kyde::toggle_branch_popup).unwrap();
        cx.run_until_parked();
        press(cx, "down down down enter");
        handle
            .update(cx, |k, _w, _cx| {
                assert_eq!(k.branch.hl, Some(2));
                assert!(k.branch.expanded.contains("sec:local"));
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
