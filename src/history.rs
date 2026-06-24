//! History view (git log) — the third rail mode. Commit list + changed-files tree + a
//! read-only side-by-side diff, with branch picker, path scope, and compare modes. A
//! crate-root child module; its `impl Kyde` block reaches Kyde's private fields directly.

use super::*;

impl Kyde {
    /// History (git log) view: a commit list (left), the selected commit's changed files
    /// (middle), and a read-only side-by-side diff (right). A branch dropdown picks which
    /// ref to log; a segmented control picks what the commit is diffed against.
    pub(crate) fn render_history(
        &mut self,
        ui: &'static str,
        fs: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();

        // ── header: branch chip + commit search + compare-mode segmented control ──
        let rev_label: SharedString = format!("⎇ {}", self.history_rev).into();
        let branch_chip = div()
            .id("hist-branch")
            .flex()
            .items_center()
            .gap_1()
            .h(px(28.0))
            .px_2()
            .rounded_md()
            .border_1()
            .border_color(t.divider)
            .text_color(t.secondary_text)
            .cursor_pointer()
            .hover(|d| d.bg(t.bg_mid))
            .child(rev_label)
            // Chevron → reads as a select.
            .child(
                svg()
                    .path("icons/chevron-down.svg")
                    .size(px(14.0))
                    .text_color(t.line_number),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, window, cx| {
                    this.toggle_history_branches(cx);
                    if this.history_branch_open {
                        // Focus now and next frame: the dropdown isn't in the tree on first open.
                        let handle = this.history_branch_query.read(cx).focus_handle.clone();
                        window.focus(&handle);
                        window.defer(cx, move |window, _cx| window.focus(&handle));
                    }
                }),
            );

