//! `Kyde` state + controller logic — every non-render method (the `impl Kyde`
//! that holds refresh/select/stage/commit/navigation/etc). Split out of `main.rs`.
//! Sibling of `render.rs`; methods the view (or root) calls are `pub(crate)`.

use super::*;

/// Rows of context kept above the target line when auto-scrolling to a diff hunk or a
/// search hit, so it lands a few rows below the viewport top instead of pinned to it.
pub(crate) const SCROLL_CONTEXT_ROWS: usize = 3;
/// Debounce before the editable diff pane saves + re-diffs after a keystroke (the save +
/// `git status` + re-diff all shell out, so bursts of typing are coalesced).
pub(crate) const DIFF_EDIT_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(180);
/// Debounce before a Browse edit triggers a background `git status` refresh.
pub(crate) const STATUS_REFRESH_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(400);
/// Debounce before a Find-in-Files keystroke fires the background `git grep` (coalesces
/// bursts of typing — a full-repo grep is far too expensive to run per keystroke).
pub(crate) const CONTENT_SEARCH_DEBOUNCE: std::time::Duration =
    std::time::Duration::from_millis(200);
/// Minimum query length before Find-in-Files runs. 1-char queries match almost every line
/// in the repo (tens of MB of hits), so we wait until the query is specific enough.
pub(crate) const CONTENT_MIN_QUERY: usize = 2;
/// Max fuzzy-finder results rendered at once.
pub(crate) const FINDER_RESULT_CAP: usize = 50;

/// Recursively list files under `root` (repo-relative, sorted) for the Browse tree when the
/// folder is NOT a git repo — `git ls-files` can't drive it then, so we walk the filesystem
/// ourselves. Skips `.git` plus any directory named in the folder's `.gitignore` (simple,
/// non-glob name patterns) and the usual build/IDE noise, and caps the count so a stray huge
/// tree (e.g. a `target/` that wasn't ignored) can't hang the walk. Symlinks are not followed
/// (their `file_type` is neither file nor dir here), so the walk can't cycle.
fn list_dir_files(root: &std::path::Path) -> Vec<PathBuf> {
    const CAP: usize = 20_000;
    let mut skip_dirs: std::collections::HashSet<String> =
        [".git", "target", "dist", "node_modules"]
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
    if let Ok(gitignore) = std::fs::read_to_string(root.join(".gitignore")) {
        for line in gitignore.lines() {
            let l = line.trim();
            // Skip comments, blanks, and glob patterns (we only match plain dir names).
            if l.is_empty() || l.starts_with('#') || l.contains('*') {
                continue;
            }
            skip_dirs.insert(l.trim_start_matches('/').trim_end_matches('/').to_string());
        }
    }
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= CAP {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !skip_dirs.contains(&name) {
                    stack.push(entry.path());
                }
            } else if ft.is_file() {
                if let Ok(rel) = entry.path().strip_prefix(root) {
                    out.push(rel.to_path_buf());
                }
            }
        }
    }
    out.sort();
    out.truncate(CAP);
    out
}

/// A consistent read of everything `refresh` pulls from git + disk, computed on a
/// background thread so the (potentially many-subprocess, big-repo-slow) IO never blocks
/// the UI thread. `apply` writes it back into `Kyde` on the foreground. See
/// [`Kyde::refresh`].
struct RepoSnapshot {
    /// Changed files from `git status` (empty when `status_err` is set).
    files: Vec<ChangedFile>,
    /// Per-file `(added, removed)` line counts for `files` (see [`Repo::numstat`]).
    stats: std::collections::HashMap<std::path::PathBuf, (usize, usize)>,
    /// `Some(message)` when `git status` failed — surfaced as the op-error banner while
    /// the previous file list is kept (so a transient failure doesn't blank the tree).
    status_err: Option<String>,
    /// All tracked+untracked files (git) or the filesystem walk (non-git), for the tree.
    all_files: Vec<PathBuf>,
    /// Current branch (`None` when detached / non-git).
    current_branch: Option<String>,
    /// All worktrees of the repo, main first (empty when non-git). Drives the status-bar
    /// worktree chip and the branch popup's checked-out-elsewhere markers.
    worktrees: Vec<git::Worktree>,
    /// Commits ahead of the push base (`None` when non-git / unborn).
    ahead: Option<usize>,
    /// Commits behind the upstream (`None` when non-git / no upstream).
    behind: Option<usize>,
    /// The revision a push is measured against.
    push_base: String,
    /// Files a push would send.
    push_files: Vec<ChangedFile>,
    /// Scratch files under the project root.
    scratches: Vec<PathBuf>,
    /// What's being merged in when a merge is in progress (`MERGE_HEAD`'s friendly name)
    /// — drives the resolve-conflicts banner. `None` = no merge in progress.
    merging: Option<String>,
    /// Whether `root` is inside a git working tree (drives which fields are meaningful).
    is_git: bool,
    /// The project root this was read for, and the git toplevel `discover` found for it
    /// (`None` = not a git repo) — cached so `Kyde::repo()` needn't re-discover.
    top: (PathBuf, Option<PathBuf>),
}

