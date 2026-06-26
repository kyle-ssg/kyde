//! Commit (git staging) view — changed-files tree, per-hunk staging, commit message box.
//! Crate-root child module.

use crate::*;

impl Kyde {
    pub(crate) fn render_commit(
        &mut self,
        ui: &'static str,
        fs: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let commit_n = self.files.len();
        let push_n = self.push_files.len();
        // Nothing to commit AND nothing to push → a single centered message.
        if commit_n == 0 && push_n == 0 {
            return div()
                .flex()
                .flex_1()
                .h_full()
                .items_center()
                .justify_center()
                .bg(t.main_bg)
                .rounded(px(theme::ISLAND_RADIUS))
                .font_family(ui)
                .text_size(px(theme::get().ui_font_size + 1.0))
                .text_color(t.line_number)
                .child("You have nothing to commit or push")
                .into_any_element();
        }

        // Only tabs with content are shown; fall back to the available one if the selected
        // tab is the empty one (state is normalised after commit/push, this is a display guard).
        let active = match self.git_tab {
            GitTab::Commit if commit_n == 0 => GitTab::Push,
            GitTab::Push if push_n == 0 => GitTab::Commit,
            other => other,
        };
        // Tab bar (Commit / Push), then the active tab's left column + shared diff pane.
        let tabs = self.render_git_tabs(active, cx);
        // The Commit tab's files panel can be minimised to a thin strip (the `−` button in its
        // header), handing the full width to the side-by-side diff.
        let collapsed = active == GitTab::Commit && self.commit_collapsed;
        // Auto-focus the commit-message box when the Commit view opens (deferred so the input
        // element is in the tree first). Only when its tab is active and not collapsed — the
        // box isn't rendered otherwise.
        if self.focus_commit_msg && active == GitTab::Commit && !collapsed {
            self.focus_commit_msg = false;
            let handle = self.commit_editor.read(cx).focus_handle.clone();
            window.focus(&handle);
            window.defer(cx, move |window, _cx| window.focus(&handle));
        }
        let divider = div()
            .id("commit-divider")
            .w(px(theme::FRAME_GAP))
            .h_full()
            .flex_none()
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                    this.start_divider_drag(Divider::Tree, e.position, window);
                    cx.notify();
                }),
            );
        // `min_w_0` is essential: without it the diff island (flex_1) sizes to its wide editor
        // content instead of the remaining row width, and the last flex child (the right diff
        // pane) collapses to zero — the side-by-side then shows only the base. History's diff
        // works because its container already sets `min_w_0`.
        let mut body = div().flex().flex_row().flex_1().min_h_0().min_w_0();
        if collapsed {
            // Thin strip with a `»` expand button where the files panel was (same affordance as
            // the Browse tree's collapsed strip). No resize divider — nothing to resize.
            body = body.child(
                div()
                    .flex_none()
                    .h_full()
                    .w(px(30.0))
                    .py_1()
                    .bg(t.panel_bg)
                    .rounded(px(theme::ISLAND_RADIUS))
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .id("commit-expand")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(24.0))
                            .rounded_md()
                            .text_size(px(15.0))
                            .text_color(t.line_number)
                            .hover(|s| s.bg(t.bg_light).text_color(t.text))
                            .cursor_pointer()
                            .tooltip(|_w, cx| cx.new(|_| Tip("Expand files panel".into())).into())
                            .child("»")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _e, _w, cx| {
                                    this.commit_collapsed = false;
                                    cx.notify();
                                }),
                            ),
                    ),
            );
        } else {
            let left = match active {
                GitTab::Commit => self.render_commit_left(ui, cx),
                GitTab::Push => self.render_push_left(ui, cx),
            };
            body = body.child(left).child(divider);
        }
        let body = body.child(self.render_diff(ui, fs, Some(window), cx));

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(tabs)
            .child(body)
            .into_any_element()
    }

    /// Tab strip atop the git view: Commit (working changes) and Push (committed-but-unpushed),
    /// each with a count badge when non-empty.
    fn render_git_tabs(&self, active: GitTab, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let ui = theme::font::UI_FAMILY;
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .flex_none()
            .px_1()
            .pb(px(theme::FRAME_GAP))
            .font_family(ui)
            .text_size(px(t.ui_font_size));
        // A tab is shown only when it has files (reusable pill component, IntelliJ-style).
        if !self.files.is_empty() {
            row = row.child(
                tab_pill(
                    "git-tab-commit",
                    "Commit",
                    self.files.len(),
                    active == GitTab::Commit,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.set_git_tab(GitTab::Commit, cx)),
                ),
            );
        }
        if !self.push_files.is_empty() {
            row = row.child(
                tab_pill(
                    "git-tab-push",
                    "Push",
                    self.push_files.len(),
                    active == GitTab::Push,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.set_git_tab(GitTab::Push, cx)),
                ),
            );
        }
        row.into_any_element()
    }

    /// Left column of the Commit tab: the changed-files tree (search + checkboxes) + the
    /// commit-message bar.
    fn render_commit_left(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let root_name = self
            .repo_root
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        // Changed files as a folder tree (root + everything expanded by rebuild_commit_view).
        let mut visible = vec![tree::Row {
            path: PathBuf::new(),
            is_dir: true,
            depth: 0,
        }];
        if self.commit_expanded.contains(&PathBuf::new()) {
            for mut r in self.commit_tree.visible(&self.commit_expanded) {
                r.depth += 1;
                visible.push(r);
            }
        }
        // Filter the changed-files list by the search box: keep the root, files whose path
        // matches, and folders that contain a matching file.
        let query = self.commit_search.read(cx).text().trim().to_lowercase();
        if !query.is_empty() {
            let files = &self.files;
            visible.retain(|r| {
                r.path.as_os_str().is_empty()
                    || (!r.is_dir && r.path.to_string_lossy().to_lowercase().contains(&query))
                    || (r.is_dir
                        && files.iter().any(|f| {
                            f.path.starts_with(&r.path)
                                && f.path.to_string_lossy().to_lowercase().contains(&query)
                        }))
            });
        }
        let rows: Vec<gpui::AnyElement> = visible
            .into_iter()
            .map(|r| {
                let is_root = r.path.as_os_str().is_empty();
                let checked = if r.is_dir {
                    self.folder_all_checked(&r.path)
                } else {
                    self.commit_checked.contains(&r.path)
                };
                let file_idx = (!r.is_dir)
                    .then(|| self.files.iter().position(|f| f.path == r.path))
                    .flatten();
                let selected = file_idx.is_some() && self.selected == file_idx;
                let name_color = file_idx
                    .and_then(|i| self.files.get(i))
                    .map(|f| status_color(f.status))
                    .unwrap_or(t.text);
                let name: SharedString = if is_root {
                    root_name.clone().into()
                } else {
                    r.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into()
                };
                let expanded = self.commit_expanded.contains(&r.path);
                let is_dir = r.is_dir;
                let (p_act, p_check, p_ctx) = (r.path.clone(), r.path.clone(), r.path.clone());
                ui::tree::item(
                    cx,
                    self.dragging(Divider::Tree),
                    &r.path,
                    is_dir,
                    expanded,
                    r.depth,
                    selected,
                    name,
                    name_color,
                    Some(checked),
                    move |this, _e, _w, cx| {
                        if is_dir {
                            this.toggle_commit_dir(p_act.clone(), cx);
                        } else if let Some(i) = this.files.iter().position(|f| f.path == p_act) {
                            this.select_with(i, Some(cx));
                            cx.notify();
                        }
                    },
                    move |this, cx| this.toggle_commit_check(p_check.clone(), is_dir, cx),
                    move |this, pos, cx| {
                        this.open_menu(pos, MenuTarget::CommitPath(p_ctx.clone(), is_dir), cx);
                    },
                )
            })
            .collect();
        // File-list island (same island styling/width as the Browse tree): a fixed search
        // header (filter box + divider) over the scrollable changed-files list.
        // Header: filter box + a `−` button that minimises the panel to a thin strip (same
        // affordance as the Browse tree), handing the full width to the side-by-side diff.
        let collapse_btn = div()
            .id("commit-minimize")
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(px(22.0))
            .rounded_md()
            .text_size(px(16.0))
            .text_color(t.line_number)
            .hover(|s| s.bg(t.bg_light).text_color(t.text))
            .cursor_pointer()
            .tooltip(|_w, cx| cx.new(|_| Tip("Collapse files panel".into())).into())
            .child("−")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.commit_collapsed = true;
                    cx.notify();
                }),
            );
        let search_header = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1p5()
            .text_size(px(theme::get().ui_font_size))
            .child(div().flex_1().min_w_0().child(self.commit_search.clone()))
            .child(collapse_btn);
        let search_hr = div().flex_none().h(px(1.0)).mx_1().bg(t.divider);
        let file_list = div()
            .id("commit-tree")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .px_1()
            .py_1()
            .children(rows);
        let list_island = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .bg(t.panel_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .text_color(t.text)
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size + 1.0))
            .child(search_header)
            .child(search_hr)
            .child(file_list);

        // Left column: file list + commit message, the same width as the Browse tree.
        div()
            .flex()
            .flex_col()
            .gap(px(theme::FRAME_GAP))
            .w(px(self.tree_width))
            .flex_none()
            .h_full()
            .child(list_island)
            .child(self.render_commit_bar(cx))
            .into_any_element()
    }

    fn render_commit_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        // Cancel → back to the Browse (code) view.
        let cancel_btn = div()
            .px_4()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(t.divider)
            .text_color(t.secondary_text)
            .child("Cancel")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.mode = Mode::Browse;
                    cx.notify();
                }),
            );
        // Emphasized primary CTA: standard primary button + taller pad + semibold. While a
        // commit is in flight it's dimmed + labelled "Committing…" (clicks are no-ops).
        let committing = self.committing;
        let commit_btn = btn_primary_state(
            "commit",
            if committing {
                "Committing…"
            } else {
                "Commit"
            },
            committing,
        )
        .py_2()
        .font_weight(FontWeight::SEMIBOLD)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| {
                this.commit_now(cx);
                cx.notify();
            }),
        );

        div()
            .flex()
            .flex_col()
            .gap_2()
            .h(px(150.0))
            .flex_none()
            .p_2()
            .bg(t.panel_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p_2()
                    .bg(t.bg_mid)
                    .rounded_md()
                    .child(self.commit_editor.clone()),
            )
            // Cancel + Commit on their own line, right-aligned.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(cancel_btn)
                    .child(commit_btn),
            )
            .into_any_element()
    }

    pub(crate) fn select(&mut self, idx: usize) {
        self.select_with(idx, None);
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

    /// Browse → "Commit": jump to the Commit view, selecting the file if it changed.
    /// Indices into `self.files` that are committable/rollback-able under `path`:
    /// a file → itself; a folder → every change beneath it; the repo root (`""`) → all.
    pub(crate) fn changed_under(&self, path: &std::path::Path) -> Vec<usize> {
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
    pub(crate) fn rebuild_commit_view(&mut self, check_all: bool) {
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

    /// After a revert leaves the working tree clean, drop back to the file (Browse) view —
    /// there's nothing left to commit, so the git view would just be empty.
    pub(crate) fn exit_commit_if_clean(&mut self) {
        if self.mode == Mode::Commit && self.files.is_empty() {
            self.mode = Mode::Browse;
        }
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
        self.switch_mode(Mode::Commit, cx);
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
}
