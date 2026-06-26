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
            branch_remotes: Vec::new(),
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

    /// Record a failed git operation so the user sees it (op-error banner) instead of a
    /// silent no-op. `ctx` is a short human label ("Commit", "Push", …); the error is
    /// stringified after it. Still logs to stderr for debugging.
    pub(crate) fn fail(&mut self, ctx: &str, e: anyhow::Error) {
        eprintln!("{ctx} failed: {e:#}");
        self.op_error = Some(format!("{ctx} failed: {e}"));
    }

    /// Reset the editor to nothing-open.
    pub(crate) fn clear_open(&mut self, cx: &mut Context<Self>) {
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
impl Kyde {}
