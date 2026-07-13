//! Rollback (discard changes) modal — checkbox tree of changes + delete-added toggle.
//! Crate-root child module.

use crate::*;

impl Kyde {
    /// Rollback Changes window body: checkbox tree of changes + Close/Rollback. Right-click a
    /// row → View Diff. Hosted in its own native window (`ModalWindow`).
    pub(crate) fn render_rollback_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let fs = px(theme::get().ui_font_size);
        let t = theme::get();
        let root_name = self
            .repo_root
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or_else(|| "/".to_string(), |s| s.to_string_lossy().into_owned());
        // Same folder tree as the Commit view, with rollback checkboxes.
        let mut visible = vec![tree::Row {
            path: PathBuf::new(),
            is_dir: true,
            depth: 0,
        }];
        if self.commit.expanded.contains(&PathBuf::new()) {
            for mut r in self.commit.tree.visible(&self.commit.expanded) {
                r.depth += 1;
                visible.push(r);
            }
        }
        let rows: Vec<gpui::AnyElement> = visible
            .into_iter()
            .map(|r| {
                let is_root = r.path.as_os_str().is_empty();
                let checked = if r.is_dir {
                    self.rollback_folder_all_checked(&r.path)
                } else {
                    self.rollback_checked.contains(&r.path)
                };
                let name_color = self
                    .files
                    .iter()
                    .find(|f| f.path == r.path)
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
                let expanded = self.commit.expanded.contains(&r.path);
                let is_dir = r.is_dir;
                let (p_act, p_check, p_ctx) = (r.path.clone(), r.path.clone(), r.path.clone());
                ui::tree::item(
                    cx,
                    self.dragging(Divider::Tree),
                    &r.path,
                    is_dir,
                    expanded,
                    r.depth,
                    false,
                    name,
                    name_color,
                    Some(checked),
                    // Click toggles the row's checkbox (folders expand); the diff is reached
                    // by right-click → View Diff, not a plain click.
                    move |this, _e, _w, cx| {
                        if is_dir {
                            this.toggle_commit_dir(p_act.clone(), cx);
                        } else {
                            this.toggle_rollback_check(p_act.clone(), false, cx);
                        }
                    },
                    move |this, cx| this.toggle_rollback_check(p_check.clone(), is_dir, cx),
                    move |this, pos, cx| {
                        if !is_dir {
                            if let Some(i) = this.files.iter().position(|f| f.path == p_ctx) {
                                this.open_menu(pos, MenuTarget::RollbackFile(i), cx);
                            }
                        }
                    },
                )
            })
            .collect();

        let count = self.files.len();

        let list = div()
            .id("rollback-list")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .flex_1()
            .p_2()
            .child(
                div()
                    .px_1()
                    .py_1()
                    .text_color(t.line_number)
                    .child(SharedString::from(format!("Changes  {count} files"))),
            )
            .children(rows);

        let delete_added = self.rollback_delete_added;
        let delete_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .text_color(t.text)
            .cursor_pointer()
            .child(checkbox_box(delete_added))
            .child("Delete local copies of added files")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.rollback_delete_added = !this.rollback_delete_added;
                    cx.notify();
                }),
            );

        let footer = ui::window_footer()
            .child(
                btn_secondary("rollback-close", "Close")
                    .py_1p5()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.close_rollback_window(cx)),
                    ),
            )
            .child(btn_primary("rollback", "Rollback").py_1p5().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.do_rollback(cx)),
            ));

        // Fills its native window (the OS titlebar shows "Rollback Changes" + close button).
        let mut panel = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.panel_bg)
            .font_family(ui)
            .text_size(fs)
            .text_color(t.text)
            .child(list)
            .child(delete_row)
            .child(footer);
        // Right-click → View Diff menu renders inside THIS window (its target is a RollbackFile).
        if matches!(
            self.context_menu.as_ref().map(|m| &m.target),
            Some(MenuTarget::RollbackFile(_))
        ) {
            panel = panel.child(self.render_context_menu(cx));
        }
        panel.into_any_element()
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
                        // A failed unstage means the file is NOT rolled back (still staged) —
                        // count it as a failure like any other, don't silently drop it.
                        repo.unstage(&f.path).and_then(|()| {
                            if delete_added {
                                repo.delete_file(&f.path)
                            } else {
                                Ok(())
                            }
                        })
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
        // Stash before refresh — the async status read clears `op_error` on success, so a
        // direct set would be wiped (see `pending_error`); `apply_snapshot` re-applies it.
        if !failures.is_empty() {
            self.pending_error = Some(format!(
                "Rollback failed for {} file(s): {}",
                failures.len(),
                failures.join(", ")
            ));
        }
        self.refresh(cx);
        self.exit_commit_if_clean();
        cx.notify();
    }
}
