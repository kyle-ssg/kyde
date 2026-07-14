//! Branch switcher + status bar (branch chip, ahead/behind, fetch/pull/push) + branch picker.
//! Crate-root child module.

use crate::*;

impl Kyde {
    /// Bottom status bar — currently just the branch switcher, anchored bottom-right.
    pub(crate) fn render_status_bar(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        // All bottom-bar text is this muted grey; icons keep their own colours.
        let bar_text = kyde_color::Color::rgb(0x808289);
        let label = self
            .current_branch
            .clone()
            .unwrap_or_else(|| "(no branch)".into());
        let chip = div()
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
                        .path("icons/git-branch.svg")
                        .size(px(15.0))
                        .text_color(bar_text),
                ),
            )
            .child(SharedString::from(label))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, window, cx| this.toggle_branch_popup(window, cx)),
            );

        // Breadcrumb of the open file: <repo> › dir › … › <badge> file. flex_1 + min_w_0
        // + overflow hidden lets it shrink and clip rather than push into the branch chip.
        let mut crumbs = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            // Line the first crumb up with the rail button icons (rail margin + icon inset).
            .pl(px(8.0));
        // Breadcrumb follows the tree selection (folder or file), falling back to the open
        // file. A folder selection shows a folder icon; a file shows its type badge.
        let crumb = self
            .browse
            .selected_path
            .as_ref()
            .or(self.browse.open_path.as_ref());
        if let Some(rel) = crumb.filter(|p| !p.as_os_str().is_empty()) {
            // Real filesystem check (not "is it the open file") — single-click selection
            // can point at any file or folder, so the icon must follow the path itself.
            let is_file = self
                .repo_root
                .as_ref()
                .is_some_and(|root| root.join(rel).is_file());
            let repo_name = self
                .repo_root
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let sep = || div().flex_none().text_color(bar_text).child("›");
            let folder_icon = || {
                div().flex_none().child(
                    svg()
                        .path("icons/folder.svg")
                        .size(px(16.0))
                        .text_color(t.line_number),
                )
            };
            crumbs = crumbs.child(
                div()
                    .flex_none()
                    .text_color(bar_text)
                    .child(SharedString::from(repo_name)),
            );
            let comps: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let last = comps.len().saturating_sub(1);
            for (i, name) in comps.into_iter().enumerate() {
                crumbs = crumbs.child(sep());
                // Icon: file-type badge for the open file's final segment; folder icon
                // for every directory segment (including a selected folder).
                let icon = if i == last && is_file {
                    div()
                        .flex_none()
                        .child(badge_inner(file_badge(rel), 2.0))
                        .into_any_element()
                } else {
                    folder_icon().into_any_element()
                };
                crumbs = crumbs.child(icon).child(
                    div()
                        .flex_none()
                        .text_color(bar_text)
                        .child(SharedString::from(name)),
                );
            }
        }

        // Push button: ↑ + "Push", with an ahead-of-upstream count badge. Tooltip
        // carries the last push error (or a hint). Disabled while a push is running.
        let pushing = self.sync.pushing;
        let ahead = self.sync.ahead.unwrap_or(0);
        let tip_text: SharedString = self
            .sync
            .push_msg
            .clone()
            .map_or_else(|| "Push to origin".into(), SharedString::from);
        let push_btn = div()
            .id("push-btn")
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .gap_1p5()
            .px_2()
            .rounded_md()
            .cursor_pointer()
            .hover(|s| s.bg(t.bg_light))
            .text_color(if self.sync.push_msg.is_some() {
                t.status_deleted
            } else {
                bar_text
            })
            .tooltip(move |_w, cx| cx.new(|_| Tip(tip_text.clone())).into())
            .child(div().flex_none().child(if pushing { "↻" } else { "↑" }))
            .child(SharedString::from(if pushing {
                "Pushing…"
            } else {
                "Push"
            }))
            .when(ahead > 0, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px_1p5()
                        .rounded_md()
                        .bg(t.bg_light)
                        .text_color(t.text)
                        .child(SharedString::from(ahead.to_string())),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.open_push_modal(cx)),
            );

        // Pull chip: ↓ + "Pull", with a behind-of-upstream count badge. Shown only when we
        // know we're behind (or a pull's in flight); the branch popup always offers Pull.
        let pulling = self.sync.pulling;
        let behind = self.sync.behind.unwrap_or(0);
        let pull_btn = div()
            .id("pull-btn")
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
            .tooltip(|_w, cx| cx.new(|_| Tip("Pull from origin (rebase)".into())).into())
            .child(div().flex_none().child(if pulling { "↻" } else { "↓" }))
            .child(SharedString::from(if pulling {
                "Pulling…"
            } else {
                "Pull"
            }))
            .when(behind > 0, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px_1p5()
                        .rounded_md()
                        .bg(t.bg_light)
                        .text_color(t.text)
                        .child(SharedString::from(behind.to_string())),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.do_pull(cx)),
            );

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_2()
            .h(px(28.0))
            .mb(px(6.0))
            .px_2()
            // Joins the surrounding chrome: same chrome colour, no separating border.
            .bg(t.frame_bg)
            .font_family(ui)
            // Same size as the file-tree rows.
            .text_size(px(theme::get().ui_font_size + 3.0))
            .child(crumbs)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_none()
                    .gap_2()
                    // Only show Pull when we know we're behind (or one's in flight).
                    .when(
                        self.sync.behind.unwrap_or(0) > 0 || self.sync.pulling,
                        |d| d.child(pull_btn),
                    )
                    // Only show Push when there's actually something to push (or one's in flight).
                    .when(self.sync.ahead.unwrap_or(0) > 0 || self.sync.pushing, |d| {
                        d.child(push_btn)
                    })
                    // Worktree chip only when the repo has linked worktrees (list = main +
                    // linked, so > 1) — zero chrome otherwise.
                    .when(self.worktree.list.len() > 1, |d| {
                        d.child(self.render_worktree_chip(cx))
                    })
                    .child(chip),
            )
            .into_any_element()
    }

    /// Branch switcher popup: search box, New Branch, Recent, then All Branches.
    /// Anchored bottom-right above the status bar; transparent backdrop closes it.
    pub(crate) fn render_branch_popup(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let q = self.branch.query.read(cx).text().to_string();
        let ql = q.trim().to_lowercase();
        let current = self.current_branch.clone();
        let matches = |b: &str| ql.is_empty() || b.to_lowercase().contains(&ql);

        let recent: Vec<String> = self
            .branch
            .list
            .iter()
            .filter(|b| current.as_deref() != Some(b.as_str()))
            .filter(|b| matches(b))
            .take(5)
            .cloned()
            .collect();
        let mut all: Vec<String> = self
            .branch
            .list
            .iter()
            .filter(|b| matches(b))
            .cloned()
            .collect();
        all.sort_by_key(|b| b.to_lowercase());
        let remotes: Vec<String> = self
            .branch
            .remotes
            .iter()
            .filter(|b| matches(b))
            .cloned()
            .collect();

        let nb_label = if ql.is_empty() {
            "+ New Branch".to_string()
        } else {
            format!("+ New Branch  “{}”", q.trim())
        };
        // Popup separators: the theme `divider` (#26272B) is invisible on the popup's
        // `bg_mid` (#26282B), so use a faint white hairline that actually reads.
        let sep = gpui::rgba(0xFFFFFF1A);
        let new_row = div()
            .mx_1()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .hover(|s| s.bg(t.selected_bg))
            .text_color(t.primary)
            .child(SharedString::from(nb_label))
            // Opens the "Create New Branch" dialog (the search text prefills the name).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| this.open_new_branch(cx)),
            );

        // Pull (fetch + rebase). Always available here — it's the repo-ops hub, and a pull
        // fetches first, so it works even when our last-known `behind` count is stale/0.
        let behind = self.sync.behind.unwrap_or(0);
        let pull_label = if self.sync.pulling {
            "↓ Pulling…".to_string()
        } else if behind > 0 {
            format!("↓ Pull  ({behind})")
        } else {
            "↓ Pull".to_string()
        };
        let pull_row = div()
            .mx_1()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .hover(|s| s.bg(t.selected_bg))
            .text_color(t.text)
            .child(SharedString::from(pull_label))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| this.do_pull(cx)),
            );

        let search = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(sep)
            .child(
                svg()
                    .path("icons/search.svg")
                    .size(px(15.0))
                    .flex_none()
                    .text_color(t.line_number),
            )
            .child(div().flex_1().min_w_0().child(self.branch.query.clone()));

        // Branch tree: Recent + Local sections as expandable roots; `/` → folders.
        // While searching, force everything open so matches are visible.
        let rows = branch_rows(
            &recent,
            &all,
            &remotes,
            &self.branch.expanded,
            !ql.is_empty(),
        );
        let tree_rows = self.branch_tree(rows, cx);
        let list = div()
            .id("branch-list")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .py_1()
            .child(new_row)
            .child(pull_row)
            // hairline between the actions and the branch tree
            .child(div().mx_2().my_1().h(px(1.0)).bg(sep))
            .children(tree_rows);

        let panel = div()
            .absolute()
            .right(px(8.0))
            .bottom(px(28.0))
            .w(px(340.0))
            .max_h(px(460.0))
            .flex()
            .flex_col()
            .bg(t.bg_mid)
            .border_1()
            .border_color(gpui::rgb(0x595D60))
            .rounded_md()
            .shadow_lg()
            .occlude()
            .font_family(ui)
            .text_size(fs)
            .child(search)
            .child(list);

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, window, cx| {
                    this.branch.popup_open = false;
                    window.focus(&this.focus_handle);
                    cx.notify();
                }),
            )
            .child(panel)
            .into_any_element()
    }

    /// Render the branch tree (sections as roots, `/` segments as folders).
    fn branch_tree(&self, rows: Vec<BranchRow>, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let t = theme::get();
        let current = self.current_branch.clone();
        // In a linked worktree, plain checkouts are disabled (the worktree is pinned to
        // its branch — that's the point of worktrees); rows that jump to another worktree
        // and the current branch stay active.
        let pinned = self.in_linked_worktree();
        rows.into_iter()
            .map(|r| {
                let indent = px(8.0 + r.depth as f32 * 14.0);
                match r.node {
                    BranchNode::Folder {
                        key,
                        expanded,
                        section,
                    } => {
                        let k = key.clone();
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .h(px(28.0))
                            .pl(indent)
                            .pr_2()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|s| s.bg(t.bg_light))
                            .text_color(t.text)
                            .child(
                                div()
                                    .w(px(12.0))
                                    .flex_none()
                                    .text_color(t.line_number)
                                    .child(if expanded { "▾" } else { "▸" }),
                            )
                            .when(!section, |d| {
                                d.child(
                                    svg()
                                        .path("icons/folder.svg")
                                        .size(px(14.0))
                                        .text_color(t.line_number),
                                )
                            })
                            .child(SharedString::from(r.label))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    this.toggle_branch_node(k.clone(), cx);
                                }),
                            )
                            .into_any_element()
                    }
                    BranchNode::Leaf { full } => {
                        let is_current = current.as_deref() == Some(full.as_str());
                        // Checked out in another worktree → clicking jumps there (see
                        // `checkout_branch`); mark the row with its worktree's dir name.
                        let elsewhere = self
                            .other_worktree_for_branch(&full)
                            .and_then(|w| w.path.file_name())
                            .map(|s| s.to_string_lossy().into_owned());
                        let nm = full.clone();
                        // Disabled = a plain checkout in a pinned (linked) worktree; jump
                        // rows (`elsewhere`) and the current branch stay interactive.
                        let disabled = pinned && elsewhere.is_none() && !is_current;
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .h(px(28.0))
                            .pl(indent)
                            .pr_2()
                            .rounded_md()
                            .when(!disabled, |d| {
                                d.cursor_pointer().hover(|s| s.bg(t.selected_bg))
                            })
                            .text_color(if disabled { t.line_number } else { t.text })
                            .child(div().flex_none().text_color(t.line_number).child("⎇"))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(r.label)),
                            )
                            .when_some(elsewhere, |d, wt| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_1()
                                        .flex_none()
                                        .text_color(t.line_number)
                                        .child(
                                            svg()
                                                .path("icons/layers.svg")
                                                .size(px(13.0))
                                                .text_color(t.line_number),
                                        )
                                        .child(SharedString::from(wt)),
                                )
                            })
                            .when(is_current, |d| {
                                d.child(div().flex_none().text_color(t.line_number).child("✓"))
                            })
                            .when(!disabled, |d| {
                                d.on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _e, window, cx| {
                                        this.checkout_branch(nm.clone(), window, cx);
                                    }),
                                )
                            })
                            .into_any_element()
                    }
                }
            })
            .collect()
    }

    // ── branch switcher ───────────────────────────────────────────
    pub(crate) fn toggle_branch_popup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.branch.popup_open {
            self.branch.popup_open = false;
            window.focus(&self.focus_handle);
        } else {
            self.worktree.popup_open = false;
            self.branch.list = self
                .repo()
                .and_then(|r| r.branches().ok())
                .unwrap_or_default();
            // Remote-tracking branches with no local head yet (e.g. just fetched). Keyed by
            // short name (drop the remote prefix) so checkout DWIMs a local tracking branch;
            // skip any that already exist locally and dedupe across remotes (recency order).
            let local: std::collections::HashSet<&str> =
                self.branch.list.iter().map(String::as_str).collect();
            let mut seen = std::collections::HashSet::new();
            self.branch.remotes = self
                .repo()
                .and_then(|r| r.remote_branches().ok())
                .unwrap_or_default()
                .iter()
                .filter_map(|r| r.split_once('/').map(|(_, s)| s.to_string()))
                .filter(|s| !local.contains(s.as_str()) && seen.insert(s.clone()))
                .collect();
            self.branch.query.update(cx, |e, cx| {
                e.set_content(String::new(), Lang::PlainText, cx);
            });
            // Recent expanded by default; Local collapsed.
            self.branch.expanded.insert("sec:recent".into());
            self.branch.popup_open = true;
            // Focus now and next frame: the popup element isn't in the tree on first open.
            let handle = self.branch.query.read(cx).focus_handle.clone();
            window.focus(&handle);
            window.defer(cx, move |window, _cx| window.focus(&handle));
        }
        cx.notify();
    }

    /// Pull = fetch + rebase local commits on top (auto-stashing edits), off the UI thread.
    /// Mirrors `do_push`. Closes the branch popup so the UI never freezes mid-operation.
    pub(crate) fn do_pull(&mut self, cx: &mut Context<Self>) {
        self.branch.popup_open = false;
        self.context_menu = None;
        if self.sync.pulling {
            return;
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        self.sync.pulling = true;
        self.sync.push_msg = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { Repo::discover(&root).and_then(|r| r.pull_rebase()) })
                .await;
            this.update(cx, |this, cx| {
                this.sync.pulling = false;
                let err = result.err().map(|e| e.to_string());
                this.sync.push_msg = err.clone();
                // Stash before refresh: the async status read clears `op_error` on success,
                // so a direct set would be wiped (see `pending_error`).
                if let Some(m) = err {
                    this.pending_error = Some(format!("Pull failed: {m}"));
                }
                this.refresh(cx);
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
        self.branch.popup_open = false;
        if self.sync.fetching {
            return;
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        self.sync.fetching = true;
        self.sync.push_msg = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { Repo::discover(&root).and_then(|r| r.fetch()) })
                .await;
            this.update(cx, |this, cx| {
                this.sync.fetching = false;
                let err = result.err().map(|e| e.to_string());
                if let Some(m) = err {
                    this.pending_error = Some(format!("Fetch failed: {m}"));
                }
                // refresh() recomputes ahead/behind from the freshly-fetched refs.
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn toggle_branch_node(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.branch.expanded.remove(&key) {
            self.branch.expanded.insert(key);
        }
        cx.notify();
    }

    pub(crate) fn checkout_branch(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A branch checked out in another worktree can't be checked out here (git fatals
        // with "already checked out at …") — jump to that worktree instead.
        if let Some(path) = self
            .other_worktree_for_branch(&name)
            .map(|w| w.path.clone())
        {
            self.branch.popup_open = false;
            window.focus(&self.focus_handle);
            self.open_project(path, cx);
            return;
        }
        // Linked worktrees are pinned to their branch — the popup disables these rows, but
        // guard here too so no other path can checkout inside one.
        if self.in_linked_worktree() {
            return;
        }
        self.run_branch_op(window, cx, move |r| r.checkout(&name));
    }

    /// Open the "Create New Branch" dialog (own native window). `branch_query` doubles as the
    /// name field (prefilled with whatever was typed in the branch popup).
    pub(crate) fn open_new_branch(&mut self, cx: &mut Context<Self>) {
        self.branch.popup_open = false;
        self.new_branch_checkout = true;
        self.new_branch_overwrite = false;
        self.open_modal_window(ModalKind::NewBranch, "Create New Branch", 520.0, 220.0, cx);
        cx.notify();
    }

    /// Create the branch named in the dialog, honoring the Checkout / Overwrite toggles, then
    /// close the dialog and refresh. Spaces in the name become hyphens (git rejects spaces).
    pub(crate) fn do_create_branch(&mut self, cx: &mut Context<Self>) {
        let name = slugify_branch(self.branch.query.read(cx).text());
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
                if let Err(e) = res {
                    this.fail_pending("Create branch", e);
                }
                this.refresh(cx);
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
        op: impl FnOnce(&Repo) -> kyde_git::Result<()> + Send + 'static,
    ) {
        self.branch.popup_open = false;
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
                if let Err(e) = res {
                    this.fail_pending("Branch operation", e);
                }
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Debounced git-status refresh: only the latest edit's timer wins, so status
    /// catches up ~0.4s after you stop typing instead of on every keystroke.
    pub(crate) fn schedule_status_refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_gen = self.refresh_gen.wrapping_add(1);
        let generation = self.refresh_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(STATUS_REFRESH_DEBOUNCE)
                .await;
            this.update(cx, |this, cx| {
                if this.refresh_gen == generation {
                    this.refresh(cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}
