//! `Kyde` state + controller logic — every non-render method (the `impl Kyde`
//! that holds refresh/select/stage/commit/navigation/etc). Split out of `main.rs`.
//! Sibling of `render.rs`; methods the view (or root) calls are `pub(crate)`.

use super::*;

/// Rows of context kept above the target line when auto-scrolling to a diff hunk or a
/// search hit, so it lands a few rows below the viewport top instead of pinned to it.
pub(crate) const SCROLL_CONTEXT_ROWS: usize = 3;
/// Debounce before the editable diff pane saves + re-diffs after a keystroke (the save +
/// `git status` + re-diff all shell out, so bursts of typing are coalesced).
const DIFF_EDIT_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(180);
/// Debounce before a Browse edit triggers a background `git status` refresh.
pub(crate) const STATUS_REFRESH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);
/// Debounce before a Find-in-Files keystroke fires the background `git grep` (coalesces
/// bursts of typing — a full-repo grep is far too expensive to run per keystroke).
pub(crate) const CONTENT_SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);
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
            .map(|s| s.to_string())
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

impl Kyde {
    pub(crate) fn new(
        root: Option<PathBuf>,
        keymap: Keymap,
        first_run: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let keymap_preset = keymap.preset;
        let commit_editor = cx.new(|cx| {
            let mut e = CodeEditor::new(cx, String::new(), Lang::PlainText, "Commit message…");
            e.fill_height = true; // fill the box so the whole area is clickable
            e.soft_wrap = true; // wrap long commit messages instead of running off the box
            e
        });
        // No placeholder: an empty open file should read as empty, not show prompt text.
        let file_editor = cx.new(|cx| CodeEditor::new(cx, String::new(), Lang::PlainText, ""));
        // Diff panes: left read-only (base), right editable (working copy, live-saves). The
        // base pane renders its line numbers on the RIGHT, toward the center gutter, so the
        // two panes' numbers meet in the middle (IntelliJ/GitHub side-by-side style).
        let diff_left = cx.new(|cx| {
            let mut e = CodeEditor::read_only(cx, String::new(), Lang::PlainText);
            e.gutter_right = true;
            e
        });
        let diff_right = cx.new(|cx| CodeEditor::new(cx, String::new(), Lang::PlainText, ""));
        cx.subscribe(&diff_right, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.diff_right.read(cx).dirty {
                // Debounce: typing fires Changed per keystroke, but the save + `git status`
                // + full re-diff are expensive (subprocess!). Only run them after the last
                // keystroke settles, so typing stays responsive even on large files.
                this.diff_edit_gen = this.diff_edit_gen.wrapping_add(1);
                let gen = this.diff_edit_gen;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(DIFF_EDIT_DEBOUNCE).await;
                    this.update(cx, |this, cx| {
                        if this.diff_edit_gen == gen {
                            this.diff_autosave(cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
        })
        .detach();
        // Auto-save: persist every edit to disk immediately (no Save button). Gated on
        // `dirty` so loading a file (set_content emits Changed with dirty=false) doesn't
        // rewrite it; real edits/undo set dirty=true and flush here.
        cx.subscribe(&file_editor, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.file_editor.read(cx).dirty {
                // Editing a preview (temporary) tab promotes it to a permanent tab — VS Code
                // behaviour, so the edit survives the next single-click elsewhere.
                if this.preview_tab.is_some() && this.preview_tab == this.open_path {
                    this.preview_tab = None;
                }
                this.autosave(cx);
            }
        })
        .detach();
        let finder_query = cx.new(|cx| CodeEditor::single_line(cx, "Type to search files…"));
        let plugins_query = cx.new(|cx| CodeEditor::single_line(cx, "Search plugins…"));
        let name_input = cx.new(|cx| CodeEditor::single_line(cx, "File name"));
        // Find / replace bar inputs use the "FindBar" key context (enter/escape bindings).
        let find_query = cx.new(|cx| {
            let mut e = CodeEditor::single_line(cx, "Find");
            e.ctx_override = Some("FindBar");
            e
        });
        let replace_query = cx.new(|cx| {
            let mut e = CodeEditor::single_line(cx, "Replace");
            e.ctx_override = Some("FindBar");
            e
        });
        cx.subscribe(&find_query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.find_open {
                this.recompute_find(cx);
            }
        })
        .detach();
        // History branch-picker filter; re-render the dropdown live as it changes.
        let history_branch_query = cx.new(|cx| CodeEditor::single_line(cx, "Search branches…"));
        cx.subscribe(&history_branch_query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.history_branch_open {
                cx.notify();
            }
        })
        .detach();
        // Commit-list filter; re-render the history view live as it changes.
        let history_commit_query = cx.new(|cx| CodeEditor::single_line(cx, "Search commits…"));
        cx.subscribe(&history_commit_query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.mode == Mode::History {
                cx.notify();
            }
        })
        .detach();
        // History files-tree filter.
        let history_files_query = cx.new(|cx| CodeEditor::single_line(cx, "Search files…"));
        cx.subscribe(&history_files_query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.mode == Mode::History {
                cx.notify();
            }
        })
        .detach();
        let project_search = cx.new(|cx| CodeEditor::single_line(cx, "Search projects"));
        let branch_query = cx.new(|cx| CodeEditor::single_line(cx, "Search / new branch name"));
        // Re-filter the branch popup live as the query changes.
        cx.subscribe(&branch_query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.branch_popup_open {
                cx.notify();
            }
        })
        .detach();
        cx.subscribe(&project_search, |_this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        // Commit-view file filter — repaint the changed-files list as the query changes.
        let commit_search = cx.new(|cx| CodeEditor::single_line(cx, "Search files…"));
        cx.subscribe(&commit_search, |_this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) {
                cx.notify();
            }
        })
        .detach();

        // Re-query the finder whenever its input changes.
        cx.subscribe(&finder_query, |this, _e, ev, cx| {
            if matches!(ev, EditorEvent::Changed) && this.finder_open {
                // Find-in-Files shells out to `git grep` (expensive on large repos), so it's
                // debounced + run on a background thread. Every other mode is an in-memory
                // fuzzy match — cheap, run inline.
                if this.finder_mode == FinderMode::Content {
                    this.schedule_content_search(cx);
                } else {
                    this.recompute_finder(cx);
                    cx.notify();
                }
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
            mode: Mode::Browse, // code-first: a freshly opened project shows the editor
            focus_handle: cx.focus_handle(),
            focus_commit_msg: false,
            keymap,
            plugins: Plugins::load(),
            ignored_packs: std::collections::HashSet::new(),
            recents: Recents::load(),
            project_search,
            commit_search,
            files: Vec::new(),
            selected: None,
            commit_focus: std::collections::HashSet::new(),
            commit_tree: tree::Tree::default(),
            commit_expanded: std::collections::HashSet::new(),
            commit_checked: std::collections::HashSet::new(),
            current_diff: None,
            old_spans: Vec::new(),
            new_spans: Vec::new(),
            commit_editor,
            diff_left,
            diff_right,
            diff_path: None,
            diff_image: None,
            diff_readonly: false,
            diff_base: String::new(),
            diff_scroll: ScrollHandle::new(),
            diff_split: 0.5,
            divider_drag: None,
            file_scroll: ScrollHandle::new(),
            sb_drag: None,
            scroll_dims: std::collections::HashMap::new(),
            md_editor_scroll: ScrollHandle::new(),
            md_preview_scroll: ScrollHandle::new(),
            md_view: None,
            projects_search_focused: false,
            md_editor_w: 480.0,
            all_files: Vec::new(),
            file_tree: tree::Tree::default(),
            // Root folder starts expanded so the tree shows on open.
            expanded: std::collections::HashSet::from([PathBuf::new()]),
            tree_width: 320.0,
            tree_collapsed: false,
            commit_collapsed: false,
            open_path: None,
            open_tabs: Vec::new(),
            preview_tab: None,
            scratches: Vec::new(),
            tab_scroll: ScrollHandle::new(),
            selected_path: None,
            tree_scroll: ScrollHandle::new(),
            file_editor,
            find_open: false,
            find_replace: false,
            find_query,
            replace_query,
            find_matches: Vec::new(),
            find_idx: 0,
            diff_edit_gen: 0,
            finder_gen: 0,
            show_fps: load_show_fps(),
            fps_value: 0.0,
            fps_shown: 0.0,
            fps_last: None,
            fps_file_last: None,
            finder_open: false,
            finder_mode: FinderMode::Files,
            finder_query,
            finder_results: Vec::new(),
            content_results: Vec::new(),
            action_results: Vec::new(),
            finder_selected: 0,
            onboarding_open: first_run,
            onboarding_forced: first_run,
            plugins_win: None,
            plugins_query,
            fonts_win: None,
            clear_data_win: None,
            font_preview: None,
            welcome_frame: 0,
            onboarding_choice: keymap_preset,
            onboarding_install_cmd: true,
            shell_cmd_error: None,
            pending_crash: crash_log_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .filter(|s| !s.trim().is_empty()),
            op_error: None,
            context_menu: None,
            diff_win: None,
            diff_modal_open: false,
            main_window_bounds: None,
            diff_view_open: false,
            find_target: crate::FindTarget::File,
            rollback_win: None,
            new_branch_win: None,
            new_branch_checkout: true,
            new_branch_overwrite: false,
            delete_target: None,
            name_prompt: None,
            name_input,
            rollback_checked: std::collections::HashSet::new(),
            rollback_delete_added: false,
            current_branch: None,
            branch_list: Vec::new(),
            branch_popup_open: false,
            branch_query,
            branch_expanded: std::collections::HashSet::new(),
            refresh_gen: 0,
            ahead: None,
            behind: None,
            pushing: false,
            committing: false,
            pulling: false,
            fetching: false,
            push_msg: None,
            push_win: None,
            push_files: Vec::new(),
            push_base: String::new(),
            git_tab: GitTab::Commit,
            push_selected: None,
            update_available: None,
            updating: false,
            history_rev: "HEAD".to_string(),
            history_path: None,
            history_commits: Vec::new(),
            history_selected: None,
            history_files: Vec::new(),
            history_file_selected: None,
            history_files_tree: tree::Tree::default(),
            history_files_expanded: std::collections::HashSet::new(),
            history_files_query,
            history_panel_h: 320.0,
            history_panel_collapsed: false,
            history_compare: CompareMode::Local,
            history_compare_open: false,
            history_branch_open: false,
            history_locals: Vec::new(),
            history_remotes: Vec::new(),
            history_branch_query,
            history_commit_query,
            history_scroll: ScrollHandle::new(),
            history_commit_frac: 2.0 / 3.0,
            #[cfg(feature = "terminal")]
            term_tabs: Vec::new(),
            #[cfg(feature = "terminal")]
            term_active: 0,
            #[cfg(feature = "terminal")]
            term_open: false,
            #[cfg(feature = "terminal")]
            term_height: 260.0,
            // Restore the user's persisted "maximized terminal" preference.
            #[cfg(feature = "terminal")]
            term_maximized: crate::load_ui_bool("terminal_maximized", false),
        };
        me.refresh();
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

    pub(crate) fn repo(&self) -> Option<Repo> {
        Repo::discover(self.repo_root.as_ref()?).ok()
    }

    /// Open a folder as the active project (or switch to it if already open): record it in
    /// recents, add a project tab, and load its state. Each open project is a tab above the
    /// UI; switching preserves the one you're leaving (see `ProjectSession`).
    pub(crate) fn open_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.recents.touch(&path);
        self.recents.save();
        // Keep the Dock + menu-bar "Recent Projects" lists in sync with the new order.
        cx.set_dock_menu(dock_menu(&self.recents));
        cx.set_menus(crate::app_menus(&self.recents));
        // Stash the project we're leaving so a later switch back restores it.
        self.save_active_session();
        if !self.open_projects.contains(&path) {
            self.open_projects.push(path.clone());
        }
        self.load_project_state(path, cx);
        cx.notify();
    }

    /// Snapshot the active project's UI state into `project_sessions` (no-op on the landing
    /// view). Called before switching away so switching back restores it.
    fn save_active_session(&mut self) {
        if let Some(root) = self.repo_root.clone() {
            self.project_sessions.insert(
                root,
                crate::ProjectSession {
                    mode: self.mode,
                    open_path: self.open_path.clone(),
                    open_tabs: self.open_tabs.clone(),
                    preview_tab: self.preview_tab.clone(),
                    selected: self.selected,
                    expanded: self.expanded.clone(),
                },
            );
        }
    }

    /// Make `path` the active project, restoring its saved session if we have one (which file
    /// was open, the editor tabs, tree expansion, mode) or starting fresh otherwise.
    fn load_project_state(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.repo_root = Some(path.clone());
        match self.project_sessions.remove(&path) {
            Some(s) => {
                self.mode = s.mode;
                self.expanded = s.expanded;
                self.open_tabs = s.open_tabs;
                self.preview_tab = s.preview_tab;
                self.selected = s.selected;
                self.refresh();
                // Reload the file that was open into the editor; else leave it empty.
                // Restore as permanent — `open_file` would clear the preview slot, so save it
                // first and put it back (the restored active file may itself be the preview).
                let preview = self.preview_tab.clone();
                match s.open_path {
                    Some(p) => self.open_file(p, cx),
                    None => self.open_path = None,
                }
                self.preview_tab = preview;
            }
            None => {
                self.mode = Mode::Browse; // open into the code view, not git
                self.open_path = None;
                self.open_tabs.clear();
                self.preview_tab = None;
                self.selected = None;
                self.expanded.clear();
                self.expanded.insert(PathBuf::new()); // root folder visible by default
                self.refresh();
            }
        }
    }

    /// Close an open-project tab. Switches to a neighbour if it was active; closing the last
    /// one returns to the Projects landing view.
    pub(crate) fn close_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let Some(idx) = self.open_projects.iter().position(|p| p == &root) else {
            return;
        };
        let was_active = self.repo_root.as_ref() == Some(&root);
        self.open_projects.remove(idx);
        self.project_sessions.remove(&root);
        if !was_active {
            cx.notify();
            return;
        }
        if self.open_projects.is_empty() {
            // Back to the landing view.
            self.repo_root = None;
            self.open_path = None;
            self.open_tabs.clear();
            self.preview_tab = None;
            self.selected = None;
        } else {
            // Prefer the tab that shifted into this slot, else the previous one.
            let next = self.open_projects[idx.min(self.open_projects.len() - 1)].clone();
            self.load_project_state(next, cx);
        }
        cx.notify();
    }

    /// Open a project chosen from the Dock's "Recent Projects" submenu.
    pub(crate) fn open_recent_project(
        &mut self,
        a: &OpenRecentProject,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_project(PathBuf::from(&a.0), cx);
    }

    /// File → Open… — pick a folder and open it as a new project tab.
    pub(crate) fn act_open_project(
        &mut self,
        _: &OpenProject,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pick_folder(cx);
    }

    /// Native folder picker for the "Open" / "New Project" buttons.
    pub(crate) fn pick_folder(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths.into_iter().next() {
                    this.update(cx, |this, cx| this.open_project(p, cx)).ok();
                }
            }
        })
        .detach();
    }

    pub(crate) fn refresh(&mut self) {
        if let Some(repo) = self.repo() {
            // `git status` failing means we can't trust the file list — surface it rather
            // than show an empty (looks-clean) tree. A later success clears the banner.
            match repo.status() {
                Ok(files) => {
                    self.files = files;
                    self.op_error = None;
                }
                Err(e) => self.fail("Reading status", e),
            }
            self.all_files = repo.list_files().unwrap_or_default();
            self.file_tree = tree::Tree::build(&self.all_files);
            self.current_branch = repo.current_branch();
            self.ahead = repo.ahead_count();
            self.behind = repo.behind_count();
            // What a push would send — kept live so the Push tab's count badge is accurate.
            self.push_base = repo.push_base();
            self.push_files = repo.push_files();
        } else if let Some(root) = self.repo_root.clone() {
            // Not a git repo: Browse is still a file tree, so populate it by walking the
            // filesystem. Git-only state (changed files, branch, ahead) stays empty, and the
            // commit/push/rollback flows simply have nothing to act on.
            self.files.clear();
            self.push_files.clear();
            self.current_branch = None;
            self.ahead = None;
            self.op_error = None;
            self.all_files = list_dir_files(&root);
            self.file_tree = tree::Tree::build(&self.all_files);
        }
        if let Some(root) = self.repo_root.clone() {
            self.scratches = scratch::list(&root);
        }
        self.rebuild_commit_view(false);
        match self.selected {
            Some(i) if i < self.files.len() => self.select(i),
            _ if !self.files.is_empty() => self.select(0),
            _ => {
                self.selected = None;
                self.current_diff = None;
                self.old_spans.clear();
                self.new_spans.clear();
            }
        }
    }












    /// Re-read git + the open file from disk. Triggered when the window regains focus,
    /// since an external tool (another editor, a branch switch, a rebase, etc.) may have
    /// changed files behind our back.
    pub(crate) fn reload_external(&mut self, cx: &mut Context<Self>) {
        if self.repo_root.is_none() {
            return; // Projects landing — nothing to reload.
        }
        // git status, file tree, and the selected file's diff (all read fresh from disk/git).
        self.refresh();

        // Reload the Browse editor's open file — but only when the user has no unsaved
        // edits (never clobber), the file still exists, and the on-disk bytes actually
        // changed (avoid pointless cursor/selection resets).
        if let (Some(rel), Some(repo)) = (self.open_path.clone(), self.repo()) {
            let exists = repo.root().join(&rel).exists();
            if exists && !self.file_editor.read(cx).dirty {
                if let Ok(content) = repo.working_content(&rel) {
                    if self.file_editor.read(cx).text() != content {
                        let lang = self.effective_lang(&rel);
                        self.file_editor
                            .update(cx, |e, cx| e.set_content(content, lang, cx));
                    }
                }
            }
        }
        // An external change may have emptied the active git tab — keep it valid.
        self.normalize_git_tab(cx);
        cx.notify();
    }

    // ── context menu ──────────────────────────────────────────────
    pub(crate) fn open_menu(
        &mut self,
        at: Point<Pixels>,
        target: MenuTarget,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = Some(ContextMenu { at, target });
        cx.notify();
    }
    pub(crate) fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.context_menu = None;
        cx.notify();
    }










    /// Act on the update banner. Running from a `.app` bundle with a zip asset → download,
    /// swap in place, relaunch. Otherwise (dev binary, or a release with no zip) → open the
    /// release page in the browser.
    pub(crate) fn do_update(&mut self, cx: &mut Context<Self>) {
        let Some(rel) = self.update_available.clone() else {
            return;
        };
        match update::running_bundle() {
            Some(bundle) if !rel.zip_url.is_empty() => {
                if self.updating {
                    return;
                }
                self.updating = true;
                cx.notify();
                let zip = rel.zip_url.clone();
                cx.spawn(async move |this, cx| {
                    let res = cx
                        .background_executor()
                        .spawn({
                            let bundle = bundle.clone();
                            async move { update::download_and_swap(&zip, &bundle) }
                        })
                        .await;
                    this.update(cx, |this, cx| {
                        this.updating = false;
                        match res {
                            // Relaunch the freshly-swapped bundle, then quit this instance.
                            Ok(()) => {
                                let _ = std::process::Command::new("open").arg(&bundle).spawn();
                                cx.quit();
                            }
                            Err(e) => {
                                this.op_error = Some(format!("Update failed: {e}"));
                                cx.notify();
                            }
                        }
                    })
                    .ok();
                })
                .detach();
            }
            _ => {
                // No bundle to swap (dev binary). Download the zip to ~/Downloads and reveal
                // it in Finder; only fall back to the release page if there's no zip asset.
                if rel.zip_url.is_empty() {
                    let url = if rel.page_url.is_empty() {
                        "https://github.com/kyle-ssg/kyde/releases/latest".to_string()
                    } else {
                        rel.page_url.clone()
                    };
                    let _ = std::process::Command::new("open").arg(url).spawn();
                    return;
                }
                if self.updating {
                    return;
                }
                self.updating = true;
                cx.notify();
                let zip = rel.zip_url.clone();
                cx.spawn(async move |this, cx| {
                    let dir = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                        .join("Downloads");
                    let res = cx
                        .background_executor()
                        .spawn(async move { update::download_zip(&zip, &dir) })
                        .await;
                    this.update(cx, |this, cx| {
                        this.updating = false;
                        match res {
                            // Reveal the downloaded zip so the user can install it.
                            Ok(path) => {
                                let _ = std::process::Command::new("open")
                                    .arg("-R")
                                    .arg(&path)
                                    .spawn();
                            }
                            Err(e) => this.op_error = Some(format!("Download failed: {e}")),
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// Dismiss the update banner for this session (reappears on next launch if still behind).
    pub(crate) fn dismiss_update(&mut self, cx: &mut Context<Self>) {
        self.update_available = None;
        cx.notify();
    }
















    /// The window handle slot for a modal kind.
    fn modal_slot(&mut self, kind: ModalKind) -> &mut Option<gpui::WindowHandle<ModalWindow>> {
        match kind {
            ModalKind::Rollback => &mut self.rollback_win,
            ModalKind::Push => &mut self.push_win,
            ModalKind::Diff => &mut self.diff_win,
            ModalKind::NewBranch => &mut self.new_branch_win,
            ModalKind::Plugins => &mut self.plugins_win,
            ModalKind::Fonts => &mut self.fonts_win,
            ModalKind::ClearData => &mut self.clear_data_win,
        }
    }

    /// Open (or re-focus) a modal as its own native OS window. Opened from a spawned task so
    /// `cx.open_window` never runs inside this `Kyde` update — the new window's first render
    /// calls back into `Kyde` (`kyde.update`), which would panic re-entrantly otherwise. See
    /// the memory note on gpui phase/re-entrancy gotchas.
    pub(crate) fn open_modal_window(
        &mut self,
        kind: ModalKind,
        title: impl Into<SharedString>,
        w: f32,
        h: f32,
        cx: &mut Context<Self>,
    ) {
        let title = title.into();
        // Already open → just bring it forward (handle.update fails if it was closed).
        if let Some(existing) = *self.modal_slot(kind) {
            if existing
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return;
            }
            *self.modal_slot(kind) = None; // stale (user closed it) → fall through and reopen
        }
        let kyde = cx.entity();
        // The Diff modal opens at the MAIN window's bounds (captured each frame in
        // `impl Render for Kyde`) so it's as big as the editor and lands over it — NOT the
        // focused window's, which may be another modal (e.g. Rollback) it was launched from.
        let main_bounds = (kind == ModalKind::Diff)
            .then_some(self.main_window_bounds)
            .flatten();
        cx.spawn(async move |this, cx| {
            let opened = cx.update(|cx| {
                // Center on the display the main window is on (else gpui picks the primary
                // monitor, so the modal can pop up on a different screen than the IDE).
                let display = cx
                    .active_window()
                    .and_then(|w| {
                        w.update(cx, |_, window, cx| window.display(cx).map(|d| d.id()))
                            .ok()
                    })
                    .flatten();
                let window_bounds = main_bounds.unwrap_or_else(|| {
                    WindowBounds::Windowed(Bounds::centered(display, gpui::size(px(w), px(h)), cx))
                });
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(window_bounds),
                        titlebar: Some(gpui::TitlebarOptions {
                            title: Some(title.clone()),
                            appears_transparent: false,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    {
                        let kyde = kyde.clone();
                        move |_, cx| cx.new(|cx| ModalWindow::new(kyde.clone(), kind, cx))
                    },
                )
            });
            if let Ok(Ok(handle)) = opened {
                let _ = handle.update(cx, |view, window, cx| {
                    // New Branch: focus the name field so you can type immediately. Others:
                    // focus the root so Escape (on_key_down) dispatches.
                    if view.kind == ModalKind::NewBranch {
                        let input = view.kyde.read(cx).branch_query.read(cx).focus_handle(cx);
                        window.focus(&input);
                    } else if view.kind == ModalKind::Plugins {
                        let input = view.kyde.read(cx).plugins_query.read(cx).focus_handle(cx);
                        window.focus(&input);
                    } else {
                        let fh = view.focus_handle(cx);
                        window.focus(&fh);
                    }
                    cx.activate(true);
                });
                this.update(cx, |k, _| *k.modal_slot(kind) = Some(handle))
                    .ok();
            }
        })
        .detach();
    }

    /// Close a modal's native window (if open) and clear its handle. The actual
    /// `remove_window` is deferred: it's often called from *inside* that window's own button
    /// handler (e.g. the rollback window's "Rollback" button → `do_rollback`), and removing a
    /// window mid-dispatch of its own event is re-entrant; deferring runs it once the current
    /// effect cycle finishes.
    pub(crate) fn close_modal_window(&mut self, kind: ModalKind, cx: &mut Context<Self>) {
        if let Some(handle) = self.modal_slot(kind).take() {
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
    }









    /// Expand/collapse a directory in the Browse tree.
    pub(crate) fn toggle_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if !self.expanded.remove(&dir) {
            self.expanded.insert(dir);
        }
        cx.notify();
    }

    /// "Select Opened File in Tree" (IntelliJ-style): switch to Browse, expand
    /// every ancestor of the active file, select its row, and scroll it into
    /// view. Falls back to the highlighted row if no file is open.
    pub(crate) fn reveal_in_tree(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self
            .open_path
            .clone()
            .or_else(|| self.selected_path.clone())
        else {
            return;
        };
        self.mode = Mode::Browse;
        // Expand every ancestor dir (incl. the root `""`) so the row is visible.
        for anc in target.ancestors().skip(1) {
            self.expanded.insert(anc.to_path_buf());
        }
        self.selected_path = Some(target.clone());
        // Find the target's index in the same flattened order render_browse uses
        // (root row, then tree rows, then scratches) and scroll it into view.
        let mut idx = if target.as_os_str().is_empty() {
            Some(0)
        } else {
            None
        };
        if idx.is_none() {
            let mut i = 1usize;
            for r in self.file_tree.visible(&self.expanded) {
                if r.path == target {
                    idx = Some(i);
                    break;
                }
                i += 1;
            }
            if idx.is_none() {
                for s in &self.scratches {
                    if *s == target {
                        idx = Some(i);
                        break;
                    }
                    i += 1;
                }
            }
        }
        if let Some(i) = idx {
            self.tree_scroll.scroll_to_item(i);
        }
        cx.notify();
    }

    /// Open `rel` as a permanent tab (double-click, finder, "Open", restore, …). If it was
    /// the preview tab, it's promoted (the preview slot clears).
    pub(crate) fn open_file(&mut self, rel: PathBuf, cx: &mut Context<Self>) {
        self.open_file_inner(rel, false, cx);
    }

    /// Open `rel` as the VS Code-style *preview* (temporary) tab: a single-click in the tree.
    /// Reuses the one preview slot — a subsequent single-click on a different file replaces it
    /// in place rather than opening a new tab. Clicking a file that's already open as a
    /// permanent tab just activates it (no demotion).
    pub(crate) fn preview_file(&mut self, rel: PathBuf, cx: &mut Context<Self>) {
        self.open_file_inner(rel, true, cx);
    }

    fn open_file_inner(&mut self, rel: PathBuf, preview: bool, cx: &mut Context<Self>) {
        // Images preview via `img()` and font files preview in their own typeface (see
        // render_browse) — don't load their binary bytes into the text editor.
        if !is_image(&rel) && !is_font_file(&rel) {
            // Scratch files live outside the repo (absolute paths) — read them straight
            // from disk. Repo-relative files go through the repo's working tree when this is
            // a git repo, else straight from disk under the project root (non-git Browse).
            let content = if rel.is_absolute() {
                std::fs::read_to_string(&rel).unwrap_or_default()
            } else if let Some(repo) = self.repo() {
                repo.working_content(&rel).ok().unwrap_or_default()
            } else if let Some(root) = self.repo_root.as_ref() {
                std::fs::read_to_string(root.join(&rel)).unwrap_or_default()
            } else {
                String::new()
            };
            let lang = self.effective_lang(&rel);
            self.file_editor.update(cx, |e, cx| {
                e.line_numbers = true;
                e.set_content(content, lang, cx);
            });
            // Point the editor at whichever scroll container it renders in, so caret-follow
            // and drag auto-scroll move the right one: the Markdown split uses
            // `md_editor_scroll`, plain Browse uses `file_scroll` (mirrors the `md` gate in
            // render_browse).
            let md = matches!(highlight::Lang::from_path(&rel), highlight::Lang::Markdown)
                && self.plugins.is_installed("markdown");
            let h = if md {
                self.md_editor_scroll.clone()
            } else {
                self.file_scroll.clone()
            };
            self.file_editor.update(cx, |e, _| e.set_scroll_handle(h));
        }
        self.selected_path = Some(rel.clone());
        if self.open_tabs.contains(&rel) {
            // Already open. A permanent open promotes it out of the preview slot; a preview
            // open of an already-permanent tab leaves its permanence alone (just activates).
            if !preview && self.preview_tab.as_ref() == Some(&rel) {
                self.preview_tab = None;
            }
        } else if preview {
            // Reuse the single preview slot: replace its path in place if one exists, else
            // append. The replaced file's tab vanishes — exactly one temporary tab at a time.
            match self
                .preview_tab
                .take()
                .and_then(|prev| self.open_tabs.iter().position(|t| t == &prev))
            {
                Some(i) => self.open_tabs[i] = rel.clone(),
                None => self.open_tabs.push(rel.clone()),
            }
            self.preview_tab = Some(rel.clone());
        } else {
            self.open_tabs.push(rel.clone());
        }
        self.open_path = Some(rel);
        // Scroll the (possibly off-screen) active tab into view on next paint.
        if let Some(i) = self
            .open_path
            .as_ref()
            .and_then(|p| self.open_tabs.iter().position(|t| t == p))
        {
            self.tab_scroll.scroll_to_item(i);
        }
        self.load_font_preview(cx);
    }

    /// If the open file is a font and the "font" plugin is installed, parse its family name
    /// and register it with the text system so the preview pane can render it. Otherwise
    /// clears the cached preview. Cheap + idempotent (skips re-registering the same path).
    fn load_font_preview(&mut self, cx: &mut Context<Self>) {
        let Some(rel) = self.open_path.clone().filter(|p| is_font_file(p)) else {
            self.font_preview = None;
            return;
        };
        if !self.plugins.is_installed("font") {
            self.font_preview = None;
            return;
        }
        if self.font_preview.as_ref().is_some_and(|(p, _)| *p == rel) {
            return; // already loaded
        }
        let abs = self
            .repo_root
            .as_ref()
            .map(|r| r.join(&rel))
            .unwrap_or_else(|| rel.clone());
        let Ok(bytes) = std::fs::read(&abs) else {
            self.font_preview = None;
            return;
        };
        let Some(family) = font_family_name(&bytes) else {
            self.font_preview = None;
            return;
        };
        // Register the face so `.font_family(family)` resolves to it (idempotent in gpui).
        let _ = cx
            .text_system()
            .add_fonts(vec![std::borrow::Cow::Owned(bytes)]);
        self.font_preview = Some((rel, SharedString::from(family)));
    }

    /// Close the tab at `idx`. If it was active, fall to its right neighbour (else left,
    /// else nothing open).
    pub(crate) fn close_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.open_tabs.len() {
            return;
        }
        let closing = self.open_tabs.remove(idx);
        if self.preview_tab.as_ref() == Some(&closing) {
            self.preview_tab = None;
        }
        if self.open_path.as_ref() == Some(&closing) {
            let next = self
                .open_tabs
                .get(idx)
                .or_else(|| self.open_tabs.get(idx.saturating_sub(1)))
                .cloned();
            match next {
                Some(p) => self.open_file(p, cx),
                None => self.clear_open(cx),
            }
        }
        self.close_menu(cx);
    }

    /// Close every tab except the one at `idx`, and make it active.
    pub(crate) fn close_other_tabs(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(keep) = self.open_tabs.get(idx).cloned() else {
            return;
        };
        self.open_tabs = vec![keep.clone()];
        // Drop a stale preview pointer if its tab was among those closed.
        self.preview_tab = self.preview_tab.take().filter(|p| p == &keep);
        self.open_file(keep, cx);
        self.close_menu(cx);
    }

    /// Close all tabs to the right of `idx`. If the active tab was among them, activate `idx`.
    pub(crate) fn close_tabs_right(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx + 1 >= self.open_tabs.len() {
            self.close_menu(cx);
            return;
        }
        let active_removed = self
            .open_path
            .as_ref()
            .and_then(|p| self.open_tabs.iter().position(|t| t == p))
            .is_some_and(|pos| pos > idx);
        self.open_tabs.truncate(idx + 1);
        // Drop a stale preview pointer if its tab was truncated away.
        if self
            .preview_tab
            .as_ref()
            .is_some_and(|p| !self.open_tabs.contains(p))
        {
            self.preview_tab = None;
        }
        if active_removed {
            if let Some(p) = self.open_tabs.get(idx).cloned() {
                self.open_file(p, cx);
            }
        }
        self.close_menu(cx);
    }



    /// Open a pre-filled GitHub issue for the previous crash, then dismiss the banner.
    fn report_crash(&mut self, cx: &mut Context<Self>) {
        if let Some(crash) = self.pending_crash.clone() {
            cx.open_url(&crash_issue_url(&crash));
        }
        self.dismiss_crash(cx);
    }
    /// Clear the crash banner + truncate the log so it doesn't reappear.
    fn dismiss_crash(&mut self, cx: &mut Context<Self>) {
        self.pending_crash = None;
        if let Some(p) = crash_log_path() {
            let _ = std::fs::write(p, "");
        }
        cx.notify();
    }
    /// Thin top banner shown after a crash, with Report-on-GitHub + Dismiss.
    pub(crate) fn render_crash_banner(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let btn = |label: &'static str, primary: bool| {
            div()
                .px_3()
                .py_1()
                .rounded_md()
                .when(primary, |d| d.bg(t.primary).text_color(t.primary_text))
                .when(!primary, |d| d.text_color(t.secondary_text))
                .child(label)
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .h(px(34.0))
            .px_3()
            .bg(gpui::rgb(0x3A2A2C))
            .border_b_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size))
            .text_color(t.text)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child("kyde crashed on the previous run."),
            )
            .child(btn("Report on GitHub", true).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.report_crash(cx)),
            ))
            .child(btn("Dismiss", false).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.dismiss_crash(cx)),
            ))
            .into_any_element()
    }

    /// Record a failed git operation so the user sees it (op-error banner) instead of a
    /// silent no-op. `ctx` is a short human label ("Commit", "Push", …); the error is
    /// stringified after it. Still logs to stderr for debugging.
    pub(crate) fn fail(&mut self, ctx: &str, e: anyhow::Error) {
        eprintln!("{ctx} failed: {e:#}");
        self.op_error = Some(format!("{ctx} failed: {e}"));
    }

    /// Dismiss the git-operation error banner.
    fn dismiss_op_error(&mut self, cx: &mut Context<Self>) {
        self.op_error = None;
        cx.notify();
    }

    /// Thin banner shown when a git operation failed, with a Dismiss button. Mirrors the
    /// crash banner (same surface + placement); shown only while `op_error` is set.
    pub(crate) fn render_op_error_banner(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let msg = self.op_error.clone().unwrap_or_default();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .h(px(34.0))
            .px_3()
            .bg(gpui::rgb(0x3A2A2C))
            .border_b_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size))
            .text_color(t.text)
            .child(div().flex_1().min_w_0().truncate().child(msg))
            .child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .text_color(t.secondary_text)
                    .child("Dismiss")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.dismiss_op_error(cx)),
                    ),
            )
            .into_any_element()
    }

    /// Reset the editor to nothing-open.
    fn clear_open(&mut self, cx: &mut Context<Self>) {
        self.open_path = None;
        self.preview_tab = None;
        self.file_editor.update(cx, |e, cx| {
            e.set_content(String::new(), Lang::PlainText, cx)
        });
    }

    /// The language to actually highlight with: the file's detected language if
    /// its pack is installed (or it needs no pack), else PlainText — so an
    /// un-installed type renders fast and unparsed until the user opts in.
    pub(crate) fn effective_lang(&self, rel: &std::path::Path) -> Lang {
        let lang = Lang::from_path(rel);
        match lang.pack() {
            Some(p) if !self.plugins.is_installed(p.id) => Lang::PlainText,
            _ => lang,
        }
    }

    /// Pack available for the open file but not yet installed (drives the banner).
    pub(crate) fn pending_pack(&self) -> Option<&'static highlight::Pack> {
        self.open_path
            .as_ref()
            .and_then(|p| Lang::from_path(p).pack())
            .filter(|p| !self.plugins.is_installed(p.id) && !self.ignored_packs.contains(p.id))
    }

    /// Dismiss the install banner for the open file's type (session-only).
    pub(crate) fn ignore_open_pack(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.pending_pack() {
            self.ignored_packs.insert(p.id);
            cx.notify();
        }
    }

    /// Install the pack for the open file and re-highlight it in place
    /// (without disturbing the buffer's content, selection, or dirty flag).
    pub(crate) fn install_open_pack(&mut self, cx: &mut Context<Self>) {
        let Some(rel) = self.open_path.clone() else {
            return;
        };
        let lang = Lang::from_path(&rel);
        if let Some(p) = lang.pack() {
            self.plugins.install(p.id);
            self.plugins.save();
            // Re-highlight in place so the colors appear immediately — previously this only
            // set the lang, leaving the cached (plain) spans until the file was reopened.
            self.file_editor.update(cx, |e, cx| e.set_lang(lang, cx));
        }
    }

    /// Persist `rel`'s `text` to disk: through the repo's working tree in a git repo, else
    /// straight to disk under the project root (non-git Browse). Absolute paths (scratch
    /// files) write to themselves, since `join` keeps an absolute right-hand side. Best-effort
    /// — errors are swallowed (this runs on every keystroke via autosave).
    fn write_open_file(&self, rel: &std::path::Path, text: &str) {
        if let Some(repo) = self.repo() {
            let _ = repo.save_file(rel, text);
        } else if let Some(root) = self.repo_root.as_ref() {
            let full = root.join(rel);
            if let Some(parent) = full.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(full, text);
        }
    }

    /// Write the open file to disk without the git refresh — cheap enough to run on
    /// every keystroke. The changed-files tree re-syncs on mode switch / window refocus.
    fn autosave(&mut self, cx: &mut Context<Self>) {
        let (Some(rel), text) = (
            self.open_path.clone(),
            self.file_editor.read(cx).text().to_string(),
        ) else {
            return;
        };
        self.write_open_file(&rel, &text);
        self.file_editor.update(cx, |e, _| e.dirty = false);
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
            self.open_path.clone(),
            self.file_editor.read(cx).text().to_string(),
        ) else {
            return;
        };
        self.write_open_file(&rel, &text);
        self.file_editor.update(cx, |e, _| e.dirty = false);
        self.refresh();
    }




    // ── in-editor find / replace ──────────────────────────────────
    pub(crate) fn act_toggle_fps(
        &mut self,
        _: &ToggleFps,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_fps = !self.show_fps;
        self.fps_last = None;
        save_show_fps(self.show_fps); // remember across launches
        cx.notify();
    }
    /// Escape: close whatever overlay is open (most-transient first); if none, cancel the
    /// Commit view back to Browse. A no-op in plain Browse.
    /// Native-menu "Plugins…": open the language-pack manager (native modal window).
    pub(crate) fn act_open_plugins(
        &mut self,
        _: &OpenPlugins,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_modal_window(ModalKind::Plugins, "Language Plugins", 520.0, 560.0, cx);
    }

    /// Native-menu "Clear Data & Restart…": open the confirmation as a native modal window.
    pub(crate) fn act_clear_data(
        &mut self,
        _: &ClearData,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_modal_window(
            ModalKind::ClearData,
            "Clear Data & Restart",
            460.0,
            230.0,
            cx,
        );
    }

    /// Confirmed: wipe the config dir (uninstalls every plugin, drops keymap/theme/projects/
    /// ui prefs) and restart into a clean first-run state.
    pub(crate) fn do_clear_data(&mut self, cx: &mut Context<Self>) {
        let _ = std::fs::remove_dir_all(crate::config_dir());
        cx.restart();
    }

    pub(crate) fn act_escape(
        &mut self,
        _: &EscapeKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.context_menu.is_some() {
            self.context_menu = None;
        } else if self.find_open {
            self.close_find(&CloseFind, window, cx);
            return;
        } else if self.diff_view_open {
            self.diff_view_open = false; // Escape leaves the full-screen Show-Diff view
        } else if self.delete_target.is_some() {
            self.delete_target = None;
        } else if self.branch_popup_open {
            self.branch_popup_open = false;
        } else if self.onboarding_open && !self.onboarding_forced {
            self.onboarding_open = false;
        } else if self.mode == Mode::Commit {
            self.mode = Mode::Browse; // Escape = Cancel in the Commit view
        } else {
            return; // nothing to close
        }
        window.focus(&self.focus_handle);
        cx.notify();
    }






    /// Toggle a language pack's installed state from the plugin manager, persist it, and
    /// re-highlight the open file in place if it's affected (so colors appear/clear at once).
    /// Install a pack by id (used by the font-file install prompt), persist, and refresh the
    /// relevant preview/highlight so it applies immediately.
    pub(crate) fn install_pack(&mut self, id: &str, cx: &mut Context<Self>) {
        self.plugins.install(id);
        self.plugins.save();
        if id == "font" {
            self.load_font_preview(cx);
        } else if let Some(rel) = self.open_path.clone() {
            let eff = self.effective_lang(&rel);
            self.file_editor.update(cx, |e, cx| e.set_lang(eff, cx));
        }
        cx.notify();
    }

    pub(crate) fn toggle_plugin(&mut self, pack_id: &str, cx: &mut Context<Self>) {
        if self.plugins.is_installed(pack_id) {
            self.plugins.uninstall(pack_id);
        } else {
            self.plugins.install(pack_id);
        }
        self.plugins.save();
        if pack_id == "font" {
            self.load_font_preview(cx);
        } else if let Some(rel) = self.open_path.clone() {
            // If the open file's language maps to this pack, re-highlight it now.
            let lang = Lang::from_path(&rel);
            if lang.pack().map(|p| p.id) == Some(pack_id) {
                let eff = self.effective_lang(&rel);
                self.file_editor.update(cx, |e, cx| e.set_lang(eff, cx));
            }
        }
        cx.notify();
    }

    // ── keymap / onboarding ───────────────────────────────────────
    pub(crate) fn open_keymap(&mut self, _: &OpenKeymap, _: &mut Window, cx: &mut Context<Self>) {
        self.onboarding_choice = self.keymap.preset;
        self.onboarding_open = true;
        cx.notify();
    }
    pub(crate) fn choose_preset(&mut self, preset: Preset, cx: &mut Context<Self>) {
        self.keymap.set_preset(preset);
        self.keymap.save();
        apply_keymap(cx, &self.keymap);
        self.onboarding_open = false;
        self.onboarding_forced = false;
        cx.notify();
    }

    // ── configurable action handlers ──────────────────────────────
    pub(crate) fn act_save(&mut self, _: &SaveFile, _: &mut Window, cx: &mut Context<Self>) {
        self.save_open(cx);
        cx.notify();
    }
    /// ⌘W — close the active editor tab (no-op when nothing is open).
    pub(crate) fn act_close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(idx) = self
            .open_path
            .as_ref()
            .and_then(|p| self.open_tabs.iter().position(|t| t == p))
        {
            self.close_tab(idx, cx);
        }
    }
    pub(crate) fn act_mode_browse(
        &mut self,
        _: &ModeBrowse,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mode = Mode::Browse;
        self.diff_view_open = false;
        cx.notify();
    }













}