        // Compare-mode dropdown trigger (replaces the old segmented control). The menu itself
        // is rendered near the branch dropdown below (anchored above the panel).
        let cmp_label: SharedString = self.history_compare.label().into();
        let compare_chip = div()
            .id("hist-compare")
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .h(px(28.0))
            .px_2()
            .rounded_md()
            .border_1()
            .border_color(t.divider)
            .text_color(t.secondary_text)
            .text_size(px(t.ui_font_size))
            .cursor_pointer()
            .hover(|d| d.bg(t.bg_mid))
            .child(cmp_label)
            .child(
                svg()
                    .path("icons/chevron-down.svg")
                    .size(px(14.0))
                    .text_color(t.line_number),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.history_compare_open = !this.history_compare_open;
                    cx.notify();
                }),
            );
        // Scope chip: shown when the log is restricted to a folder/file; click clears it.
        let scope_chip = self.history_path.as_ref().map(|p| {
            let label: SharedString = format!("▸ {}", p.to_string_lossy()).into();
            div()
                .id("hist-scope")
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(t.bg_mid)
                .text_color(t.secondary_text)
                .cursor_pointer()
                .hover(|d| d.bg(t.bg_light))
                .tooltip(|_w, cx| cx.new(|_| Tip("Clear path filter".into())).into())
                .child(label)
                .child(div().text_color(t.line_number).child("×"))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.enter_history(cx)),
                )
        });
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .flex_none()
            .px_1()
            // Equal top/bottom gap to the divider below.
            .pt(px(theme::FRAME_GAP))
            .pb(px(theme::FRAME_GAP))
            .font_family(ui)
            .child(branch_chip)
            .children(scope_chip)
            // Commit search box, immediately right of the branch dropdown.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    // Right margin matches the branch chip's left inset (header px_1).
                    .mr_1()
                    .px_2()
                    // py_1 + 18px editor line + 2px border = 28px, matching the chip's h(28).
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(t.divider)
                    .text_size(px(t.ui_font_size))
                    .child(self.history_commit_query.clone()),
            )
            .child(compare_chip)
            // Minimise / expand the bottom panel (IDE tool-window style).
            .child({
                let collapsed = self.history_panel_collapsed;
                div()
                    .id("hist-panel-toggle")
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(28.0))
                    .rounded_md()
                    .text_size(px(16.0))
                    .cursor_pointer()
                    .text_color(t.line_number)
                    .hover(|d| d.bg(t.bg_mid).text_color(t.text))
                    .tooltip(move |_w, cx| {
                        let tip = if collapsed {
                            "Expand panel"
                        } else {
                            "Minimise panel"
                        };
                        cx.new(|_| Tip(tip.into())).into()
                    })
                    // `−` to minimise (same as the Browse tree's collapse button); to expand,
                    // the tree's `»` double-chevron rotated to point up (this panel grows
                    // upward) = Lucide `chevrons-up`.
                    .child(if collapsed {
                        // svg() does NOT inherit the parent div's text color — set it here, or
                        // the icon draws with no stroke (invisible).
                        svg()
                            .path("icons/chevrons-up.svg")
                            .size(px(16.0))
                            .text_color(t.line_number)
                            .into_any_element()
                    } else {
                        SharedString::from("−").into_any_element()
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.history_panel_collapsed = !this.history_panel_collapsed;
                            cx.notify();
                        }),
                    )
            });

        // ── commit list (filtered by the commit search box) ──
        let cq = self
            .history_commit_query
            .read(cx)
            .text()
            .trim()
            .to_lowercase();
        let commit_rows: Vec<gpui::AnyElement> = self
            .history_commits
            .iter()
            .enumerate()
            // Keep the original index (drives selection); match subject / author / hash.
            .filter(|(_, c)| {
                cq.is_empty()
                    || c.subject.to_lowercase().contains(&cq)
                    || c.author.to_lowercase().contains(&cq)
                    || c.short.to_lowercase().contains(&cq)
            })
            .map(|(i, c)| {
                let selected = self.history_selected == Some(i);
                let subject: SharedString = c.subject.clone().into();
                let meta: SharedString = format!("{} · {} · {}", c.short, c.author, c.date).into();
                let refs = c.refs.clone();
                div()
                    .id(("hist-commit", i))
                    .flex()
                    .flex_col()
                    .gap(px(1.0))
                    .mx_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.selected_bg))
                    .when(!selected, |d| d.hover(|d| d.bg(t.bg_mid)))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(t.text)
                                    .child(subject),
                            )
                            .when(!refs.is_empty(), |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .px_1()
                                        .rounded_sm()
                                        .bg(t.bg_mid)
                                        .text_size(px(10.0))
                                        .text_color(t.primary)
                                        .child(SharedString::from(refs.clone())),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(t.line_number)
                            .truncate()
                            .child(meta),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.select_history_commit(i, cx)),
                    )
                    // Right-click → the same compare options as the header dropdown.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                            this.open_menu(e.position, MenuTarget::HistoryCompare(i), cx)
                        }),
                    )
                    .into_any_element()
            })
            .collect();
        // Scrollable commit list, wrapped in a flex-basis sizer (the basis must live on a
        // plain wrapper, NOT on the scroll element itself — that's why it collapsed before;
        // mirrors how render_diff sizes its two panes). Default 2/3 of the panel width.
        let commit_pane = div()
            .id("hist-commits")
            .overflow_y_scroll()
            .track_scroll(&self.history_scroll)
            .size_full()
            .flex()
            .flex_col()
            .py_1()
            .font_family(ui)
            .text_size(px(t.ui_font_size + 1.0))
            .children(commit_rows);
        // Commit list = a resizable share of the panel width on the LEFT (defaults to 2/3); the
        // files pane fills the rest (flex_1). Sized in EXPLICIT pixels from the same formula the
        // divider drag inverts (`Divider::HistCommit`), so the bar tracks the cursor exactly 1:1.
        let commit_frac = self.history_commit_frac.clamp(0.15, 0.85);
        let commit_w = commit_frac * full_island_w(f32::from(window.viewport_size().width));
        let commit_wrap = div()
            .w(px(commit_w))
            .flex_none()
            .min_w_0()
            .h_full()
            .child(commit_pane);

        // ── changed files of the selected commit, as a folder tree + a search box ──
        let fq = self
            .history_files_query
            .read(cx)
            .text()
            .trim()
            .to_lowercase();
        let mut visible = vec![tree::Row {
            path: PathBuf::new(),
            is_dir: true,
            depth: 0,
        }];
        if self.history_files_expanded.contains(&PathBuf::new()) {
            for mut r in self
                .history_files_tree
                .visible(&self.history_files_expanded)
            {
                r.depth += 1;
                visible.push(r);
            }
        }
        // Filter by the search box: keep root, matching files, and dirs containing a match.
        if !fq.is_empty() {
            let files = &self.history_files;
            visible.retain(|r| {
                r.path.as_os_str().is_empty()
                    || (!r.is_dir && r.path.to_string_lossy().to_lowercase().contains(&fq))
                    || (r.is_dir
                        && files.iter().any(|f| {
                            f.path.starts_with(&r.path)
                                && f.path.to_string_lossy().to_lowercase().contains(&fq)
                        }))
            });
        }
        // Root row shows the project name (like the Browse tree), not a generic "Files".
        let root_name: SharedString = self
            .repo_root
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string())
            .into();
        let file_rows: Vec<gpui::AnyElement> = visible
            .into_iter()
            .map(|r| {
                let is_root = r.path.as_os_str().is_empty();
                let file_idx = (!r.is_dir)
                    .then(|| self.history_files.iter().position(|f| f.path == r.path))
                    .flatten();
                let selected = file_idx.is_some() && self.history_file_selected == file_idx;
                let name_color = file_idx
                    .and_then(|i| self.history_files.get(i))
                    .map(|f| status_color(f.status))
                    .unwrap_or(t.text);
                let name: SharedString = if is_root {
                    root_name.clone()
                } else {
                    r.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into()
                };
                let expanded = self.history_files_expanded.contains(&r.path);
                let is_dir = r.is_dir;
                let p_act = r.path.clone();
                self.tree_row(
                    cx,
                    &r.path,
                    is_dir,
                    expanded,
                    r.depth,
                    selected,
                    name,
                    name_color,
                    None,
                    move |this, _e, _w, cx| {
                        if is_dir {
                            if !this.history_files_expanded.remove(&p_act) {
                                this.history_files_expanded.insert(p_act.clone());
                            }
                            cx.notify();
                        } else if let Some(i) =
                            this.history_files.iter().position(|f| f.path == p_act)
                        {
                            this.select_history_file(i, cx);
                        }
                    },
                    |_this, _cx| {},
                    |_this, _pos, _cx| {},
                )
            })
            .collect();
        let files_search = div()
            .flex_none()
            .px_2()
            .py_1p5()
            .text_size(px(t.ui_font_size))
            .child(self.history_files_query.clone());
        let files_hr = div().flex_none().h(px(1.0)).mx_1().bg(t.divider);
        // No changed files (e.g. "Compare with Local" when the working tree matches the
        // commit) → an explicit empty-state instead of a lone, childless root folder row.
        let files_body = if self.history_files.is_empty() {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .text_color(t.line_number)
                .child("No changes")
                .into_any_element()
        } else {
            div()
                .id("hist-files")
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .px_1()
                .py_1()
                .children(file_rows)
                .into_any_element()
        };
        let files_wrap = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .font_family(ui)
            .text_size(px(t.ui_font_size + 1.0))
            .child(files_search)
            .child(files_hr)
            .child(files_body);

        // Flat 1px dividers between sections (IntelliJ-style), not frame-gap islands.
        let hdiv = || div().h(px(3.0)).flex_none().bg(t.bg_light);
        // Draggable commit/files divider. A touch wider than 1px so it's easy to grab.
        let split_divider = div()
            .id("hist-split")
            .w(px(5.0))
            .flex_none()
            .h_full()
            .cursor_col_resize()
            .child(div().w(px(1.0)).h_full().mx(px(2.0)).bg(t.divider))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                    this.start_divider_drag(Divider::HistCommit, e.position, window);
                    cx.notify();
                }),
            );

        // Bottom panel (one island): toolbar, then commit list | files, divider-split.
        // Height is drag-resizable via the strip above it (`history_panel_h`). The header
        // chevron minimises it to just the toolbar (`history_panel_collapsed`).
        let panel_h = self.history_panel_h;
        let collapsed = self.history_panel_collapsed;
        // Toolbar height: pt + 28px chip row + pb. Used to anchor the dropdowns + as the
        // panel height when minimised.
        let header_h = theme::FRAME_GAP * 2.0 + 28.0;
        let panel_visible_h = if collapsed { header_h } else { panel_h };
        let mut bottom = div()
            .flex()
            .flex_col()
            .flex_none()
            .bg(t.main_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .overflow_hidden()
            .child(header);
        if !collapsed {
            bottom = bottom.h(px(panel_h)).child(hdiv()).child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .child(commit_wrap)
                    .child(split_divider)
                    .child(files_wrap),
            );
        }

        // Drag strip between the diff (top) and the log panel (bottom) — resizes the panel.
        let v_divider = div()
            .id("hist-vsplit")
            .h(px(theme::FRAME_GAP))
            .flex_none()
            .w_full()
            .cursor_row_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                    this.start_divider_drag(Divider::HistPanel, e.position, window);
                    cx.notify();
                }),
            );

        // Top = diff (main), divider, bottom = the log panel. The resize strip is dropped
        // when the panel is minimised (nothing to resize).
        let mut root = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(self.render_diff(ui, fs, Some(window), cx));
        if !collapsed {
            root = root.child(v_divider);
        }
        root = root.child(bottom);

        // Branch dropdown: a search box over Local / Remote sections, anchored ABOVE the
        // bottom-panel toolbar (it grows up over the diff so it isn't clipped by the panel).
        // A transparent backdrop closes it.
        if self.history_branch_open {
            let q = self
                .history_branch_query
                .read(cx)
                .text()
                .trim()
                .to_lowercase();
            let matches = |n: &str| q.is_empty() || n.to_lowercase().contains(&q);
            let cur = self.history_rev.clone();
            let locals: Vec<String> = self
                .history_locals
                .iter()
                .filter(|n| matches(n))
                .cloned()
                .collect();
            let remotes: Vec<String> = self
                .history_remotes
                .iter()
                .filter(|n| matches(n))
                .cloned()
                .collect();
            // One clickable branch row → re-log that ref.
            let mk = |b: String, cx: &mut Context<Self>| {
                let selected = b == cur;
                let label: SharedString = b.clone().into();
                div()
                    .id(SharedString::from(format!("hist-rev-{b}")))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(t.text)
                    .when(selected, |d| d.bg(t.selected_bg))
                    .when(!selected, |d| d.hover(|d| d.bg(t.bg_mid)))
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.set_history_rev(b.clone(), cx)),
                    )
                    .into_any_element()
            };
            let section = |label: &'static str| {
                div()
                    .px_2()
                    .pt_1()
                    .text_size(px(10.0))
                    .text_color(t.line_number)
                    .child(label)
            };
            let mut menu = div()
                .id("hist-rev-list")
                .absolute()
                // Anchor just above the bottom panel's toolbar; grows upward over the diff.
                .bottom(px(panel_visible_h + 2.0))
                .left(px(2.0))
                .flex()
                .flex_col()
                .max_h(px(380.0))
                .overflow_y_scroll()
                .min_w(px(260.0))
                .p_1()
                .bg(t.panel_bg)
                .border_1()
                .border_color(t.divider)
                .rounded_md()
                .font_family(ui)
                .text_size(px(t.ui_font_size))
                // Clicks inside the menu must not fall through to the backdrop (which closes).
                .on_mouse_down(MouseButton::Left, |_e, _w, cx: &mut App| {
                    cx.stop_propagation()
                })
                .child(div().px_1().pb_1().child(self.history_branch_query.clone()));
            if matches("HEAD") {
                menu = menu.child(mk("HEAD".to_string(), cx));
            }
            if !locals.is_empty() {
                menu = menu.child(section("LOCAL"));
                for b in locals {
                    menu = menu.child(mk(b, cx));
                }
            }
            if !remotes.is_empty() {
                menu = menu.child(section("REMOTE"));
                for b in remotes {
                    menu = menu.child(mk(b, cx));
                }
            }
            root = root.child(
                div()
                    .absolute()
                    .top(px(0.0))
                    .left(px(0.0))
                    .size_full()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.history_branch_open = false;
                            cx.notify();
                        }),
                    )
                    .child(menu),
            );
        }

        // Compare-mode dropdown: anchored above the panel toolbar on the RIGHT (the chip is
        // top-right). Each row = label + one-line description so the taxonomy is self-explaining.
        if self.history_compare_open {
            let cur = self.history_compare;
            let mut menu = div()
                .id("hist-compare-list")
                .absolute()
                .bottom(px(panel_visible_h + 2.0))
                .right(px(2.0))
                .flex()
                .flex_col()
                .min_w(px(300.0))
                .p_1()
                .bg(t.panel_bg)
                .border_1()
                .border_color(t.divider)
                .rounded_md()
                .font_family(ui)
                .text_size(px(t.ui_font_size))
                .on_mouse_down(MouseButton::Left, |_e, _w, cx: &mut App| {
                    cx.stop_propagation()
                });
            for mode in CompareMode::ALL {
                let selected = mode == cur;
                menu = menu.child(
                    div()
                        .id(mode.key())
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .when(selected, |d| d.bg(t.selected_bg))
                        .when(!selected, |d| d.hover(|d| d.bg(t.bg_mid)))
                        .child(div().text_color(t.text).child(mode.label()))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(t.line_number)
                                .child(mode.desc()),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| this.set_history_compare(mode, cx)),
                        ),
                );
            }
            root = root.child(
                div()
                    .absolute()
                    .top(px(0.0))
                    .left(px(0.0))
                    .size_full()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.history_compare_open = false;
                            cx.notify();
                        }),
                    )
                    .child(menu),
            );
        }

        root.into_any_element()
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
