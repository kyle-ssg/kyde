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
    pub(crate) fn load_font_preview(&mut self, cx: &mut Context<Self>) {
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




    /// Record a failed git operation so the user sees it (op-error banner) instead of a
    /// silent no-op. `ctx` is a short human label ("Commit", "Push", …); the error is
    /// stringified after it. Still logs to stderr for debugging.
    pub(crate) fn fail(&mut self, ctx: &str, e: anyhow::Error) {
        eprintln!("{ctx} failed: {e:#}");
        self.op_error = Some(format!("{ctx} failed: {e}"));
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