impl RepoSnapshot {
    /// Do all the blocking git + filesystem reads for `root`. Safe to call off the UI
    /// thread — it touches only owned data (no `Kyde`, no gpui).
    fn read(root: &std::path::Path) -> Self {
        let scratches = scratch::list(root);
        let Ok(repo) = Repo::discover(root) else {
            // Not a git repo: Browse is still a file tree (walk the filesystem); all
            // git-only state stays empty.
            return Self {
                files: Vec::new(),
                stats: std::collections::HashMap::new(),
                status_err: None,
                all_files: list_dir_files(root),
                current_branch: None,
                worktrees: Vec::new(),
                ahead: None,
                behind: None,
                push_base: String::new(),
                push_files: Vec::new(),
                scratches,
                merging: None,
                is_git: false,
                top: (root.to_path_buf(), None),
            };
        };
        // `git status` failing means we can't trust the file list — carry the message so
        // `apply` keeps the old list and shows the banner rather than a looks-clean tree.
        let (files, status_err) = match repo.status() {
            Ok(files) => (files, None),
            Err(e) => (Vec::new(), Some(format!("Reading status failed: {e}"))),
        };
        // One push-base resolution feeds both (`push_files()` would resolve it again).
        let push_base = repo.push_base();
        Self {
            files,
            stats: repo.numstat().unwrap_or_default(),
            status_err,
            all_files: repo.list_files().unwrap_or_default(),
            current_branch: repo.current_branch(),
            worktrees: repo.worktrees().unwrap_or_default(),
            ahead: repo.ahead_count(),
            behind: repo.behind_count(),
            push_files: repo.diff_files(&push_base, Some("HEAD"), None),
            push_base,
            scratches,
            merging: repo.merging(),
            is_git: true,
            top: (root.to_path_buf(), Some(repo.root().to_path_buf())),
        }
    }
}