#[cfg(feature = "terminal")]
impl Kyde {
    /// Toggle the bottom terminal panel. Opening it spawns the first tab (lazily, so a
    /// build that never opens a terminal pays no PTY cost) and focuses it.
    pub(crate) fn act_toggle_terminal(
        &mut self,
        _: &crate::ToggleTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.term_open = !self.term_open;
        if self.term_open {
            if self.term_tabs.is_empty() {
                self.new_terminal_tab(cx);
            }
            // Open in the user's persisted maximized state.
            self.term_maximized = crate::load_ui_bool("terminal_maximized", false);
            self.focus_active_terminal(window, cx);
        } else {
            self.term_maximized = false;
        }
        cx.notify();
    }

    /// ⌘T while the terminal is focused: open a fresh tab (panel already open) and focus it.
    pub(crate) fn act_new_terminal_tab(
        &mut self,
        _: &crate::NewTerminalTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.term_open {
            self.term_open = true;
        }
        self.new_terminal_tab(cx);
        self.focus_active_terminal(window, cx);
        cx.notify();
    }

    /// Spawn a new terminal tab rooted at the open project (or `$HOME`) and make it active.
    pub(crate) fn new_terminal_tab(&mut self, cx: &mut Context<Self>) {
        let cwd = self.repo_root.clone();
        let view = cx.new(|cx| crate::terminal::TerminalView::new(cwd, cx));
        // Repaint on title change; close the tab when its context menu's Close is hit.
        cx.subscribe(&view, |this, v, ev, cx| match ev {
            crate::terminal::TerminalEvent::TitleChanged => cx.notify(),
            crate::terminal::TerminalEvent::CloseRequested => {
                if let Some(idx) = this.term_tabs.iter().position(|t| t == &v) {
                    this.close_terminal_tab(idx, cx);
                }
            }
        })
        .detach();
        self.term_tabs.push(view);
        self.term_active = self.term_tabs.len() - 1;
        cx.notify();
    }

    /// Close a terminal tab; closing the last one hides the panel.
    pub(crate) fn close_terminal_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.term_tabs.len() {
            return;
        }
        self.term_tabs.remove(idx);
        if self.term_tabs.is_empty() {
            self.term_open = false;
            self.term_maximized = false;
            self.term_active = 0;
        } else if self.term_active >= self.term_tabs.len() {
            self.term_active = self.term_tabs.len() - 1;
        }
        cx.notify();
    }

    /// Move focus to the active terminal tab's widget. Focus now AND next frame via
    /// `window.defer`: on first open the tab was just spawned this frame, so its
    /// `TerminalElement` isn't in the window tree yet and an immediate-only focus
    /// wouldn't stick (same gotcha as the finder/branch-popup focus).
    pub(crate) fn focus_active_terminal(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.term_tabs.get(self.term_active) {
            let handle = view.read(cx).handle();
            window.focus(&handle);
            window.defer(cx, move |window, _cx| window.focus(&handle));
        }
    }
}
