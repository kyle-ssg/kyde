//! Push view — the Push tab + push confirmation modal (commits ahead of upstream).
//! Crate-root child module.

use super::*;

impl Kyde {
    /// Left column of the Push tab: a flat list of the files a push would send (click → diff)
    /// + a footer with the Push button. No checkboxes — a push is all-or-nothing.
    pub(crate) fn render_push_left(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let n = self.push_files.len();
        let rows: Vec<gpui::AnyElement> = self
            .push_files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let selected = self.push_selected == Some(i);
                let name: SharedString = f
                    .path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| f.path.to_string_lossy().into_owned())
                    .into();
                let dir = f
                    .path
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty());
                let name_color = status_color(f.status);
                let path = f.path.clone();
                div()
                    .id(("push-file", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .mx(px(6.0))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.selected_bg))
                    .when(!selected, |d| d.hover(|s| s.bg(t.bg_mid)))
                    .child(div().flex_none().child(badge_inner(file_badge(&path), 2.0)))
                    .child(div().flex_none().text_color(name_color).child(name))
                    .when_some(dir, |d, dir| {
                        d.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(t.line_number)
                                .child(SharedString::from(dir)),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.select_push_file(i, cx)),
                    )
                    .into_any_element()
            })
            .collect();

        let count_header = div()
            .flex_none()
            .px_3()
            .py_1p5()
            .text_color(t.line_number)
            .child(SharedString::from(if n == 0 {
                "Nothing to push".to_string()
            } else {
                format!("{n} file{} to push", if n == 1 { "" } else { "s" })
            }));
        let list = div()
            .id("push-list")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
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
            .child(count_header)
            .child(div().flex_none().h(px(1.0)).mx_1().bg(t.divider))
            .child(list);

        // Footer bar: Cancel (back to Browse) + Push, styled like the commit bar's buttons.
        let cancel_btn = div()
            .px_4()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(t.divider)
            .text_color(t.secondary_text)
            .cursor_pointer()
            .child("Cancel")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.mode = Mode::Browse;
                    cx.notify();
                }),
            );
        let pushing = self.pushing;
        let push_btn =
            btn_primary_state("push", if pushing { "Pushing…" } else { "Push" }, pushing)
                .py_2()
                .font_weight(FontWeight::SEMIBOLD)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.do_push(cx)),
                );
        let bar = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .flex_none()
            .p_2()
            .bg(t.panel_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .font_family(ui)
            .child(cancel_btn)
            .when(n > 0, |d| d.child(push_btn));

        div()
            .flex()
            .flex_col()
            .gap(px(theme::FRAME_GAP))
            .w(px(self.tree_width))
            .flex_none()
            .h_full()
            .child(list_island)
            .child(bar)
            .into_any_element()
    }

    /// Delete-confirmation modal: name the target, Cancel / Delete.
    /// Push confirmation modal: branch + the commits that would be pushed, Cancel / Push.
    /// Push confirmation window body (own native window; titlebar shows "Push").
    pub(crate) fn render_push_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        let n = self.push_files.len();
        // Flat file list, styled like the commit/rollback rows (badge + name + status
        // color), right-click → "View Diff". No checkboxes — a push is all-or-nothing.
        let rows: Vec<gpui::AnyElement> = self
            .push_files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let name: SharedString = f
                    .path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| f.path.to_string_lossy().into_owned())
                    .into();
                let dir = f
                    .path
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty());
                let name_color = status_color(f.status);
                let path = f.path.clone();
                div()
                    .id(("push-file", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .mx(px(6.0))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.bg_mid))
                    .child(div().flex_none().child(badge_inner(file_badge(&path), 2.0)))
                    .child(div().flex_none().text_color(name_color).child(name))
                    .when_some(dir, |d, dir| {
                        d.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(t.line_number)
                                .child(SharedString::from(dir)),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                            this.open_menu(e.position, MenuTarget::PushFile(i), cx);
                        }),
                    )
                    .into_any_element()
            })
            .collect();

        let list = div()
            .id("push-list")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .flex_1()
            .py_1()
            .child(
                div()
                    .px_3()
                    .py_1()
                    .text_color(t.line_number)
                    .child(SharedString::from(if n == 0 {
                        "Nothing to push.".to_string()
                    } else {
                        format!("{n} file{} to push", if n == 1 { "" } else { "s" })
                    })),
            )
            .children(rows);

        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(t.divider)
            .child(
                btn_secondary("push-cancel", "Cancel")
                    .py_1p5()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.close_modal_window(ModalKind::Push, cx);
                        }),
                    ),
            )
            .child(
                btn_primary_state(
                    "push",
                    if self.pushing { "Pushing…" } else { "Push" },
                    self.pushing,
                )
                .py_1p5()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.do_push(cx)),
                ),
            );

        let mut panel = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.panel_bg)
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size))
            .text_color(t.text)
            .child(list)
            .child(footer);
        // Right-click → View Diff menu renders inside THIS window (target = PushFile).
        if matches!(
            self.context_menu.as_ref().map(|m| &m.target),
            Some(MenuTarget::PushFile(_))
        ) {
            panel = panel.child(self.render_context_menu(cx));
        }
        panel.into_any_element()
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

}
