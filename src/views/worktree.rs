//! Worktree switcher: a status-bar chip (next to the branch chip) + popup listing every
//! worktree — dir name, checked-out branch, changed-files count badge — so parallel agent
//! worktrees double as a review queue. Clicking a row switches to that worktree via
//! `open_project` (each worktree is its own toplevel, so discover/session-restore just
//! work). Crate-root child module.

use crate::*;

impl Kyde {
    /// The worktree checked out on `branch`, when it is NOT the active project — the case
    /// where a plain `git checkout` would fail with "already checked out at …" and we jump
    /// to that worktree instead. `None` = normal checkout applies.
    pub(crate) fn other_worktree_for_branch(&self, branch: &str) -> Option<&git::Worktree> {
        let root = self.repo_root.as_deref()?;
        self.worktree
            .list
            .iter()
            .filter(|w| !same_dir(&w.path, root))
            .find(|w| w.branch.as_deref() == Some(branch))
    }

    /// Toggle the worktree popup. On open, kick off one background `git status` per
    /// worktree to fill the changed-count badges (never on the render path).
    pub(crate) fn toggle_worktree_popup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.worktree.popup_open {
            self.worktree.popup_open = false;
            window.focus(&self.focus_handle);
            cx.notify();
            return;
        }
        self.branch.popup_open = false;
        self.worktree.popup_open = true;
        self.worktree.counts.clear();
        self.worktree.counts_gen = self.worktree.counts_gen.wrapping_add(1);
        let generation = self.worktree.counts_gen;
        let paths: Vec<PathBuf> = self.worktree.list.iter().map(|w| w.path.clone()).collect();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let counts = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .map(|p| {
                            let n = Repo::discover(&p)
                                .and_then(|r| r.status())
                                .map_or(0, |files| files.len());
                            (p, n)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                // Only the newest open's gather wins (a re-open supersedes an in-flight one).
                if this.worktree.counts_gen == generation {
                    this.worktree.counts.extend(counts);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Switch the app to the worktree at `path` (no-op for the active one). Reuses
    /// `open_project`, so per-project session save/restore preserves open file/tabs/state.
    pub(crate) fn switch_worktree(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.worktree.popup_open = false;
        if self
            .repo_root
            .as_deref()
            .is_some_and(|r| same_dir(r, &path))
        {
            cx.notify();
            return;
        }
        self.open_project(path, cx);
    }

    /// Worktree chip for the status bar: layers icon + the active worktree's dir name.
    /// Only called when the repo has linked worktrees (the caller hides it otherwise).
    pub(crate) fn render_worktree_chip(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        // Same muted grey as the rest of the bottom bar (see `render_status_bar`).
        let bar_text = kyde_color::Color::rgb(0x808289);
        let label = self
            .repo_root
            .as_deref()
            .and_then(dir_name)
            .unwrap_or_else(|| "(worktree)".into());
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .gap_1p5()
            .px_2()
            .rounded_md()
            .cursor_pointer()
            .hover(|s| s.bg(t.bg_light))
            .text_color(bar_text)
            .child(
                div().flex_none().child(
                    svg()
                        .path("icons/layers.svg")
                        .size(px(15.0))
                        .text_color(bar_text),
                ),
            )
            .child(SharedString::from(label))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, window, cx| this.toggle_worktree_popup(window, cx)),
            )
            .into_any_element()
    }

    /// Worktree popup: one row per worktree (dir name, branch, changed-count badge), the
    /// active one marked ✓. Anchored bottom-right above the status bar like the branch
    /// popup; transparent backdrop closes it.
    pub(crate) fn render_worktree_popup(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let root = self.repo_root.clone();
        let rows: Vec<gpui::AnyElement> = self
            .worktree
            .list
            .iter()
            .map(|w| {
                let is_current = root.as_deref().is_some_and(|r| same_dir(r, &w.path));
                let name = dir_name(&w.path).unwrap_or_else(|| w.path.display().to_string());
                let branch = w
                    .branch
                    .clone()
                    .unwrap_or_else(|| format!("({})", &w.head[..w.head.len().min(7)]));
                let changed = self.worktree.counts.get(&w.path).copied();
                let path = w.path.clone();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(px(30.0))
                    .mx_1()
                    .px_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.selected_bg))
                    .text_color(t.text)
                    .child(
                        div().flex_none().child(
                            svg()
                                .path("icons/layers.svg")
                                .size(px(14.0))
                                .text_color(t.line_number),
                        ),
                    )
                    .child(div().flex_none().child(SharedString::from(name)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(t.line_number)
                            .child(SharedString::from(format!("⎇ {branch}"))),
                    )
                    // Changed-files badge: the "which agent has work waiting?" signal.
                    // Absent while the background status gather is still running.
                    .when_some(changed.filter(|n| *n > 0), |d, n| {
                        d.child(
                            div()
                                .flex_none()
                                .px_1p5()
                                .rounded_md()
                                .bg(t.bg_light)
                                .text_color(t.text)
                                .child(SharedString::from(n.to_string())),
                        )
                    })
                    .when(is_current, |d| {
                        d.child(div().flex_none().text_color(t.line_number).child("✓"))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.switch_worktree(path.clone(), cx);
                        }),
                    )
                    .into_any_element()
            })
            .collect();

        let panel = div()
            .absolute()
            .right(px(8.0))
            .bottom(px(28.0))
            .w(px(340.0))
            .max_h(px(460.0))
            .flex()
            .flex_col()
            .py_1()
            .bg(t.bg_mid)
            .border_1()
            .border_color(gpui::rgb(0x595D60))
            .rounded_md()
            .shadow_lg()
            .occlude()
            .font_family(ui)
            .text_size(fs)
            .child(
                div()
                    .id("worktree-list")
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(rows),
            );

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, window, cx| {
                    this.worktree.popup_open = false;
                    window.focus(&this.focus_handle);
                    cx.notify();
                }),
            )
            .child(panel)
            .into_any_element()
    }
}

/// The last path component as an owned string (a worktree's display name).
fn dir_name(p: &std::path::Path) -> Option<String> {
    p.file_name().map(|s| s.to_string_lossy().into_owned())
}

/// Path equality for worktree roots, robust to symlink aliases (`/tmp` vs `/private/tmp`
/// on macOS): canonicalize both when possible, fall back to the raw paths.
fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}
