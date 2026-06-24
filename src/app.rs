//! `Kyde` state + controller logic — every non-render method (the `impl Kyde`
//! that holds refresh/select/stage/commit/navigation/etc). Split out of `main.rs`.
//! Sibling of `render.rs`; methods the view (or root) calls are `pub(crate)`.

use super::*;

/// Rows of context kept above the target line when auto-scrolling to a diff hunk or a
/// search hit, so it lands a few rows below the viewport top instead of pinned to it.
const SCROLL_CONTEXT_ROWS: usize = 3;
/// Debounce before the editable diff pane saves + re-diffs after a keystroke (the save +
/// `git status` + re-diff all shell out, so bursts of typing are coalesced).
const DIFF_EDIT_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(180);
/// Debounce before a Browse edit triggers a background `git status` refresh.
const STATUS_REFRESH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);
/// Debounce before a Find-in-Files keystroke fires the background `git grep` (coalesces
/// bursts of typing — a full-repo grep is far too expensive to run per keystroke).
const CONTENT_SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);
/// Minimum query length before Find-in-Files runs. 1-char queries match almost every line
/// in the repo (tens of MB of hits), so we wait until the query is specific enough.
const CONTENT_MIN_QUERY: usize = 2;
/// Max fuzzy-finder results rendered at once.
const FINDER_RESULT_CAP: usize = 50;

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

    fn repo(&self) -> Option<Repo> {
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

    fn refresh(&mut self) {
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

    fn select(&mut self, idx: usize) {
        self.select_with(idx, None);
    }

    /// Push the diff `d`'s per-line / word backgrounds + filler onto both panes. Pure
    /// decoration: it never touches content, language, read-only, line numbers, or scroll
    /// — those are owned by `load_diff_panes` on the initial load and intentionally left
    /// alone on a re-diff. Shared by `load_diff_panes`, `recompute_diff`, and the `»` revert.
    fn apply_diff_decorations(&mut self, d: &FileDiff, cx: &mut Context<Self>) {
        let (old_bg, new_bg) = diff_line_bgs(d);
        let (old_words, new_words) = diff_word_bgs(d);
        let (lf, lf_end, rf, rf_end) = diff_fillers(d);
        let t = theme::get();
        self.diff_left.update(cx, |e, _| {
            e.line_bg = old_bg;
            e.word_bg = old_words;
            e.word_bg_color = t.diff_word_old_bg;
            e.filler = lf;
            e.filler_end = lf_end;
        });
        self.diff_right.update(cx, |e, _| {
            e.line_bg = new_bg;
            e.word_bg = new_words;
            e.word_bg_color = t.diff_word_new_bg;
            e.filler = rf;
            e.filler_end = rf_end;
        });
    }

    /// Compute the `before`→`after` diff, store it (`current_diff`/`diff_base`/`diff_path`),
    /// highlight both sides, and load both panes — content + decorations + shared scroll —
    /// opening scrolled to the first hunk (a few lines of context above). The left (base)
    /// pane is always read-only; `readonly` locks the right pane too (committed/push diffs)
    /// and is mirrored into `diff_readonly`. Shared by `select_with` (editable, `false`) and
    /// `push_show_diff` (committed, `true`).
    fn load_diff_panes(
        &mut self,
        path: std::path::PathBuf,
        before: String,
        after: String,
        lang: Lang,
        readonly: bool,
        cx: &mut Context<Self>,
    ) {
        self.old_spans = highlight::highlight(&before, lang);
        self.new_spans = highlight::highlight(&after, lang);
        let d = FileDiff::compute(&before, &after);
        // Row of the first change (for the open-at-first-hunk scroll below). The leading
        // region before the first hunk is all-equal, so its display-row count ==
        // the hunk's old_range.start.
        let first_hunk_row = d.hunks.first().map(|h| h.old_range.start);
        self.diff_path = Some(path);
        self.diff_readonly = readonly;
        self.diff_base = before.clone();
        self.apply_diff_decorations(&d, cx);
        self.current_diff = Some(d);
        // Content goes in its own update closure — `set_content` leaves the decoration
        // fields set just above intact. Left is always locked (base); right tracks `readonly`.
        self.diff_left.update(cx, |e, cx| {
            e.read_only = true;
            e.line_numbers = true;
            e.set_content(before, lang, cx);
        });
        self.diff_right.update(cx, |e, cx| {
            e.read_only = readonly;
            e.line_numbers = true;
            e.set_content(after, lang, cx);
        });
        // Both panes scroll via the shared `diff_scroll`, so caret-follow / drag auto-scroll
        // and the first-hunk offset below move both panes + the gutter together.
        let dh = self.diff_scroll.clone();
        self.diff_left
            .update(cx, |e, _| e.set_scroll_handle(dh.clone()));
        self.diff_right.update(cx, |e, _| e.set_scroll_handle(dh));
        if let Some(start) = first_hunk_row {
            let row = start.saturating_sub(SCROLL_CONTEXT_ROWS) as f32;
            self.diff_scroll
                .set_offset(gpui::point(px(0.0), px(-row * editor::line_height_px())));
        }
    }

    /// Select a changed file and load it into the diff editors. `cx` is needed to push
    /// content into the editor entities; when called without a context (e.g. during a
    /// plain `refresh`) the editors are left as-is and only `current_diff` updates.
    pub(crate) fn select_with(&mut self, idx: usize, cx: Option<&mut Context<Self>>) {
        self.selected = Some(idx);
        self.commit_focus.clear(); // a single selection drops any folder-group highlight
        let Some(file) = self.files.get(idx).cloned() else {
            return;
        };
        // Image files preview as an image (like Browse), not a text diff. Clear it for every
        // other selection so a stale preview never lingers.
        self.diff_image = None;
        if is_image(&file.path) {
            self.old_spans = Vec::new();
            self.new_spans = Vec::new();
            self.current_diff = None;
            self.diff_path = None; // keep autosave disabled — never write an image pane
            self.diff_image = Some(file.path.clone()); // set unconditionally — refresh re-selects with cx=None
            if let Some(cx) = cx {
                // Drop any stale text so nothing flashes behind the image.
                self.diff_left.update(cx, |e, cx| {
                    e.set_content(String::new(), Lang::PlainText, cx)
                });
                self.diff_right.update(cx, |e, cx| {
                    e.set_content(String::new(), Lang::PlainText, cx)
                });
            }
            return;
        }
        if let Some(repo) = self.repo() {
            // A deleted file has no working copy: its "after" is empty, so the diff shows the
            // old content on the left only (render_diff drops the empty right pane). Reading
            // the (now-absent) file would otherwise error into the binary path below.
            let after = if matches!(file.status, FileStatus::Deleted) {
                String::new()
            } else {
                // A binary / unreadable working file errors here — don't feed an empty
                // string through the diff (it would render as "all deleted" and, worse,
                // the right pane's autosave would truncate the file to empty).
                let Ok(a) = repo.working_content(&file.path) else {
                    self.old_spans = Vec::new();
                    self.new_spans = Vec::new();
                    self.current_diff = None;
                    if let Some(cx) = cx {
                        self.diff_path = None; // disables diff_autosave for this file
                        let msg = String::from("Binary or non-text file — no diff.");
                        self.diff_left
                            .update(cx, |e, cx| e.set_content(msg.clone(), Lang::PlainText, cx));
                        self.diff_right
                            .update(cx, |e, cx| e.set_content(msg, Lang::PlainText, cx));
                    }
                    return;
                };
                a
            };
            let before = repo.base_content(&file.path).unwrap_or_default();
            let lang = self.effective_lang(&file.path);
            match cx {
                // No context (e.g. during a plain `refresh`): update only the diff model,
                // leaving the editor entities as-is.
                None => {
                    self.old_spans = highlight::highlight(&before, lang);
                    self.new_spans = highlight::highlight(&after, lang);
                    self.current_diff = Some(FileDiff::compute(&before, &after));
                }
                // With a context, load both panes (editable working diff: right unlocked).
                Some(cx) => {
                    self.load_diff_panes(file.path.clone(), before, after, lang, false, cx);
                }
            }
        }
    }

    /// Live-save the editable (right) diff pane to disk, then re-diff + recolor.
    fn diff_autosave(&mut self, cx: &mut Context<Self>) {
        let (Some(rel), text) = (
            self.diff_path.clone(),
            self.diff_right.read(cx).text().to_string(),
        ) else {
            return;
        };
        if let Some(repo) = self.repo() {
            let _ = repo.save_file(&rel, &text);
            self.files = repo.status().unwrap_or_default();
        }
        self.recompute_diff(&text, cx);
    }

    /// Re-diff the working text against the cached base and push backgrounds/filler/spans
    /// onto both panes. Shared by live autosave and the `»` revert.
    fn recompute_diff(&mut self, text: &str, cx: &mut Context<Self>) {
        let d = FileDiff::compute(&self.diff_base, text);
        let lang = self
            .diff_path
            .clone()
            .map(|p| self.effective_lang(&p))
            .unwrap_or(Lang::PlainText);
        self.old_spans = highlight::highlight(&self.diff_base, lang);
        self.new_spans = highlight::highlight(text, lang);
        self.apply_diff_decorations(&d, cx);
        self.current_diff = Some(d);
        self.rebuild_commit_view(false);
        cx.notify();
    }

    /// `»` in the diff gutter: discard one hunk's working change by replacing its new
    /// lines with the base lines, then save + re-diff. (Clean text op, no `git apply`.)
    pub(crate) fn diff_revert_hunk(&mut self, hi: usize, cx: &mut Context<Self>) {
        let Some(d) = self.current_diff.clone() else {
            return;
        };
        let Some(h) = d.hunks.get(hi) else {
            return;
        };
        let mut lines = d.new.clone();
        let replacement = d.old[h.old_range.clone()].to_vec();
        lines.splice(h.new_range.clone(), replacement);
        let content = lines.join("\n");
        let lang = self
            .diff_path
            .clone()
            .map(|p| self.effective_lang(&p))
            .unwrap_or(Lang::PlainText);
        self.diff_right
            .update(cx, |e, cx| e.set_content(content.clone(), lang, cx));
        if let (Some(rel), Some(repo)) = (self.diff_path.clone(), self.repo()) {
            let _ = repo.save_file(&rel, &content);
            self.files = repo.status().unwrap_or_default();
        }
        self.recompute_diff(&content, cx);
        self.exit_commit_if_clean();
    }

    /// Alt+↓ — jump to the next changed region in the diff.
    pub(crate) fn act_diff_next(
        &mut self,
        _: &crate::DiffNextChange,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_nav_hunk(true, cx);
    }

    /// Alt+↑ — jump to the previous changed region in the diff.
    pub(crate) fn act_diff_prev(
        &mut self,
        _: &crate::DiffPrevChange,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_nav_hunk(false, cx);
    }

    /// Display-row index where each hunk begins (in the aligned two-pane layout). Drives the
    /// diff's change count + the prev/next navigation.
    fn diff_hunk_rows(&self) -> Vec<usize> {
        let Some(d) = self.current_diff.as_ref() else {
            return Vec::new();
        };
        crate::aligned_rows(d)
            .iter()
            .enumerate()
            .filter(|(_, r)| r.hunk_start)
            .map(|(i, _)| i)
            .collect()
    }

    /// Jump the diff to the next (`next`) or previous changed region, wrapping around. The
    /// anchor is the hunk currently scrolled to the top (`load_diff_panes` parks each hunk
    /// `SCROLL_CONTEXT_ROWS` below the viewport top, so we add that back).
    pub(crate) fn diff_nav_hunk(&mut self, next: bool, cx: &mut Context<Self>) {
        let rows = self.diff_hunk_rows();
        if rows.is_empty() {
            return;
        }
        let lh = editor::line_height_px();
        let top = (-f32::from(self.diff_scroll.offset().y) / lh).round() as i64;
        let anchor = top + SCROLL_CONTEXT_ROWS as i64;
        let target = if next {
            rows.iter()
                .copied()
                .find(|&r| (r as i64) > anchor)
                .unwrap_or(rows[0]) // past the last → wrap to the first
        } else {
            rows.iter()
                .copied()
                .rev()
                .find(|&r| (r as i64) < anchor)
                .unwrap_or_else(|| *rows.last().unwrap()) // before the first → wrap to the last
        };
        let row = target.saturating_sub(SCROLL_CONTEXT_ROWS) as f32;
        self.diff_scroll
            .set_offset(gpui::point(gpui::px(0.0), gpui::px(-row * lh)));
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

    // ── branch switcher ───────────────────────────────────────────
    pub(crate) fn toggle_branch_popup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.branch_popup_open {
            self.branch_popup_open = false;
            window.focus(&self.focus_handle);
        } else {
            self.branch_list = self
                .repo()
                .and_then(|r| r.branches().ok())
                .unwrap_or_default();
            self.branch_query.update(cx, |e, cx| {
                e.set_content(String::new(), Lang::PlainText, cx)
            });
            // Recent expanded by default; Local collapsed.
            self.branch_expanded.insert("sec:recent".into());
            self.branch_popup_open = true;
            // Focus now and next frame: the popup element isn't in the tree on first open.
            let handle = self.branch_query.read(cx).focus_handle.clone();
            window.focus(&handle);
            window.defer(cx, move |window, _cx| window.focus(&handle));
        }
        cx.notify();
    }

    /// Open the push confirmation modal (lists the files that would be pushed, like the
    /// commit/rollback views). Push doesn't fire until the user confirms.
    pub(crate) fn open_push_modal(&mut self, cx: &mut Context<Self>) {
        self.context_menu = None;
        if let Some(repo) = self.repo() {
            self.push_base = repo.push_base();
            self.push_files = repo.push_files();
        } else {
            self.push_files.clear();
        }
        self.open_modal_window(ModalKind::Push, "Push", 520.0, 560.0, cx);
        cx.notify();
    }

    /// Switch the git view's tab (Commit / Push), selecting the first file of that tab so
    /// its diff shows right away.
    pub(crate) fn set_git_tab(&mut self, tab: GitTab, cx: &mut Context<Self>) {
        if self.git_tab == tab {
            return;
        }
        self.git_tab = tab;
        match tab {
            GitTab::Commit => {
                // Switching onto the Commit tab focuses the message box, same as entering it.
                self.focus_commit_msg = true;
                if self.files.is_empty() {
                    self.clear_diff_panes(cx);
                } else {
                    let i = self.selected.filter(|&i| i < self.files.len()).unwrap_or(0);
                    self.select_with(i, Some(cx));
                }
            }
            GitTab::Push => {
                if self.push_files.is_empty() {
                    self.push_selected = None;
                    self.clear_diff_panes(cx);
                } else {
                    let i = self
                        .push_selected
                        .filter(|&i| i < self.push_files.len())
                        .unwrap_or(0);
                    self.select_push_file(i, cx);
                }
            }
        }
        cx.notify();
    }

    /// Keep `git_tab` on a tab that has content: if the current tab just emptied (after a
    /// commit or push), switch to the other one and select its first file. No-op when the
    /// current tab still has files, or when both are empty (the view shows its central message).
    pub(crate) fn normalize_git_tab(&mut self, cx: &mut Context<Self>) {
        if self.mode != Mode::Commit {
            return;
        }
        let want = match self.git_tab {
            GitTab::Commit if self.files.is_empty() && !self.push_files.is_empty() => GitTab::Push,
            GitTab::Push if self.push_files.is_empty() && !self.files.is_empty() => GitTab::Commit,
            _ => return,
        };
        self.set_git_tab(want, cx);
    }

    /// Empty the diff panes (both sides + cached diff/path) — used when a tab has no file to
    /// show, so a stale file doesn't linger from the other tab.
    fn clear_diff_panes(&mut self, cx: &mut Context<Self>) {
        self.diff_path = None;
        self.current_diff = None;
        self.diff_left.update(cx, |e, cx| {
            e.set_content(String::new(), Lang::PlainText, cx)
        });
        self.diff_right.update(cx, |e, cx| {
            e.set_content(String::new(), Lang::PlainText, cx)
        });
    }

    /// Select a file in the Push tab → load its committed change (`push_base` vs HEAD)
    /// read-only into the diff panes (no working-tree edit, so no revert chevrons/autosave).
    pub(crate) fn select_push_file(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.push_selected = Some(idx);
        let Some(file) = self.push_files.get(idx).cloned() else {
            return;
        };
        let Some(repo) = self.repo() else { return };
        let before = repo
            .committed_content(&self.push_base, &file.path)
            .unwrap_or_default();
        let after = repo
            .committed_content("HEAD", &file.path)
            .unwrap_or_default();
        let lang = self.effective_lang(&file.path);
        self.load_diff_panes(file.path.clone(), before, after, lang, true, cx);
        cx.notify();
    }

    /// Push modal → right-click a file → "View Diff": show that file's committed change
    /// (`push_base` vs HEAD) read-only in the diff window — no working-tree edit, so no
    /// gutter revert chevrons or autosave.
    pub(crate) fn push_show_diff(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.context_menu = None;
        let Some(file) = self.push_files.get(idx).cloned() else {
            return;
        };
        let Some(repo) = self.repo() else { return };
        let before = repo
            .committed_content(&self.push_base, &file.path)
            .unwrap_or_default();
        let after = repo
            .committed_content("HEAD", &file.path)
            .unwrap_or_default();
        let lang = self.effective_lang(&file.path);
        // `diff_path = Some` (set inside `load_diff_panes`) makes `render_diff` show the
        // panes (it gates on a path); `readonly = true` suppresses the revert chevrons +
        // autosave for this committed diff.
        self.load_diff_panes(file.path.clone(), before, after, lang, true, cx);
        // Show the diff full-screen in the MAIN window; close the Push window we came from.
        self.diff_view_open = true;
        self.close_modal_window(ModalKind::Push, cx);
        cx.notify();
    }

    /// Leave the full-screen Show-Diff view, back to whatever mode the main window was in.
    pub(crate) fn close_diff_view(&mut self, cx: &mut Context<Self>) {
        self.diff_view_open = false;
        cx.notify();
    }

    /// Push the current branch to origin on a background thread (network I/O must
    /// not block the UI), then refresh status and the ahead-count badge.
    pub(crate) fn do_push(&mut self, cx: &mut Context<Self>) {
        self.close_modal_window(ModalKind::Push, cx);
        if self.pushing {
            return;
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        self.pushing = true;
        self.push_msg = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { Repo::discover(&root).and_then(|r| r.push_rebasing()) })
                .await;
            this.update(cx, |this, cx| {
                this.pushing = false;
                let err = result.err().map(|e| e.to_string());
                this.push_msg = err.clone();
                this.refresh();
                // After refresh (which clears `op_error` on a clean status read).
                if let Some(m) = err {
                    this.op_error = Some(format!("Push failed: {m}"));
                }
                // Pushed → the Push tab may be empty now; flip to Commit if it has work.
                this.normalize_git_tab(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
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

    /// Pull = fetch + rebase local commits on top (auto-stashing edits), off the UI thread.
    /// Mirrors `do_push`. Closes the branch popup so the UI never freezes mid-operation.
    pub(crate) fn do_pull(&mut self, cx: &mut Context<Self>) {
        self.branch_popup_open = false;
        self.context_menu = None;
        if self.pulling {
            return;
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        self.pulling = true;
        self.push_msg = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { Repo::discover(&root).and_then(|r| r.pull_rebase()) })
                .await;
            this.update(cx, |this, cx| {
                this.pulling = false;
                let err = result.err().map(|e| e.to_string());
                this.push_msg = err.clone();
                this.refresh();
                // After refresh (which clears `op_error` on a clean status read).
                if let Some(m) = err {
                    this.op_error = Some(format!("Pull failed: {m}"));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Fetch remote-tracking refs off the UI thread, then refresh so the ahead/behind badges
    /// reflect the true remote state. Doesn't touch the working tree (unlike Pull).
    pub(crate) fn do_fetch(&mut self, cx: &mut Context<Self>) {
        self.context_menu = None;
        self.branch_popup_open = false;
        if self.fetching {
            return;
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        self.fetching = true;
        self.push_msg = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { Repo::discover(&root).and_then(|r| r.fetch()) })
                .await;
            this.update(cx, |this, cx| {
                this.fetching = false;
                let err = result.err().map(|e| e.to_string());
                // refresh() recomputes ahead/behind from the freshly-fetched refs.
                this.refresh();
                if let Some(m) = err {
                    this.op_error = Some(format!("Fetch failed: {m}"));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn toggle_branch_node(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.branch_expanded.remove(&key) {
            self.branch_expanded.insert(key);
        }
        cx.notify();
    }

    pub(crate) fn checkout_branch(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_branch_op(window, cx, move |r| r.checkout(&name));
    }

    /// Open the "Create New Branch" dialog (own native window). `branch_query` doubles as the
    /// name field (prefilled with whatever was typed in the branch popup).
    pub(crate) fn open_new_branch(&mut self, cx: &mut Context<Self>) {
        self.branch_popup_open = false;
        self.new_branch_checkout = true;
        self.new_branch_overwrite = false;
        self.open_modal_window(ModalKind::NewBranch, "Create New Branch", 520.0, 220.0, cx);
        cx.notify();
    }

    /// Create the branch named in the dialog, honoring the Checkout / Overwrite toggles, then
    /// close the dialog and refresh. Spaces in the name become hyphens (git rejects spaces).
    pub(crate) fn do_create_branch(&mut self, cx: &mut Context<Self>) {
        let name = slugify_branch(self.branch_query.read(cx).text());
        if name.is_empty() {
            return;
        }
        let (checkout, overwrite) = (self.new_branch_checkout, self.new_branch_overwrite);
        self.close_modal_window(ModalKind::NewBranch, cx);
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    Repo::discover(&root)
                        .and_then(|r| r.create_branch_opts(&name, checkout, overwrite))
                })
                .await;
            this.update(cx, |this, cx| {
                this.refresh();
                // After refresh (which clears `op_error` on a clean status read).
                if let Err(e) = res {
                    this.fail("Create branch", e);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run a branch git op (checkout / create) OFF the UI thread, then refresh. Closes the
    /// popup immediately so the UI never freezes mid-operation (`git checkout` touches the
    /// whole working tree and was blocking the main thread).
    fn run_branch_op(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        op: impl FnOnce(&Repo) -> anyhow::Result<()> + Send + 'static,
    ) {
        self.branch_popup_open = false;
        window.focus(&self.focus_handle);
        cx.notify();
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { Repo::discover(&root).and_then(|r| op(&r)) })
                .await;
            this.update(cx, |this, cx| {
                this.refresh();
                // After refresh (which clears `op_error` on a clean status read).
                if let Err(e) = res {
                    this.fail("Branch operation", e);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    /// Browse → "Commit": jump to the Commit view, selecting the file if it changed.
    /// Indices into `self.files` that are committable/rollback-able under `path`:
    /// a file → itself; a folder → every change beneath it; the repo root (`""`) → all.
    fn changed_under(&self, path: &std::path::Path) -> Vec<usize> {
        self.files
            .iter()
            .enumerate()
            .filter(|(_, f)| f.path == path || f.path.starts_with(path))
            .map(|(i, _)| i)
            .collect()
    }

    /// True if there is anything to commit/rollback under `path` — gates the Browse
    /// context menu so Commit/Rollback never show on unchanged files or folders.
    pub(crate) fn has_changes_under(&self, path: &std::path::Path) -> bool {
        !self.changed_under(path).is_empty()
    }

    /// Rebuild the commit view's folder tree from the current changed files. `check_all`
    /// re-checks everything (entering the view); otherwise existing checks are preserved
    /// (dropping files that are no longer changed).
    fn rebuild_commit_view(&mut self, check_all: bool) {
        let paths: Vec<PathBuf> = self.files.iter().map(|f| f.path.clone()).collect();
        self.commit_tree = tree::Tree::build(&paths);
        // Expand the whole tree (root + every ancestor dir) so all changes are visible.
        self.commit_expanded.clear();
        self.commit_expanded.insert(PathBuf::new());
        for p in &paths {
            for anc in p.ancestors().skip(1) {
                self.commit_expanded.insert(anc.to_path_buf());
            }
        }
        if check_all {
            self.commit_checked = paths.into_iter().collect();
        } else {
            let live: std::collections::HashSet<PathBuf> = paths.into_iter().collect();
            self.commit_checked.retain(|p| live.contains(p));
        }
    }

    /// Whether every changed file under `path` (a folder, or `""` = root) is checked.
    pub(crate) fn folder_all_checked(&self, path: &std::path::Path) -> bool {
        let desc = self.changed_under(path);
        !desc.is_empty()
            && desc.iter().all(|&i| {
                self.files
                    .get(i)
                    .is_some_and(|f| self.commit_checked.contains(&f.path))
            })
    }

    /// Toggle a commit checkbox. For a folder, set every changed file under it to match
    /// (uncheck-all if currently all checked, else check-all).
    pub(crate) fn toggle_commit_check(
        &mut self,
        path: PathBuf,
        is_dir: bool,
        cx: &mut Context<Self>,
    ) {
        if is_dir {
            let want = !self.folder_all_checked(&path);
            let descendants: Vec<PathBuf> = self
                .changed_under(&path)
                .iter()
                .filter_map(|&i| self.files.get(i))
                .map(|f| f.path.clone())
                .collect();
            for p in descendants {
                if want {
                    self.commit_checked.insert(p);
                } else {
                    self.commit_checked.remove(&p);
                }
            }
        } else if !self.commit_checked.remove(&path) {
            self.commit_checked.insert(path);
        }
        cx.notify();
    }
    pub(crate) fn toggle_commit_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if !self.commit_expanded.remove(&dir) {
            self.commit_expanded.insert(dir);
        }
        cx.notify();
    }

    /// Same as `folder_all_checked`/`toggle_commit_check`, but for the rollback selection.
    pub(crate) fn rollback_folder_all_checked(&self, path: &std::path::Path) -> bool {
        let desc = self.changed_under(path);
        !desc.is_empty()
            && desc.iter().all(|&i| {
                self.files
                    .get(i)
                    .is_some_and(|f| self.rollback_checked.contains(&f.path))
            })
    }
    pub(crate) fn toggle_rollback_check(
        &mut self,
        path: PathBuf,
        is_dir: bool,
        cx: &mut Context<Self>,
    ) {
        if is_dir {
            let want = !self.rollback_folder_all_checked(&path);
            let descendants: Vec<PathBuf> = self
                .changed_under(&path)
                .iter()
                .filter_map(|&i| self.files.get(i))
                .map(|f| f.path.clone())
                .collect();
            for p in descendants {
                if want {
                    self.rollback_checked.insert(p);
                } else {
                    self.rollback_checked.remove(&p);
                }
            }
        } else if !self.rollback_checked.remove(&path) {
            self.rollback_checked.insert(path);
        }
        cx.notify();
    }

    /// Browse → "Commit": jump to the Commit view with every change under the target
    /// (a file or a whole folder) highlighted as a group, the first one open for diff.
    pub(crate) fn menu_commit_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let idxs = self.changed_under(&path);
        self.context_menu = None;
        let Some(&first) = idxs.first() else {
            cx.notify();
            return;
        };
        self.mode = Mode::Commit;
        // Build the commit tree (we may be arriving straight from Browse, never having entered
        // the commit view) without clobbering it; we set the checkboxes explicitly below.
        self.rebuild_commit_view(false);
        // `select_with(.., Some(cx))` (not `select`) so the first file's diff actually opens —
        // plain `select` passes cx=None and leaves the pane on "Select a file". Clears
        // commit_focus, so the group is set afterwards.
        self.select_with(first, Some(cx));
        let group: std::collections::HashSet<PathBuf> = idxs
            .iter()
            .filter_map(|&i| self.files.get(i))
            .map(|f| f.path.clone())
            .collect();
        self.commit_focus = group.clone();
        // Tick exactly the right-clicked path's changes — "Commit this folder/file" means those
        // files are the ones staged for the commit (otherwise the view opens with nothing
        // checked and the Commit button does nothing).
        self.commit_checked = group;
        cx.notify();
    }
    /// Commit → "Show Diff": open the floating diff viewer for that changed file.
    pub(crate) fn menu_show_diff(&mut self, idx: usize, cx: &mut Context<Self>) {
        // `select_with(.., Some(cx))` — not `select` — so the diff editors + `diff_path`
        // actually populate (plain `select` only updates `current_diff`).
        self.select_with(idx, Some(cx));
        self.context_menu = None;
        // Show the diff full-screen in the MAIN window (reuses every editor feature). Close the
        // Rollback window we were launched from so the main window comes forward.
        self.diff_view_open = true;
        self.close_modal_window(ModalKind::Rollback, cx);
        cx.notify();
    }

    /// Browse → "Rollback": open the rollback modal with every change under `path`
    /// (a file, or all changes within a folder) pre-checked.
    pub(crate) fn open_rollback_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let checked: std::collections::HashSet<PathBuf> = self
            .changed_under(&path)
            .iter()
            .filter_map(|&i| self.files.get(i))
            .map(|f| f.path.clone())
            .collect();
        self.context_menu = None;
        if checked.is_empty() {
            cx.notify();
            return;
        }
        self.rollback_checked = checked;
        self.rollback_delete_added = false;
        self.open_modal_window(ModalKind::Rollback, "Rollback Changes", 560.0, 640.0, cx);
        cx.notify();
    }

    /// Close the rollback window.
    pub(crate) fn close_rollback_window(&mut self, cx: &mut Context<Self>) {
        self.close_modal_window(ModalKind::Rollback, cx);
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
    fn open_modal_window(
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
    /// Discard the checked files (modified/deleted → restore from HEAD; added/untracked →
    /// unstage and, if "delete local copies" is set, remove the file).
    pub(crate) fn do_rollback(&mut self, cx: &mut Context<Self>) {
        let delete_added = self.rollback_delete_added;
        let targets: Vec<ChangedFile> = self
            .files
            .iter()
            .filter(|f| self.rollback_checked.contains(&f.path))
            .cloned()
            .collect();
        let mut failures: Vec<String> = Vec::new();
        if let Some(repo) = self.repo() {
            for f in targets {
                let r = match f.status {
                    FileStatus::Untracked => {
                        if delete_added {
                            repo.delete_file(&f.path)
                        } else {
                            Ok(())
                        }
                    }
                    FileStatus::Added => {
                        let _ = repo.unstage(&f.path);
                        if delete_added {
                            repo.delete_file(&f.path)
                        } else {
                            Ok(())
                        }
                    }
                    _ => repo.discard(&f.path),
                };
                if let Err(e) = r {
                    eprintln!("rollback {:?} failed: {e:#}", f.path);
                    failures.push(f.path.display().to_string());
                }
            }
        }
        self.close_rollback_window(cx);
        self.refresh();
        // Set after refresh — a successful status read clears `op_error`, so this would
        // otherwise be wiped out the moment it's set.
        if !failures.is_empty() {
            self.op_error = Some(format!(
                "Rollback failed for {} file(s): {}",
                failures.len(),
                failures.join(", ")
            ));
        }
        self.exit_commit_if_clean();
        cx.notify();
    }

    /// After a revert leaves the working tree clean, drop back to the file (Browse) view —
    /// there's nothing left to commit, so the git view would just be empty.
    fn exit_commit_if_clean(&mut self) {
        if self.mode == Mode::Commit && self.files.is_empty() {
            self.mode = Mode::Browse;
        }
    }

    /// Open the delete-confirmation modal for a tree path (is_dir derived from disk).
    /// Open the "new file" prompt, creating in `dir` (rel path; `""` = repo root).
    pub(crate) fn start_new_file(
        &mut self,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        self.name_prompt = Some(NamePrompt::NewFile(dir));
        self.name_input.update(cx, |e, cx| {
            e.set_content(String::new(), Lang::PlainText, cx)
        });
        let handle = self.name_input.read(cx).focus_handle.clone();
        window.focus(&handle);
        window.defer(cx, move |window, _cx| window.focus(&handle));
        cx.notify();
    }

    /// Open the "rename" prompt for `path` (rel), pre-filled with its current name.
    pub(crate) fn start_rename(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        let cur = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.name_prompt = Some(NamePrompt::Rename(path));
        self.name_input
            .update(cx, |e, cx| e.set_content(cur, Lang::PlainText, cx));
        let handle = self.name_input.read(cx).focus_handle.clone();
        window.focus(&handle);
        window.defer(cx, move |window, _cx| window.focus(&handle));
        cx.notify();
    }

    pub(crate) fn cancel_name_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.name_prompt = None;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    /// Apply the name prompt: create the new file (and open it) or rename, then
    /// refresh. A blank name just cancels.
    pub(crate) fn confirm_name_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.name_prompt.take() else {
            return;
        };
        let name = self.name_input.read(cx).text().trim().to_string();
        window.focus(&self.focus_handle);
        if name.is_empty() {
            cx.notify();
            return;
        }
        match prompt {
            NamePrompt::NewFile(dir) => {
                let rel = if dir.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    dir.join(&name)
                };
                if let Some(repo) = self.repo() {
                    if repo.save_file(&rel, "").is_ok() {
                        self.refresh();
                        self.open_file(rel, cx);
                    }
                }
            }
            NamePrompt::Rename(path) => {
                let dst = path
                    .parent()
                    .map(|d| d.join(&name))
                    .unwrap_or_else(|| PathBuf::from(&name));
                let ok = self
                    .repo()
                    .map(|r| r.rename(&path, &dst).is_ok())
                    .unwrap_or(false);
                if ok {
                    // Repoint any open tab / selection from the old path to the new one.
                    for t in self.open_tabs.iter_mut() {
                        if *t == path {
                            *t = dst.clone();
                        }
                    }
                    let was_open = self.open_path.as_ref() == Some(&path);
                    if self.selected_path.as_ref() == Some(&path) {
                        self.selected_path = Some(dst.clone());
                    }
                    self.refresh();
                    if was_open {
                        self.open_file(dst, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn open_delete(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let abs = if path.is_absolute() {
            path.clone()
        } else {
            self.repo_root
                .as_ref()
                .map(|r| r.join(&path))
                .unwrap_or_else(|| path.clone())
        };
        let is_dir = abs.is_dir();
        self.context_menu = None;
        self.delete_target = Some((path, is_dir));
        cx.notify();
    }

    /// Delete the pending file/folder from disk, then refresh the trees.
    pub(crate) fn do_delete(&mut self, cx: &mut Context<Self>) {
        let Some((path, is_dir)) = self.delete_target.take() else {
            return;
        };
        let abs = if path.is_absolute() {
            path.clone()
        } else {
            self.repo_root
                .as_ref()
                .map(|r| r.join(&path))
                .unwrap_or_else(|| path.clone())
        };
        let r = if is_dir {
            std::fs::remove_dir_all(&abs)
        } else {
            std::fs::remove_file(&abs)
        };
        if let Err(e) = r {
            eprintln!("delete {abs:?} failed: {e:#}");
        }
        // Drop any open tab / selection pointing at the deleted path.
        self.open_tabs.retain(|t| t != &path);
        if self.open_path.as_ref() == Some(&path) {
            self.open_path = self.open_tabs.last().cloned();
        }
        if self.selected_path.as_ref() == Some(&path) {
            self.selected_path = None;
        }
        self.refresh();
        cx.notify();
    }

    pub(crate) fn commit_now(&mut self, cx: &mut Context<Self>) {
        if self.committing {
            return;
        }
        let msg = self.commit_editor.read(cx).text().trim().to_string();
        if msg.is_empty() || self.commit_checked.is_empty() {
            return;
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        // Snapshot what to stage vs unstage so the actual git work runs off the UI thread
        // (staging + commit shell out per file — keep the button responsive + show feedback).
        let checked: Vec<PathBuf> = self.commit_checked.iter().cloned().collect();
        let all: Vec<PathBuf> = self.files.iter().map(|f| f.path.clone()).collect();
        self.committing = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let repo = Repo::discover(&root)?;
                    for p in &all {
                        if checked.contains(p) {
                            repo.stage(p)?;
                        } else {
                            repo.unstage(p)?;
                        }
                    }
                    repo.commit(&msg)
                })
                .await;
            this.update(cx, |this, cx| {
                this.committing = false;
                match result {
                    Ok(()) => {
                        this.commit_editor.update(cx, |e, cx| {
                            e.set_content(String::new(), Lang::PlainText, cx)
                        });
                        this.refresh();
                        // Tab may be empty now → flip to Push if it has work.
                        this.normalize_git_tab(cx);
                    }
                    Err(e) => this.fail("Commit", e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
    fn reveal_in_tree(&mut self, cx: &mut Context<Self>) {
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

    /// Reveal a repo-relative path in the OS file manager (macOS Finder via `open -R`).
    pub(crate) fn reveal_in_os(&mut self, rel: &std::path::Path, cx: &mut Context<Self>) {
        if let Some(root) = &self.repo_root {
            let full = root.join(rel);
            std::process::Command::new("open")
                .arg("-R")
                .arg(&full)
                .spawn()
                .ok();
        }
        self.close_menu(cx);
    }

    /// Open the system terminal in the folder containing a repo-relative path
    /// (macOS: `open -a Terminal <dir>`). Files open their parent dir; dirs open
    /// themselves.
    pub(crate) fn reveal_in_terminal(&mut self, rel: &std::path::Path, cx: &mut Context<Self>) {
        if let Some(root) = &self.repo_root {
            let full = root.join(rel);
            let dir = if full.is_dir() {
                full.clone()
            } else {
                full.parent()
                    .map_or_else(|| root.clone(), |p| p.to_path_buf())
            };
            std::process::Command::new("open")
                .arg("-a")
                .arg("Terminal")
                .arg(&dir)
                .spawn()
                .ok();
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
    fn fail(&mut self, ctx: &str, e: anyhow::Error) {
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

    // ─── Unified divider dragging ──────────────────────────────────────────────────────────
    // Every resize divider routes through these three methods. The pane each divider controls
    // is laid out at an explicit pixel size, and the grab offset captured in `start_divider_drag`
    // cancels the (context-dependent) geometry, so the bar stays exactly under the cursor — the
    // 1:1 tracking the markdown split always had, now shared by all of them.

    /// Current pixel size of the pane `d` controls. Diff/history store a *fraction*; convert it
    /// to live pixels with the SAME width the layout uses, so the round-trip through
    /// `set_divider_size` is lossless (the fractional scale cancels).
    fn divider_size(&self, d: Divider, vw: f32, _vh: f32) -> f32 {
        match d {
            Divider::Tree => self.tree_width,
            Divider::MdSplit => self.md_editor_w,
            Divider::DiffPane => {
                self.diff_split.clamp(0.15, 0.85) * (full_island_w(vw) - DIFF_GUTTER_W).max(1.0)
            }
            Divider::HistCommit => self.history_commit_frac.clamp(0.15, 0.85) * full_island_w(vw),
            Divider::HistPanel => self.history_panel_h,
            Divider::Term => {
                #[cfg(feature = "terminal")]
                {
                    self.term_height
                }
                #[cfg(not(feature = "terminal"))]
                {
                    0.0
                }
            }
        }
    }

    /// Set state so the pane `d` controls becomes `size` px, clamped to keep both sides usable.
    fn set_divider_size(&mut self, d: Divider, size: f32, vw: f32, vh: f32) {
        match d {
            Divider::Tree => self.tree_width = size.clamp(180.0, 900.0),
            Divider::MdSplit => {
                let island_left = RAIL_W + self.tree_width + theme::FRAME_GAP;
                let island_w = (vw - island_left - theme::FRAME_GAP).max(1.0);
                self.md_editor_w = size.clamp(200.0, (island_w - 200.0).max(200.0));
            }
            Divider::DiffPane => {
                let avail = (full_island_w(vw) - DIFF_GUTTER_W).max(1.0);
                self.diff_split = (size / avail).clamp(0.15, 0.85);
            }
            Divider::HistCommit => {
                self.history_commit_frac = (size / full_island_w(vw)).clamp(0.15, 0.85);
            }
            Divider::HistPanel => {
                self.history_panel_h = size.clamp(140.0, (vh - 180.0).max(140.0));
            }
            Divider::Term => {
                #[cfg(feature = "terminal")]
                {
                    self.term_height = size.clamp(120.0, (vh - 160.0).max(120.0));
                }
                #[cfg(not(feature = "terminal"))]
                {
                    let _ = (size, vh);
                }
            }
        }
    }

    /// Begin dragging divider `d`. Captures the grab offset so the first move doesn't jolt the
    /// bar to the cursor — and, because the offset absorbs the geometry, tracking is exact 1:1.
    pub(crate) fn start_divider_drag(
        &mut self,
        d: Divider,
        cursor: gpui::Point<gpui::Pixels>,
        window: &Window,
    ) {
        let sz = window.viewport_size();
        let (vw, vh) = (f32::from(sz.width), f32::from(sz.height));
        let coord = f32::from(if d.vertical() { cursor.y } else { cursor.x });
        let size = self.divider_size(d, vw, vh);
        // Leading panes grow with the cursor (size = coord − grab); trailing panes grow as the
        // cursor moves toward the near edge (size = grab − coord).
        let grab = if d.trailing() {
            coord + size
        } else {
            coord - size
        };
        self.divider_drag = Some((d, grab));
    }

    /// Apply the active divider drag for the current cursor position. Returns whether a drag was
    /// active (so the caller can `cx.notify()`).
    pub(crate) fn drag_divider(
        &mut self,
        cursor: gpui::Point<gpui::Pixels>,
        vw: f32,
        vh: f32,
    ) -> bool {
        let Some((d, grab)) = self.divider_drag else {
            return false;
        };
        let coord = f32::from(if d.vertical() { cursor.y } else { cursor.x });
        let size = if d.trailing() {
            grab - coord
        } else {
            coord - grab
        };
        self.set_divider_size(d, size, vw, vh);
        true
    }

    /// Whether divider `d` is the one currently being dragged.
    pub(crate) fn dragging(&self, d: Divider) -> bool {
        matches!(self.divider_drag, Some((k, _)) if k == d)
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
    fn effective_lang(&self, rel: &std::path::Path) -> Lang {
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

    /// Debounced git-status refresh: only the latest edit's timer wins, so status
    /// catches up ~0.4s after you stop typing instead of on every keystroke.
    fn schedule_status_refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_gen = self.refresh_gen.wrapping_add(1);
        let generation = self.refresh_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(STATUS_REFRESH_DEBOUNCE)
                .await;
            this.update(cx, |this, cx| {
                if this.refresh_gen == generation {
                    this.refresh();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
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

    // ── finder ────────────────────────────────────────────────────
    /// Debounced, background Find-in-Files. `git grep` on a large repo is far too expensive
    /// to run synchronously per keystroke (a 1-char query took ~20s / 29MB on a 2.7k-file
    /// repo and froze the UI). So: enforce a minimum query length, debounce keystroke bursts,
    /// run the grep off the UI thread, and apply results only if no newer keystroke
    /// superseded this one (generation check, like the diff-edit autosave).
    fn schedule_content_search(&mut self, cx: &mut Context<Self>) {
        let q = self.finder_query.read(cx).text().trim().to_string();
        self.finder_gen = self.finder_gen.wrapping_add(1);
        let gen = self.finder_gen;
        self.finder_selected = 0;
        // Too short → clear immediately, never grep (matches almost every line in the repo).
        if q.len() < CONTENT_MIN_QUERY {
            self.content_results = Vec::new();
            cx.notify();
            return;
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(CONTENT_SEARCH_DEBOUNCE)
                .await;
            // Superseded by a newer keystroke during the debounce window? Skip the grep.
            if this.update(cx, |this, _| this.finder_gen).unwrap_or(gen) != gen {
                return;
            }
            let hits = cx
                .background_executor()
                .spawn(async move {
                    Repo::discover(&root)
                        .map(|r| r.grep(&q))
                        .unwrap_or_default()
                })
                .await;
            this.update(cx, |this, cx| {
                // Apply only if still the latest search and the finder's still in Content mode.
                if this.finder_gen == gen
                    && this.finder_open
                    && this.finder_mode == FinderMode::Content
                {
                    this.content_results = hits
                        .into_iter()
                        .map(|(path, line, text)| ContentHit { path, line, text })
                        .collect();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn recompute_finder(&mut self, cx: &Context<Self>) {
        let q = self.finder_query.read(cx).text().to_string();
        let matcher = SkimMatcherV2::default();
        self.finder_selected = 0;
        match self.finder_mode {
            FinderMode::Files => {
                let mut scored: Vec<(i64, &PathBuf)> = self
                    .all_files
                    .iter()
                    .filter_map(|p| {
                        let s = p.to_string_lossy();
                        if q.is_empty() {
                            Some((0, p))
                        } else {
                            matcher.fuzzy_match(&s, &q).map(|sc| (sc, p))
                        }
                    })
                    .collect();
                scored.sort_by_key(|x| std::cmp::Reverse(x.0));
                self.finder_results = scored
                    .into_iter()
                    .take(FINDER_RESULT_CAP)
                    .map(|(_, p)| p.clone())
                    .collect();
            }
            FinderMode::Actions => {
                // "Reveal in Finder/Terminal" only make sense with an active/selected file.
                let have_file = self.open_path.is_some() || self.selected_path.is_some();
                let mut scored: Vec<(i64, usize)> = PALETTE
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, a, _))| {
                        have_file
                            || !matches!(
                                a,
                                PaletteAction::RevealInFinder | PaletteAction::RevealInTerminal
                            )
                    })
                    .filter_map(|(i, (label, _, _))| {
                        if q.is_empty() {
                            Some((0, i))
                        } else {
                            matcher.fuzzy_match(label, &q).map(|sc| (sc, i))
                        }
                    })
                    .collect();
                scored.sort_by_key(|x| std::cmp::Reverse(x.0));
                self.action_results = scored.into_iter().map(|(_, i)| i).collect();
            }
            FinderMode::Content => {
                // Literal (non-fuzzy) full-text search via `git grep`. Empty query → no hits.
                self.content_results = if q.is_empty() {
                    Vec::new()
                } else {
                    self.repo()
                        .map(|r| r.grep(&q))
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(path, line, text)| ContentHit { path, line, text })
                        .collect()
                };
            }
            FinderMode::Scratch => {
                let mut scored: Vec<(i64, usize)> = scratch::LANGS
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (label, _))| {
                        if q.is_empty() {
                            Some((0, i))
                        } else {
                            matcher.fuzzy_match(label, &q).map(|sc| (sc, i))
                        }
                    })
                    .collect();
                scored.sort_by_key(|x| std::cmp::Reverse(x.0));
                self.action_results = scored.into_iter().map(|(_, i)| i).collect();
            }
        }
    }

    fn open_finder(&mut self, mode: FinderMode, window: &mut Window, cx: &mut Context<Self>) {
        self.finder_mode = mode;
        self.finder_open = true;
        let placeholder = match mode {
            FinderMode::Files => "Type to search files…",
            FinderMode::Content => "Type to search file contents…",
            FinderMode::Actions => "Type to search actions…",
            FinderMode::Scratch => "Pick a language…",
        };
        self.finder_query.update(cx, |e, cx| {
            e.placeholder = placeholder.into();
            e.set_content(String::new(), Lang::PlainText, cx)
        });
        self.recompute_finder(cx);
        let handle = self.finder_query.read(cx).focus_handle.clone();
        window.focus(&handle);
        // First open: the input element isn't in the tree yet, so also focus next frame.
        window.defer(cx, move |window, _cx| window.focus(&handle));
        cx.notify();
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
    /// Backspace: delete the selected Browse-tree file/folder — identical to the
    /// right-click "Delete…" menu item (both route through `open_delete`, which pops the
    /// confirm modal). Browse mode only; no-op when nothing is selected.
    pub(crate) fn act_delete_file(
        &mut self,
        _: &DeleteFile,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode != Mode::Browse {
            return;
        }
        if let Some(path) = self.selected_path.clone() {
            self.open_delete(path, cx);
        }
    }

    pub(crate) fn act_find(&mut self, _: &FindInFile, window: &mut Window, cx: &mut Context<Self>) {
        self.open_find(false, window, cx);
    }
    pub(crate) fn act_replace(
        &mut self,
        _: &ReplaceInFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_find(true, window, cx);
    }
    /// The editor the find/replace bar currently acts on.
    fn find_ed(&self) -> Entity<CodeEditor> {
        match self.find_target {
            crate::FindTarget::File => self.file_editor.clone(),
            crate::FindTarget::DiffLeft => self.diff_left.clone(),
            crate::FindTarget::DiffRight => self.diff_right.clone(),
        }
    }
    pub(crate) fn open_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Works in the Browse editor (with a file open) and the Show-Diff view's panes.
        let target = if self.diff_view_open {
            // Default to the working (right) pane; the base (left) pane is read-only.
            if self.diff_left.read(cx).focus_handle.is_focused(window) {
                crate::FindTarget::DiffLeft
            } else {
                crate::FindTarget::DiffRight
            }
        } else if self.mode == Mode::Browse && self.open_path.is_some() {
            crate::FindTarget::File
        } else {
            return;
        };
        self.find_target = target;
        self.find_open = true;
        // Replace needs an editable target — the diff base pane / committed diffs are read-only,
        // so ⌘R there opens find only.
        self.find_replace = replace && !self.find_ed().read(cx).read_only;
        self.recompute_find(cx);
        let handle = self.find_query.read(cx).focus_handle.clone();
        window.focus(&handle);
        window.defer(cx, move |window, _cx| window.focus(&handle));
        cx.notify();
    }
    pub(crate) fn close_find(
        &mut self,
        _: &CloseFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.find_open = false;
        self.find_matches.clear();
        let ed = self.find_ed();
        // Only the file editor's `word_bg` is owned by find — the diff panes use `word_bg` for
        // word-level diff highlighting, so don't clobber it there.
        if self.find_target == crate::FindTarget::File {
            ed.update(cx, |e, _| e.word_bg.clear());
        }
        let handle = ed.read(cx).focus_handle.clone();
        window.focus(&handle);
        cx.notify();
    }
    /// Recompute match ranges for the current query (ASCII case-insensitive) and repaint
    /// the highlights + select the current match.
    fn recompute_find(&mut self, cx: &mut Context<Self>) {
        let q = self.find_query.read(cx).text().to_string();
        let content = self.find_ed().read(cx).text().to_string();
        self.find_matches.clear();
        if !q.is_empty() && q.len() <= content.len() {
            // `to_ascii_lowercase` preserves byte length, so positions map 1:1 to `content`.
            let hay = content.to_ascii_lowercase();
            let needle = q.to_ascii_lowercase();
            let mut from = 0usize;
            while let Some(pos) = hay[from..].find(&needle) {
                let s = from + pos;
                self.find_matches.push(s..s + needle.len());
                from = s + needle.len();
            }
        }
        if self.find_idx >= self.find_matches.len() {
            self.find_idx = 0;
        }
        self.apply_find_highlight(cx);
    }
    /// Paint match highlights on the editor (via its `word_bg`) and select the current one.
    fn apply_find_highlight(&mut self, cx: &mut Context<Self>) {
        let ed = self.find_ed();
        let content = ed.read(cx).text().to_string();
        let mut map: std::collections::HashMap<usize, Vec<std::ops::Range<usize>>> =
            std::collections::HashMap::new();
        for r in &self.find_matches {
            let line = content[..r.start].bytes().filter(|&b| b == b'\n').count();
            let line_start = content[..r.start].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let line_end = content[line_start..]
                .find('\n')
                .map(|i| line_start + i)
                .unwrap_or(content.len());
            let s = r.start - line_start;
            let e = (r.end.min(line_end)) - line_start;
            map.entry(line).or_default().push(s..e);
        }
        // The diff panes already use `word_bg` for word-level diff highlighting, so only paint
        // the amber match highlight on the file editor; in the diff the selection marks the
        // match (and find_next/prev navigate).
        if self.find_target == crate::FindTarget::File {
            ed.update(cx, |e, _| {
                e.word_bg = map;
                e.word_bg_color = gpui::rgba(0x6E5A1EFF); // amber search highlight
            });
        }
        if let Some(r) = self.find_matches.get(self.find_idx).cloned() {
            ed.update(cx, |e, cx| e.select_range(r, cx));
        }
        cx.notify();
    }
    pub(crate) fn find_next(&mut self, _: &FindNext, _w: &mut Window, cx: &mut Context<Self>) {
        if self.find_matches.is_empty() {
            return;
        }
        self.find_idx = (self.find_idx + 1) % self.find_matches.len();
        if let Some(r) = self.find_matches.get(self.find_idx).cloned() {
            self.find_ed().update(cx, |e, cx| e.select_range(r, cx));
        }
        cx.notify();
    }
    pub(crate) fn find_prev(&mut self, _: &FindPrev, _w: &mut Window, cx: &mut Context<Self>) {
        if self.find_matches.is_empty() {
            return;
        }
        self.find_idx = (self.find_idx + self.find_matches.len() - 1) % self.find_matches.len();
        if let Some(r) = self.find_matches.get(self.find_idx).cloned() {
            self.find_ed().update(cx, |e, cx| e.select_range(r, cx));
        }
        cx.notify();
    }
    pub(crate) fn replace_one(&mut self, _: &ReplaceOne, _w: &mut Window, cx: &mut Context<Self>) {
        let rep = self.replace_query.read(cx).text().to_string();
        if let Some(r) = self.find_matches.get(self.find_idx).cloned() {
            self.find_ed()
                .update(cx, |e, cx| e.replace_range_text(r, &rep, cx));
            // The edit fires autosave + Changed; re-scan against the new content.
            self.recompute_find(cx);
        }
    }
    pub(crate) fn replace_all(&mut self, _: &ReplaceAll, _w: &mut Window, cx: &mut Context<Self>) {
        let rep = self.replace_query.read(cx).text().to_string();
        // Replace right-to-left so earlier ranges stay valid.
        let ranges: Vec<_> = self.find_matches.clone();
        self.find_ed().update(cx, |e, cx| {
            for r in ranges.into_iter().rev() {
                e.replace_range_text(r, &rep, cx);
            }
        });
        self.recompute_find(cx);
    }

    pub(crate) fn act_go_to_file(
        &mut self,
        _: &GoToFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_finder(FinderMode::Files, window, cx);
    }
    pub(crate) fn act_find_in_files(
        &mut self,
        _: &FindInFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_finder(FinderMode::Content, window, cx);
    }
    pub(crate) fn act_actions(&mut self, _: &Actions, window: &mut Window, cx: &mut Context<Self>) {
        self.open_finder(FinderMode::Actions, window, cx);
    }

    /// Open `rel` in the editor, switch to Browse, select the 1-based `line`, and scroll
    /// it near the top — used by the Find-in-Files results to jump straight to a match.
    pub(crate) fn open_file_at_line(
        &mut self,
        rel: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file(rel, cx);
        self.mode = Mode::Browse;
        let line0 = line.saturating_sub(1) as usize;
        self.file_editor.update(cx, |e, cx| {
            // Compute the line's byte range first (immutable borrow), then select it.
            let range = {
                let text = e.text();
                let start: usize = text
                    .split_inclusive('\n')
                    .take(line0)
                    .map(|l| l.len())
                    .sum();
                let len = text[start..].find('\n').unwrap_or(text.len() - start);
                start..start + len
            };
            e.select_range(range, cx);
        });
        // Scroll so the target line sits a few rows below the top (negative offset = down).
        let lh = editor::line_height_px();
        let y = -(line0.saturating_sub(SCROLL_CONTEXT_ROWS) as f32) * lh;
        let sh = self.file_scroll.clone();
        window.defer(cx, move |_w, _cx| {
            sh.set_offset(gpui::point(gpui::px(0.0), gpui::px(y)))
        });
        window.focus(&self.focus_handle);
        cx.notify();
    }
    pub(crate) fn act_new_scratch(
        &mut self,
        _: &NewScratch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_finder(FinderMode::Scratch, window, cx);
    }

    /// Create a scratch file of the given extension and open it.
    pub(crate) fn create_scratch(&mut self, ext: &str, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        match scratch::create(&root, ext) {
            Ok(path) => {
                self.refresh();
                self.mode = Mode::Browse;
                self.open_file(path, cx);
            }
            Err(e) => eprintln!("scratch create failed: {e:#}"),
        }
        cx.notify();
    }

    pub(crate) fn run_palette(
        &mut self,
        a: PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finder_open = false;
        match a {
            PaletteAction::GoToFile => self.open_finder(FinderMode::Files, window, cx),
            PaletteAction::FindInFiles => self.open_finder(FinderMode::Content, window, cx),
            PaletteAction::NewScratch => self.open_finder(FinderMode::Scratch, window, cx),
            PaletteAction::CommitView => self.enter_commit(cx),
            PaletteAction::BrowseView => {
                self.mode = Mode::Browse;
                window.focus(&self.focus_handle);
                cx.notify();
            }
            PaletteAction::SelectInTree => {
                window.focus(&self.focus_handle);
                self.reveal_in_tree(cx);
            }
            PaletteAction::Rollback => {
                window.focus(&self.focus_handle);
                self.open_rollback_path(PathBuf::new(), cx);
            }
            PaletteAction::Settings => {
                self.onboarding_choice = self.keymap.preset;
                self.onboarding_open = true;
                window.focus(&self.focus_handle);
                cx.notify();
            }
            PaletteAction::RevealInFinder => {
                window.focus(&self.focus_handle);
                if let Some(p) = self
                    .open_path
                    .clone()
                    .or_else(|| self.selected_path.clone())
                {
                    self.reveal_in_os(&p, cx);
                }
            }
            PaletteAction::RevealInTerminal => {
                window.focus(&self.focus_handle);
                if let Some(p) = self
                    .open_path
                    .clone()
                    .or_else(|| self.selected_path.clone())
                {
                    self.reveal_in_terminal(&p, cx);
                }
            }
            PaletteAction::Plugins => {
                self.open_modal_window(ModalKind::Plugins, "Language Plugins", 520.0, 560.0, cx);
            }
            PaletteAction::Fonts => {
                self.open_modal_window(ModalKind::Fonts, "Fonts", 760.0, 620.0, cx);
            }
        }
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
    pub(crate) fn finder_close(
        &mut self,
        _: &FinderClose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finder_open = false;
        window.focus(&self.focus_handle);
        cx.notify();
    }
    pub(crate) fn finder_up(&mut self, _: &FinderUp, _: &mut Window, cx: &mut Context<Self>) {
        self.finder_selected = self.finder_selected.saturating_sub(1);
        cx.notify();
    }
    pub(crate) fn finder_down(&mut self, _: &FinderDown, _: &mut Window, cx: &mut Context<Self>) {
        let len = match self.finder_mode {
            FinderMode::Files => self.finder_results.len(),
            FinderMode::Content => self.content_results.len(),
            FinderMode::Actions | FinderMode::Scratch => self.action_results.len(),
        };
        if self.finder_selected + 1 < len {
            self.finder_selected += 1;
        }
        cx.notify();
    }
    pub(crate) fn finder_confirm(
        &mut self,
        _: &FinderConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.finder_mode {
            FinderMode::Files => {
                if let Some(p) = self.finder_results.get(self.finder_selected).cloned() {
                    self.open_file(p, cx);
                    self.mode = Mode::Browse;
                }
                self.finder_open = false;
                window.focus(&self.focus_handle);
                cx.notify();
            }
            FinderMode::Content => {
                if let Some(hit) = self.content_results.get(self.finder_selected).cloned() {
                    self.finder_open = false;
                    self.open_file_at_line(hit.path, hit.line, window, cx);
                } else {
                    self.finder_open = false;
                    window.focus(&self.focus_handle);
                }
                cx.notify();
            }
            FinderMode::Actions => {
                if let Some(&i) = self.action_results.get(self.finder_selected) {
                    self.run_palette(PALETTE[i].1, window, cx);
                } else {
                    self.finder_open = false;
                    window.focus(&self.focus_handle);
                    cx.notify();
                }
            }
            FinderMode::Scratch => {
                let ext = self
                    .action_results
                    .get(self.finder_selected)
                    .map(|&i| scratch::LANGS[i].1);
                self.finder_open = false;
                window.focus(&self.focus_handle);
                if let Some(ext) = ext {
                    self.create_scratch(ext, cx);
                } else {
                    cx.notify();
                }
            }
        }
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
    pub(crate) fn act_commit(&mut self, _: &DoCommit, _: &mut Window, cx: &mut Context<Self>) {
        // ⌘K opens the git view with the current file selected (the actual commit happens
        // from the Commit button), IntelliJ-style.
        self.enter_commit(cx);
    }
    pub(crate) fn act_mode_commit(
        &mut self,
        _: &ModeCommit,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.enter_commit(cx);
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

    /// Switch to Commit mode: re-read git status (so edits made in Browse show up) and
    /// load the selected file into the diff editors.
    pub(crate) fn enter_commit(&mut self, cx: &mut Context<Self>) {
        self.mode = Mode::Commit;
        self.diff_view_open = false;
        // Drop the caret into the commit-message box on the next frame (render_commit consumes
        // this once the input element is in the tree).
        self.focus_commit_msg = true;
        if let Some(repo) = self.repo() {
            self.files = repo.status().unwrap_or_default();
            self.push_base = repo.push_base();
            self.push_files = repo.push_files();
        }
        // Default to the tab that has work: Push if there's nothing to commit but commits
        // are waiting to be pushed; Commit otherwise.
        self.git_tab = if self.files.is_empty() && !self.push_files.is_empty() {
            GitTab::Push
        } else {
            GitTab::Commit
        };
        // On the Push tab, select the first push file so its diff shows immediately.
        if self.git_tab == GitTab::Push {
            self.rebuild_commit_view(true);
            self.select_push_file(0, cx);
            cx.notify();
            return;
        }
        self.rebuild_commit_view(true);
        // Prefer the currently-open file, else the prior selection, else the first change.
        let idx = self
            .open_path
            .as_ref()
            .and_then(|p| self.files.iter().position(|f| &f.path == p))
            .or(match self.selected {
                Some(i) if i < self.files.len() => Some(i),
                _ => None,
            })
            .or(if self.files.is_empty() { None } else { Some(0) });
        match idx {
            Some(i) => self.select_with(i, Some(cx)),
            None => {
                self.selected = None;
                self.diff_path = None;
                self.diff_left.update(cx, |e, cx| {
                    e.set_content(String::new(), Lang::PlainText, cx)
                });
                self.diff_right.update(cx, |e, cx| {
                    e.set_content(String::new(), Lang::PlainText, cx)
                });
            }
        }
        cx.notify();
    }

    // ── History (git log) view ────────────────────────────────────────────
    /// Enter the history view for the whole repo, logging the current branch.
    pub(crate) fn enter_history(&mut self, cx: &mut Context<Self>) {
        self.history_path = None;
        self.enter_history_inner(cx);
    }

    /// Enter the history view scoped to `path` (a folder/file) — commits that touched that
    /// subtree, recursively. Opened from a Browse-tree folder's right-click → "Git History".
    pub(crate) fn enter_history_for(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // Root path (empty) = whole repo.
        self.history_path = if path.as_os_str().is_empty() {
            None
        } else {
            Some(path)
        };
        self.enter_history_inner(cx);
    }

    fn enter_history_inner(&mut self, cx: &mut Context<Self>) {
        self.mode = Mode::History;
        self.diff_view_open = false;
        self.history_rev = self
            .current_branch
            .clone()
            .unwrap_or_else(|| "HEAD".to_string());
        self.reload_history(cx);
    }

    /// Reload the commit list for `history_rev` (scoped to `history_path`), select newest.
    pub(crate) fn reload_history(&mut self, cx: &mut Context<Self>) {
        let path = self.history_path.clone();
        self.history_commits = self
            .repo()
            .and_then(|r| r.log(&self.history_rev, 300, path.as_deref()).ok())
            .unwrap_or_default();
        self.history_selected = None;
        self.history_files.clear();
        self.history_file_selected = None;
        if self.history_commits.is_empty() {
            cx.notify();
        } else {
            self.select_history_commit(0, cx);
        }
    }

    /// Toggle the history branch dropdown, loading local + remote branches when opening it
    /// and resetting the search box.
    pub(crate) fn toggle_history_branches(&mut self, cx: &mut Context<Self>) {
        if !self.history_branch_open {
            if let Some(r) = self.repo() {
                self.history_locals = r.branches().unwrap_or_default();
                self.history_remotes = r.remote_branches().unwrap_or_default();
            }
            self.history_branch_query.update(cx, |e, cx| {
                e.set_content(String::new(), Lang::PlainText, cx)
            });
        }
        self.history_branch_open = !self.history_branch_open;
        cx.notify();
    }

    /// Point the log at a different branch/rev (from the branch dropdown).
    pub(crate) fn set_history_rev(&mut self, rev: String, cx: &mut Context<Self>) {
        self.history_rev = rev;
        self.history_branch_open = false;
        self.reload_history(cx);
    }

    /// `(from, to)` revisions for the current compare mode against `hash`; `to == None`
    /// means the working tree.
    fn history_revs(&self, hash: &str) -> (String, Option<String>) {
        match self.history_compare {
            CompareMode::Before => (format!("{hash}^"), Some(hash.to_string())),
            CompareMode::Local => (hash.to_string(), None),
            CompareMode::BeforeLocal => (format!("{hash}^"), None),
        }
    }

    fn recompute_history_files(&mut self) {
        self.history_files.clear();
        let (Some(idx), Some(repo)) = (self.history_selected, self.repo()) else {
            return;
        };
        let Some(commit) = self.history_commits.get(idx) else {
            return;
        };
        let (from, to) = self.history_revs(&commit.hash);
        let path = self.history_path.clone();
        self.history_files = repo.diff_files(&from, to.as_deref(), path.as_deref());
        // Folder tree of the changed files, fully expanded so every change is visible.
        let paths: Vec<PathBuf> = self.history_files.iter().map(|f| f.path.clone()).collect();
        self.history_files_tree = tree::Tree::build(&paths);
        self.history_files_expanded.clear();
        self.history_files_expanded.insert(PathBuf::new());
        for p in &paths {
            for anc in p.ancestors().skip(1) {
                self.history_files_expanded.insert(anc.to_path_buf());
            }
        }
    }

    /// Select a commit → recompute its changed files (per compare mode) + open the first.
    pub(crate) fn select_history_commit(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.history_selected = Some(idx);
        self.recompute_history_files();
        self.history_file_selected = None;
        if self.history_files.is_empty() {
            self.diff_path = None;
            cx.notify();
        } else {
            self.select_history_file(0, cx);
        }
    }

    /// Right-click a commit → pick a compare mode for it: select that commit, then apply the
    /// mode (mirrors the header dropdown, which acts on the selected commit).
    pub(crate) fn history_compare_commit(
        &mut self,
        idx: usize,
        mode: CompareMode,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        self.history_selected = Some(idx);
        self.set_history_compare(mode, cx);
    }

    /// Change the compare mode (vs parent / latest / local) → refresh files + diff.
    pub(crate) fn set_history_compare(&mut self, mode: CompareMode, cx: &mut Context<Self>) {
        self.history_compare = mode;
        self.history_compare_open = false;
        match self.history_selected {
            Some(idx) => self.select_history_commit(idx, cx),
            None => cx.notify(),
        }
    }

    /// Load a file's diff for the selected commit + compare mode (read-only).
    pub(crate) fn select_history_file(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.history_file_selected = Some(idx);
        let (Some(cidx), Some(repo)) = (self.history_selected, self.repo()) else {
            return;
        };
        let Some(commit) = self.history_commits.get(cidx).cloned() else {
            return;
        };
        let Some(file) = self.history_files.get(idx).cloned() else {
            return;
        };
        let (from, to) = self.history_revs(&commit.hash);
        // Right side is the live working tree (`to == None`) → editable + the `»` chevrons
        // replace the working hunk with the left (committed) version. Comparing two committed
        // revisions has nothing to edit, so it stays read-only.
        let editable = to.is_none();
        let before = repo
            .committed_content(&from, &file.path)
            .unwrap_or_default();
        let after = match to {
            Some(rev) => repo.committed_content(&rev, &file.path).unwrap_or_default(),
            None => repo.working_content(&file.path).unwrap_or_default(),
        };
        let lang = self.effective_lang(&file.path);
        self.load_diff_panes(file.path.clone(), before, after, lang, !editable, cx);
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