impl Kyde {
    pub(crate) fn new(
        root: Option<PathBuf>,
        keymap: Keymap,
        first_run: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let keymap_preset = keymap.preset;
        let plugins_query = cx.new(|cx| CodeEditor::single_line(cx, "Search plugins…"));
        let name_input = cx.new(|cx| CodeEditor::single_line(cx, "File name"));
        let project_search = cx.new(|cx| CodeEditor::single_line(cx, "Search projects"));
        cx.subscribe(&project_search, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) {
                this.projects_hl = None;
                cx.notify();
            }
        })
        .detach();
        // Re-filter the plugin manager list as its search box changes. Notifying Kyde
        // repaints the modal window (it observes Kyde).
        cx.subscribe(&plugins_query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.plugins_win.is_some() {
                cx.notify();
            }
        })
        .detach();

        let mut me = Self {
            // A path arg opens straight into that project, so it's the first open tab.
            open_projects: root.iter().cloned().collect(),
            project_sessions: std::collections::HashMap::new(),
            repo_root: root,
            repo_top: None,
            mode: Mode::Browse, // code-first: a freshly opened project shows the editor
            focus_handle: cx.focus_handle(),
            keymap,
            plugins: Plugins::load(),
            ignored_packs: std::collections::HashSet::new(),
            recents: Recents::load(),
            project_search,
            projects_hl: None,
            files: Vec::new(),
            stats: std::collections::HashMap::new(),
            selected: None,
            commit: CommitView::new(cx),
            diff: DiffPanes::new(cx),
            divider_drag: None,
            sb_drag: None,
            scroll_dims: std::collections::HashMap::new(),
            projects_search_focused: false,
            browse: BrowseView::new(cx),
            find: FindBar::new(cx),
            fps: Fps {
                show: load_show_fps(),
                value: 0.0,
                shown: 0.0,
                last: None,
                shown_at: None,
            },
            finder: Finder::new(cx),
            onboarding: Onboarding {
                open: first_run,
                forced: first_run,
                choice: keymap_preset,
                install_cmd: true,
                shell_cmd_error: None,
            },
            plugins_win: None,
            plugins_query,
            fonts_win: None,
            clear_data_win: None,
            clear_lh_win: None,
            remove_wt_win: None,
            scrollback_win: None,
            cleanup_win: None,
            settings_win: None,
            settings_section: SettingsSection::Appearance,
            settings_theme: ui::SelectState::new(cx),
            keymap_editor: KeymapEditor::new(cx),
            font_preview: None,
            welcome_frame: 0,
            pending_crash: crash_log_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .filter(|s| !s.trim().is_empty()),
            folder_picker_open: false,
            op_error: None,
            pending_error: None,
            context_menu: None,
            menu_focus: cx.focus_handle(),
            menu_focus_pending: false,
            menu_return_focus: None,
            main_window_bounds: None,
            chrome: chrome::WindowChrome::default(),
            diff_view_open: false,
            rollback_win: None,
            new_branch_win: None,
            new_branch_checkout: true,
            new_branch_overwrite: false,
            delete_target: None,
            name_prompt: None,
            name_input,
            rollback_checked: std::collections::HashSet::new(),
            rollback_delete_added: false,
            is_git: true,
            current_branch: None,
            branch: BranchPopup::new(cx),
            worktree: WorktreePopup::new(cx),
            refresh_gen: 0,
            sync: SyncState::new(),
            push_win: None,
            git_tab: GitTab::Commit,
            update_available: None,
            updating: false,
            history: HistoryView::new(cx),
            merge_win: None,
            merge: MergeView::new(cx),
            compare_win: None,
            compare: CompareView::new(cx),
            changelog_win: None,
            changelog: ChangelogView::new(cx),
            local_history_win: None,
            lh: LocalHistoryView::new(cx),
            fs_watcher: None,
            watch_root: None,
            watch_off: None,
            term: TermState::new(),
            agents: AgentsView::new(cx),
        };
        me.refresh(cx);
        // Learn every worktree's Claude sessions now: the first read reopens last session's
        // agent tabs in the background (`agents_restore_saved`), whatever view is showing.
        me.agents_refresh_contexts(cx);
        // An Agents-only window opens straight into the Agents view (no project needed).
        if me.agents.cfg.agents_only {
            me.enter_agents(cx);
        }
        // Background: ask GitHub if there's a newer release, then surface the update banner.
        // Network I/O off the UI thread; failures stay silent.
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { update::check().ok().flatten() })
                .await;
            if let Some(rel) = found {
                this.update(cx, |this, cx| {
                    this.update_available = Some(rel);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        me
    }

    /// The open project's folder name — the label of every tree's root row.
    pub(crate) fn project_name(&self) -> String {
        self.repo_root
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or_else(|| "/".to_string(), |s| s.to_string_lossy().into_owned())
    }

    /// The open project's repo. Uses the toplevel the last status read discovered for this
    /// root (no subprocess — this is called on every save, select and click), falling back
    /// to `git rev-parse` only before the first read lands or after a project switch.
    pub(crate) fn repo(&self) -> Option<Repo> {
        let root = self.repo_root.as_ref()?;
        match &self.repo_top {
            Some((r, top)) if r == root => top.as_ref().map(|t| Repo::at(t.clone())),
            _ => Repo::discover(root).ok(),
        }
    }

    /// Re-read git status + the file tree. The reads (a handful of `git` subprocesses, plus
    /// a `git ls-files` / filesystem walk that is O(repo size)) run on a **background
    /// thread** — on a large repo they take real time, and doing them inline used to freeze
    /// the UI on every startup, window-refocus, and save. A [`RepoSnapshot`] is computed off
    /// thread, then applied on the foreground; a monotonic `refresh_gen` drops any snapshot
    /// that a newer refresh has already superseded.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        // Keep the local-history store pointed at the open project (cheap when unchanged).
        self.lh_sync_store(cx);
        // Keep the filesystem watcher armed on the open project (cheap when unchanged) so
        // external creates/edits trigger a refresh without a window refocus.
        self.sync_fs_watcher(cx);
        let Some(root) = self.repo_root.clone() else {
            // Landing view / project closed: no repo to read — clear git-derived state so a
            // stale tree/branch never lingers behind the landing view.
            self.files.clear();
            self.stats.clear();
            self.sync.set_push_files(Vec::new());
            self.is_git = false;
            self.current_branch = None;
            self.worktree.list.clear();
            self.worktree.popup_open = false;
            self.sync.ahead = None;
            self.sync.behind = None;
            self.op_error = None;
            self.browse.all_files.clear();
            self.browse.extra_dirs.clear();
            self.browse.tree = tree::Tree::default();
            self.browse.scratches.clear();
            self.rebuild_commit_view(false);
            self.selected = None;
            self.diff.current = None;
            cx.notify();
            return;
        };
        self.refresh_gen = self.refresh_gen.wrapping_add(1);
        let generation = self.refresh_gen;
        cx.spawn(async move |this, cx| {
            let snapshot = cx
                .background_executor()
                .spawn(async move { RepoSnapshot::read(&root) })
                .await;
            this.update(cx, |this, cx| {
                // Only the newest refresh wins — a snapshot superseded by a later refresh is
                // dropped rather than clobbering fresher state.
                if this.refresh_gen == generation {
                    this.apply_snapshot(snapshot, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Write a background [`RepoSnapshot`] into `self` and rebuild the derived UI state
    /// (commit tree + current selection). Runs on the foreground (needs `&mut self`).
    fn apply_snapshot(&mut self, snap: RepoSnapshot, cx: &mut Context<Self>) {
        // `git ls-files` keeps listing a tracked file whose working copy was deleted —
        // showing a nonexistent file in the Browse tree (and ⌘P) reads as a bug. Drop
        // Deleted-status paths; they still show in the Commit view (the change itself).
        let deleted: std::collections::HashSet<&PathBuf> = snap
            .files
            .iter()
            .filter(|f| f.status == FileStatus::Deleted)
            .map(|f| &f.path)
            .collect();
        self.browse.all_files = snap
            .all_files
            .into_iter()
            .filter(|p| !deleted.contains(p))
            .collect();
        // User-created folders stay visible while still empty (file-derived trees
        // can't see them); pruned once a file exists under them or they're gone.
        if let Some(root) = self.repo_root.clone() {
            let has_file_under =
                |d: &PathBuf| self.browse.all_files.iter().any(|f| f.starts_with(d));
            self.browse
                .extra_dirs
                .retain(|d| root.join(d).is_dir() && !has_file_under(d));
        } else {
            self.browse.extra_dirs.clear();
        }
        self.browse.tree =
            tree::Tree::build_with_dirs(&self.browse.all_files, &self.browse.extra_dirs);
        self.is_git = snap.is_git;
        self.repo_top = Some(snap.top);
        self.current_branch = snap.current_branch;
        self.worktree.list = snap.worktrees;
        // Merge-in-progress state (drives the banner). Covers merges we started AND ones
        // started outside (a conflicted `git pull`/`merge` in a terminal); cleared when
        // the merge concludes/aborts anywhere. Our initiated `source` label wins while
        // both are known (it's the exact branch name the user clicked).
        self.merge.in_progress = snap.merging;
        if self.merge.in_progress.is_none() {
            self.merge.source = None;
        }
        self.sync.ahead = snap.ahead;
        self.sync.behind = snap.behind;
        self.browse.scratches = snap.scratches;
        if snap.is_git {
            // What a push would send — kept live so the Push tab's count badge is accurate.
            self.sync.push_base = snap.push_base;
            self.sync.set_push_files(snap.push_files);
            if let Some(msg) = snap.status_err {
                // Keep the previous file list on a transient failure (see `RepoSnapshot`);
                // a later success clears the banner.
                eprintln!("{msg}");
                self.op_error = Some(msg);
            } else {
                self.files = snap.files;
                self.stats = snap.stats;
                self.op_error = None;
            }
        } else {
            self.files.clear();
            self.stats.clear();
            self.sync.set_push_files(Vec::new());
            self.op_error = None;
        }
        self.rebuild_commit_view(false);
        // Re-selecting keeps the COMMIT view's working diff in sync — but the panes are
        // shared with History/Push (committed content). Leave those alone, or a clean-tree
        // refresh blanks an open history diff to "Select a file" on window refocus (#39).
        // History's "Local" compare is editable, hence the mode check on top of `readonly`.
        // In Browse the panes aren't on screen at all — skip the re-diff (a `git show` +
        // full diff on every save's refresh); entering the Commit view re-selects anyway.
        let panes_foreign =
            self.diff.readonly || (self.mode != Mode::Commit && !self.diff_view_open);
        if panes_foreign {
            // Just keep the commit-view selection index valid (it seeds the next entry).
            if self.selected.is_none_or(|i| i >= self.files.len()) {
                self.selected = (!self.files.is_empty()).then_some(0);
            }
        } else {
            match self.selected {
                Some(i) if i < self.files.len() => self.select(i),
                _ if !self.files.is_empty() => self.select(0),
                _ => {
                    self.selected = None;
                    self.diff.current = None;
                }
            }
        }
        // The fuzzy finder matches against `all_files` — if it's open while this snapshot
        // lands (e.g. opened right after boot, before the first background read finished),
        // its cached results are stale/empty and nothing else would recompute them until
        // the next keystroke. Content mode is untouched (its grep is repo-side + debounced).
        if self.finder.open && self.finder.mode != FinderMode::Content {
            self.recompute_finder(cx);
        }
        // Re-apply an operation error stashed alongside this refresh (see `pending_error`),
        // now that the status read has cleared `op_error`, so it survives the clear.
        if let Some(msg) = self.pending_error.take() {
            self.op_error = Some(msg);
        }
        // Feed external (out-of-Kyde) creates/edits into local history. Runs on every
        // refresh — window refocus, terminal output, the filesystem watcher — so a file
        // changed outside Kyde lands in the timeline even if it's never (re)opened here.
        self.lh_scan_external(cx);
        cx.notify();
    }

    /// Re-read git + the open file from disk. Triggered when the window regains focus,
    /// since an external tool (another editor, a branch switch, a rebase, etc.) may have
    /// changed files behind our back.
    pub(crate) fn reload_external(&mut self, cx: &mut Context<Self>) {
        if self.repo_root.is_none() {
            return; // Projects landing — nothing to reload.
        }
        // git status, file tree, and the selected file's diff (all read fresh from disk/git).
        self.refresh(cx);

        // Reload the Browse editor's open file — but only when the user has no unsaved
        // edits (never clobber), the file still exists, and the on-disk bytes actually
        // changed (avoid pointless cursor/selection resets).
        if let (Some(rel), Some(repo)) = (self.browse.open_path.clone(), self.repo()) {
            let exists = repo.root().join(&rel).exists();
            if exists && !self.browse.editor.read(cx).dirty {
                if let Ok(content) = repo.working_content(&rel) {
                    if self.browse.editor.read(cx).text() != content {
                        let lang = self.effective_lang(&rel);
                        self.browse
                            .editor
                            .update(cx, |e, cx| e.set_content(content, lang, cx));
                    }
                }
            }
        }
        // Same for the Commit view's side-by-side panes. `refresh` re-selects with cx=None
        // on purpose — that updates only the diff MODEL, never the pane editors, so the
        // debounced background refresh can't stomp mid-typing. But on an explicit window
        // refocus the panes must pick up external edits like the Browse editor does: reload
        // them under the same never-clobber rules (no unsaved pane edits, bytes actually
        // changed on disk).
        // Commit TAB only — on the Push tab the panes hold a committed diff, and reloading
        // would swap in an unrelated working-tree file.
        if self.mode == Mode::Commit
            && self.git_tab == GitTab::Commit
            && !self.diff.right.read(cx).dirty
        {
            if let (Some(i), Some(repo)) = (self.selected, self.repo()) {
                let changed = self
                    .files
                    .get(i)
                    .and_then(|f| repo.working_content(&f.path).ok())
                    .is_some_and(|c| self.diff.right.read(cx).text() != c);
                if changed {
                    self.select_with(i, Some(cx));
                }
            }
        }
        // History's working-tree compares ("Local"/"Before → Local") show the working copy
        // in the right pane too — reload it under the same never-clobber rules.
        if self.mode == Mode::History
            && matches!(
                self.history.compare,
                CompareMode::Local | CompareMode::BeforeLocal
            )
            && !self.diff.right.read(cx).dirty
        {
            if let (Some(i), Some(repo)) = (self.history.file_selected, self.repo()) {
                let changed = self
                    .history
                    .files
                    .get(i)
                    .and_then(|f| repo.working_content(&f.path).ok())
                    .is_some_and(|c| self.diff.right.read(cx).text() != c);
                if changed {
                    self.select_history_file(i, cx);
                }
            }
        }
        // An external change may have emptied the active git tab — keep it valid.
        self.normalize_git_tab(cx);
        cx.notify();
    }

    /// Arm (or drop) the filesystem watcher so it tracks the open project — cheap when the
    /// root is unchanged (the common refresh case). The watcher runs its own thread; on an
    /// event it only forwards a wake to a foreground pump, which debounces into
    /// `schedule_status_refresh`. That refresh re-reads git status and, via `apply_snapshot`
    /// → `lh_scan_external`, feeds external creates/edits into local history — so nothing
    /// waits for a window refocus. Pure `.git`-internal churn (index/refs/logs from our own
    /// git subprocesses) is filtered; real worktree changes from a checkout still fire.
    pub(crate) fn sync_fs_watcher(&mut self, cx: &mut Context<Self>) {
        use futures::StreamExt as _;
        use notify::{RecursiveMode, Watcher};
        let root = self.repo_root.clone();
        if self.watch_root == root {
            return; // already watching the right project
        }
        self.watch_root.clone_from(&root);
        self.fs_watcher = None; // drop the previous watcher (stops its thread)
        self.watch_off = None;
        let Some(root) = root else { return };
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            // Skip events that touch ONLY `.git`-internal paths — our own git commands
            // churn the index/refs constantly. An empty path list (a backend rescan) still
            // passes through, as do real worktree writes accompanying a checkout.
            let only_git = !ev.paths.is_empty()
                && ev
                    .paths
                    .iter()
                    .all(|p| p.components().any(|c| c.as_os_str() == ".git"));
            if !only_git {
                let _ = tx.unbounded_send(());
            }
        });
        // Path vanished / OS watch limit — stay on focus-refresh only (and say so).
        let mut watcher = match watcher {
            Ok(w) => w,
            Err(e) => return self.note_live_refresh_off(&e, &root),
        };
        if let Err(e) = watcher.watch(&root, RecursiveMode::Recursive) {
            return self.note_live_refresh_off(&e, &root);
        }
        self.fs_watcher = Some(watcher);
        let watched = root;
        cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                // Debounce the burst HERE (not via `schedule_status_refresh`, which bumps
                // `refresh_gen` up front and would cancel a concurrent op-driven refresh):
                // drain, wait a beat, drain again, then refresh once. `refresh_gen` only
                // moves when the refresh actually runs, so nothing in flight is dropped.
                while rx.try_recv().is_ok() {}
                cx.background_executor()
                    .timer(STATUS_REFRESH_DEBOUNCE)
                    .await;
                while rx.try_recv().is_ok() {}
                let alive = this
                    .update(cx, |this, cx| {
                        // Still the same project? (The user may have switched away.)
                        if this.watch_root.as_deref() == Some(watched.as_path()) {
                            this.refresh(cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break; // the app is gone — end the pump
                }
            }
        })
        .detach();
    }

    /// The OS refused to watch the project, so outside edits show up only when Kyde's window
    /// is focused again: the status bar says so for as long as that project is open. A
    /// folder that vanished needs no note.
    fn note_live_refresh_off(&mut self, e: &notify::Error, root: &std::path::Path) {
        self.watch_off = live_refresh_off(e, root).map(|why| {
            eprintln!("Live refresh off: {why}");
            why.into()
        });
    }

    /// Record a failed git operation so the user sees it (op-error banner) instead of a
    /// silent no-op. `ctx` is a short human label ("Commit", "Push", …); the error is
    /// stringified after it. Still logs to stderr for debugging.
    pub(crate) fn fail(&mut self, ctx: &str, e: impl Into<anyhow::Error>) {
        let e = e.into();
        eprintln!("{ctx} failed: {e:#}");
        self.op_error = Some(format!("{ctx} failed: {e}"));
    }

    /// Like [`fail`](Self::fail), but for an operation error raised alongside a `refresh`
    /// (see [`pending_error`](Self::pending_error)). Setting `op_error` directly would be
    /// wiped when the async refresh's clean status read lands; stashing it here lets
    /// `apply_snapshot` re-apply it after that clear, so the banner survives the refresh.
    pub(crate) fn fail_pending(&mut self, ctx: &str, e: impl Into<anyhow::Error>) {
        let e = e.into();
        eprintln!("{ctx} failed: {e:#}");
        self.pending_error = Some(format!("{ctx} failed: {e}"));
    }

    /// Reset the editor to nothing-open.
    pub(crate) fn clear_open(&mut self, cx: &mut Context<Self>) {
        self.browse.open_path = None;
        self.browse.preview_tab = None;
        self.browse.editor.update(cx, |e, cx| {
            e.set_content(String::new(), Lang::PlainText, cx);
        });
    }

    /// The language to actually highlight with: the file's detected language if
    /// its pack is installed (or it needs no pack), else `PlainText` — so an
    /// un-installed type renders fast and unparsed until the user opts in.
    pub(crate) fn effective_lang(&self, rel: &std::path::Path) -> Lang {
        let lang = Lang::from_path(rel);
        match lang.pack() {
            Some(p) if !self.plugins.is_installed(p.id) => Lang::PlainText,
            _ => lang,
        }
    }

    /// Whether error highlighting (parse-error squiggles) is opted in for `lang`:
    /// its pack installed AND the pack's error toggle on. `PlainText` and the
    /// builtin line-highlighter langs have no pack → always false.
    pub(crate) fn errors_enabled_for(&self, lang: Lang) -> bool {
        lang.pack()
            .is_some_and(|p| self.plugins.is_installed(p.id) && self.plugins.errors_on(p.id))
    }

    /// Whether ⌘-click import navigation is on for `lang` (pack installed AND
    /// the pack's links toggle on — default on, per-pack opt-out).
    pub(crate) fn links_enabled_for(&self, lang: Lang) -> bool {
        lang.pack()
            .is_some_and(|p| self.plugins.is_installed(p.id) && self.plugins.links_on(p.id))
    }

    /// Persist `rel`'s `text` to disk: through the repo's working tree in a git repo, else
    /// straight to disk under the project root (non-git Browse). Absolute paths (scratch
    /// files) write to themselves, since `join` keeps an absolute right-hand side. Returns
    /// the error so callers can surface it (a swallowed write means silently-lost edits).
    pub(crate) fn write_open_file(&self, rel: &std::path::Path, text: &str) -> anyhow::Result<()> {
        if let Some(repo) = self.repo() {
            repo.save_file(rel, text)?;
        } else if let Some(root) = self.repo_root.as_ref() {
            let full = root.join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(full, text)?;
        }
        Ok(())
    }

    /// Write the open file to disk without the git refresh — cheap enough to run on
    /// every keystroke. The changed-files tree re-syncs on mode switch / window refocus.
    pub(crate) fn autosave(&mut self, cx: &mut Context<Self>) {
        let (Some(rel), text) = (
            self.browse.open_path.clone(),
            self.browse.editor.read(cx).text().to_string(),
        ) else {
            return;
        };
        // A failed autosave means the user's keystrokes never reached disk — surface it
        // (op-error banner) and keep the buffer dirty so a later save can retry, rather
        // than silently marking it clean and losing the edit.
        if let Err(e) = self.write_open_file(&rel, &text) {
            self.fail("Saving file", e);
            cx.notify();
            return;
        }
        self.browse.editor.update(cx, |e, _| e.dirty = false);
        // Local history: the save is on disk — mark it for the throttled snapshot flush.
        self.lh_note_save(&rel, cx);
        // Optimistic status: flip the tree/tab color to "modified" the instant we save,
        // rather than waiting ~0.4s for the debounced `git status`. Only when the file isn't
        // already a known change — so a real Added/Untracked/Deleted status (e.g. a new file
        // shown green) is never clobbered; the debounced refresh reconciles the rest (and
        // clears it if an undo brings the file back to its committed contents).
        if !self.files.iter().any(|f| f.path == rel) {
            self.files.push(ChangedFile {
                path: rel.clone(),
                status: FileStatus::Modified,
            });
            cx.notify();
        }
        // The bytes are on disk now; refresh git status (tree/tab colors, commit
        // view) on a debounce so per-keystroke typing never pays for `git status`.
        self.schedule_status_refresh(cx);
    }

    fn save_open(&mut self, cx: &mut Context<Self>) {
        let (Some(rel), text) = (
            self.browse.open_path.clone(),
            self.browse.editor.read(cx).text().to_string(),
        ) else {
            return;
        };
        if let Err(e) = self.write_open_file(&rel, &text) {
            self.fail("Saving file", e);
            return;
        }
        self.browse.editor.update(cx, |e, _| e.dirty = false);
        self.lh_note_save(&rel, cx);
        self.refresh(cx);
    }

    // ── in-editor find / replace ──────────────────────────────────
    pub(crate) fn act_toggle_fps(
        &mut self,
        _: &ToggleFps,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fps.show = !self.fps.show;
        self.fps.last = None;
        save_show_fps(self.fps.show); // remember across launches
        cx.notify();
    }

    /// Escape: close whatever overlay is open (most-transient first); if none, cancel the
    /// Commit view back to Browse. A no-op in plain Browse.
    pub(crate) fn act_escape(
        &mut self,
        _: &EscapeKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.agents.renaming.is_some() {
            self.agents_finish_rename(false, cx);
        } else if self.context_menu.is_some() {
            self.context_menu = None;
        } else if self.find_shown(FindHost::Main) {
            self.close_find(&CloseFind, window, cx);
            return;
        } else if self.diff_view_open {
            self.diff_view_open = false; // Escape leaves the full-screen Show-Diff view
        } else if self.delete_target.is_some() {
            self.delete_target = None;
        } else if self.branch.popup_open {
            self.branch.popup_open = false;
        } else if self.worktree.popup_open {
            self.worktree.popup_open = false;
        } else if self.onboarding.open && !self.onboarding.forced {
            self.onboarding.open = false;
        } else if self.mode == Mode::Commit {
            self.mode = Mode::Browse; // Escape = Cancel in the Commit view
        } else {
            return; // nothing to close
        }
        window.focus(&self.focus_handle);
        cx.notify();
    }

    /// Enter at the app root: confirm the open confirmation dialog (currently the
    /// Delete overlay — its Escape-cancel twin lives in `act_escape`). The native
    /// confirm windows (New Branch / Rollback / Clear Data) handle Enter themselves
    /// in `ModalWindow::render`. No dialog open → no-op.
    pub(crate) fn act_confirm(&mut self, _: &ConfirmKey, _: &mut Window, cx: &mut Context<Self>) {
        if self.agents.renaming.is_some() {
            self.agents_finish_rename(true, cx);
        } else if self.delete_target.is_some() {
            self.do_delete(cx);
        }
    }

    // ── configurable action handlers ──────────────────────────────
    pub(crate) fn act_save(&mut self, _: &SaveFile, _: &mut Window, cx: &mut Context<Self>) {
        self.save_open(cx);
        cx.notify();
    }
    /// ⌘W — close the active editor tab (no-op when nothing is open).
    pub(crate) fn act_close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(idx) = self
            .browse
            .open_path
            .as_ref()
            .and_then(|p| self.browse.open_tabs.iter().position(|t| t == p))
        {
            self.close_tab(idx, cx);
        }
    }
    /// ⌘1 / ⇧⌘E — the Browse view with the keyboard in its file tree (IDE "focus project
    /// view"), its cursor on the open file when nothing is selected yet.
    pub(crate) fn act_mode_browse(
        &mut self,
        _: &ModeBrowse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.switch_mode(Mode::Browse, cx);
        if self.browse.selected_path.is_none() {
            self.browse.selected_path.clone_from(&self.browse.open_path);
        }
        window.focus(&self.browse.tree_focus);
    }

    /// The single path EVERY top-level view switch goes through — the activity-rail buttons and
    /// the ⌘ shortcuts. Un-maximizes the terminal first: a maximized terminal fills the whole
    /// right column, so without this, switching to Browse/Commit/History would do nothing
    /// visible (the chosen view stays hidden behind the terminal). Centralised so the reset is
    /// consistent across triggers and unit-testable.
    pub(crate) fn switch_mode(&mut self, to: Mode, cx: &mut Context<Self>) {
        // Un-maximize the terminal first — `term.panel` is always compiled, so no cfg gate.
        self.term.panel.maximized = false;
        match to {
            Mode::Commit => self.enter_commit(cx),
            Mode::History => self.enter_history(cx),
            Mode::Browse => {
                self.mode = Mode::Browse;
                self.diff_view_open = false;
                cx.notify();
            }
            Mode::Agents => self.enter_agents(cx),
        }
    }
}

/// Why live refresh is off for a project the OS won't watch (the status bar's tooltip), or
/// `None` when there is nothing to say (the folder vanished). Linux's inotify needs one watch
/// per folder, so a big tree (`node_modules`, a monorepo) can exhaust the per-user limit,
/// which macOS's `FSEvents` doesn't have.
fn live_refresh_off(e: &notify::Error, root: &std::path::Path) -> Option<String> {
    use notify::ErrorKind;
    let why = match &e.kind {
        ErrorKind::PathNotFound => return None,
        ErrorKind::MaxFilesWatch => {
            "The system's file-watch limit is reached (raise fs.inotify.max_user_watches)"
                .to_string()
        }
        kind => {
            let cause = match kind {
                ErrorKind::Io(io) => io.to_string(),
                ErrorKind::Generic(msg) => msg.clone(),
                _ => e.to_string(),
            };
            // The folder it stopped at, relative to the project (e.g. one Kyde can't read).
            match e.paths.first().and_then(|p| p.strip_prefix(root).ok()) {
                Some(p) if !p.as_os_str().is_empty() => {
                    format!("Can't watch {}: {cause}", p.display())
                }
                _ => format!("Can't watch this project: {cause}"),
            }
        }
    };
    Some(format!(
        "{why}. Outside changes show when you switch back to Kyde."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refused watch is explained, with the Linux limit spelled out; a vanished folder isn't.
    #[test]
    fn a_refused_watch_says_live_refresh_is_off() {
        use notify::{Error, ErrorKind};
        let root = std::path::Path::new("/p");
        let limit = live_refresh_off(&Error::new(ErrorKind::MaxFilesWatch), root).unwrap();
        assert!(limit.contains("file-watch limit"), "{limit}");
        assert!(limit.contains("fs.inotify.max_user_watches"), "{limit}");
        let denied = || Error::io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let at = live_refresh_off(&denied().add_path("/p/db/data".into()), root).unwrap();
        assert!(
            at.starts_with("Can't watch db/data: permission denied"),
            "{at}"
        );
        let anywhere = live_refresh_off(&denied(), root).unwrap();
        assert!(
            anywhere.starts_with("Can't watch this project"),
            "{anywhere}"
        );
        assert_eq!(live_refresh_off(&Error::path_not_found(), root), None);
    }
}
