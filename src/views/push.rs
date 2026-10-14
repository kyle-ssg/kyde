//! Push view — the Push tab + push confirmation modal (commits ahead of upstream).
//! Crate-root child module.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::{
    btn_primary_state, btn_secondary, div, nav_keys, px, status_color, theme, tree, ui, Context,
    Divider, FluentBuilder, FontWeight, GitTab, InteractiveElement, IntoElement, Kyde, MenuTarget,
    ModalKind, Mode, MouseButton, NavTarget, ParentElement, Repo, SharedString,
    StatefulInteractiveElement, Styled,
};

impl Kyde {
    /// Left column of the Push tab: the files a push would send as a folder tree (click → diff)
    /// + a footer with the Push button. No checkboxes — a push is all-or-nothing.
    pub(crate) fn render_push_left(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let n = self.sync.push_files.len();
        let rows = self.push_tree_rows(true, cx);

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
        let list = nav_keys(
            div()
                .id("push-list")
                .track_focus(&self.commit.push_focus)
                .key_context("NavList NavTree"),
            NavTarget::PushList,
            cx,
        )
        .overflow_y_scroll()
        .track_scroll(&self.sync.push_scroll)
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
        let cancel_btn = btn_secondary("push-tab-cancel", "Cancel")
            .py_2()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.mode = Mode::Browse;
                    cx.notify();
                }),
            );
        let pushing = self.sync.pushing;
        let push_btn =
            btn_primary_state("push", if pushing { "Pushing…" } else { "Push" }, pushing)
                .py_2()
                .font_weight(FontWeight::SEMIBOLD)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.do_push(cx)),
                );
        let bar = ui::modal_footer()
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
            .w(px(self.browse.tree_width))
            .flex_none()
            .h_full()
            .child(list_island)
            .child(bar)
            .into_any_element()
    }

    /// Push confirmation window body (own native window; titlebar shows "Push"): the files
    /// the push would send, Cancel / Push.
    pub(crate) fn render_push_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        let n = self.sync.push_files.len();
        // Same folder tree as the Push tab; right-click a file → "View Diff". No checkboxes —
        // a push is all-or-nothing.
        let rows = self.push_tree_rows(false, cx);

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

        let footer = ui::window_footer()
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
                    if self.sync.pushing {
                        "Pushing…"
                    } else {
                        "Push"
                    },
                    self.sync.pushing,
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

    /// The push files as a folder tree (root row = the repo, every folder expanded on load),
    /// shared by the Push tab and the Push window. In the tab (`in_tab`) clicking a file
    /// shows its diff; in the window a right-click offers "View Diff".
    fn push_tree_rows(&self, in_tab: bool, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let t = theme::get();
        let sync = &self.sync;
        let root_name = self
            .repo_root
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or_else(|| "/".to_string(), |s| s.to_string_lossy().into_owned());
        let index: HashMap<&std::path::Path, usize> = sync
            .push_files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.as_path(), i))
            .collect();
        let suppress_hover = in_tab && self.dragging(Divider::Tree);
        self.push_visible_rows()
            .into_iter()
            .map(|r| {
                let is_root = r.path.as_os_str().is_empty();
                let file_idx = (!r.is_dir)
                    .then(|| index.get(r.path.as_path()).copied())
                    .flatten();
                // The keyboard cursor: a folder when it's on one, else the selected file.
                let selected = in_tab
                    && match &sync.push_dir_cursor {
                        Some(dir) => *dir == r.path,
                        None => file_idx.is_some() && sync.push_selected == file_idx,
                    };
                let name_color = file_idx
                    .and_then(|i| sync.push_files.get(i))
                    .map_or(t.text, |f| status_color(f.status));
                let name: SharedString = if is_root {
                    root_name.clone().into()
                } else {
                    r.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into()
                };
                let expanded = sync.push_expanded.contains(&r.path);
                let is_dir = r.is_dir;
                let dir = r.path.clone();
                ui::tree::item(
                    cx,
                    suppress_hover,
                    &r.path,
                    is_dir,
                    expanded,
                    r.depth,
                    selected,
                    name,
                    name_color,
                    None,
                    None,
                    false,
                    None,
                    None,
                    move |this, _e, window, cx| {
                        if in_tab {
                            window.focus(&this.commit.push_focus);
                        }
                        if is_dir {
                            if in_tab {
                                this.sync.push_dir_cursor = Some(dir.clone());
                            }
                            this.toggle_push_dir(dir.clone(), cx);
                        } else if let (true, Some(i)) = (in_tab, file_idx) {
                            this.select_push_file(i, cx);
                        }
                    },
                    |_, _| {},
                    move |this, pos, cx| {
                        if let (false, Some(i)) = (in_tab, file_idx) {
                            this.open_menu(pos, MenuTarget::PushFile(i), cx);
                        }
                    },
                )
            })
            .collect()
    }

    /// The push tree's visible rows: the root (the repo) + every expanded folder's children.
    pub(crate) fn push_visible_rows(&self) -> Vec<tree::Row> {
        let sync = &self.sync;
        let mut visible = vec![tree::Row {
            path: PathBuf::new(),
            is_dir: true,
            depth: 0,
        }];
        if sync.push_expanded.contains(&PathBuf::new()) {
            for mut r in sync.push_tree.visible(&sync.push_expanded) {
                r.depth += 1;
                visible.push(r);
            }
        }
        visible
    }

    /// Expand / collapse a folder in the push tree.
    pub(crate) fn toggle_push_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.sync.push_expanded.remove(&path) {
            self.sync.push_expanded.insert(path);
        }
        cx.notify();
    }

    /// Open the push confirmation modal (lists the files that would be pushed, like the
    /// commit/rollback views). Push doesn't fire until the user confirms.
    pub(crate) fn open_push_modal(&mut self, cx: &mut Context<Self>) {
        self.context_menu = None;
        if let Some(repo) = self.repo() {
            self.sync.push_base = repo.push_base();
            self.sync.set_push_files(repo.push_files());
        } else {
            self.sync.set_push_files(Vec::new());
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
                self.commit.focus_msg = true;
                if self.files.is_empty() {
                    self.clear_diff_panes(cx);
                } else {
                    let i = self.selected.filter(|&i| i < self.files.len()).unwrap_or(0);
                    self.select_with(i, Some(cx));
                }
            }
            GitTab::Push => {
                if self.sync.push_files.is_empty() {
                    self.sync.push_selected = None;
                    self.clear_diff_panes(cx);
                } else {
                    let i = self
                        .sync
                        .push_selected
                        .filter(|&i| i < self.sync.push_files.len())
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
            GitTab::Commit if self.files.is_empty() && !self.sync.push_files.is_empty() => {
                GitTab::Push
            }
            GitTab::Push if self.sync.push_files.is_empty() && !self.files.is_empty() => {
                GitTab::Commit
            }
            _ => return,
        };
        self.set_git_tab(want, cx);
    }

    /// Select a file in the Push tab → load its committed change (`push_base` vs HEAD)
    /// read-only into the diff panes (no working-tree edit, so no revert chevrons/autosave).
    pub(crate) fn select_push_file(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.sync.push_selected = Some(idx);
        self.sync.push_dir_cursor = None;
        let Some(file) = self.sync.push_files.get(idx).cloned() else {
            return;
        };
        let Some(repo) = self.repo() else { return };
        let before = repo
            .committed_content(&self.sync.push_base, &file.path)
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
        let Some(file) = self.sync.push_files.get(idx).cloned() else {
            return;
        };
        let Some(repo) = self.repo() else { return };
        let before = repo
            .committed_content(&self.sync.push_base, &file.path)
            .unwrap_or_default();
        let after = repo
            .committed_content("HEAD", &file.path)
            .unwrap_or_default();
        let lang = self.effective_lang(&file.path);
        // `diff.path = Some` (set inside `load_diff_panes`) makes `render_diff` show the
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
        if self.sync.pushing {
            return;
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        self.sync.pushing = true;
        self.sync.push_msg = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { Repo::discover(&root).and_then(|r| r.push_rebasing()) })
                .await;
            this.update(cx, |this, cx| {
                this.sync.pushing = false;
                let err = result.err().map(|e| e.to_string());
                this.sync.push_msg = err.clone();
                // Stash before refresh: the async status read clears `op_error` on success,
                // so a direct set would be wiped (see `pending_error`).
                if let Some(m) = err {
                    this.pending_error = Some(format!("Push failed: {m}"));
                }
                this.refresh(cx);
                // Pushed → the Push tab may be empty now; flip to Commit if it has work.
                this.normalize_git_tab(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
