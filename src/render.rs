//! Rendering for `Kyde` — `impl Render` + every `render_*` method.
//! Split out of `main.rs` (which held ~6k lines); a child module of the crate
//! root, so it reaches the root's (private) `Kyde` fields, helpers, and types
//! directly. Pure view code: builds gpui elements from `Kyde` state.

use super::*;

impl Render for Kyde {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = theme::font::UI_FAMILY;
        let fs = px(theme::get().ui_font_size);
        // Track the MAIN window's bounds (this `impl Render` runs only for the main window; the
        // modals are `ModalWindow`). The Diff modal opens at these bounds so it's as big as the
        // editor and lands over it.
        self.main_window_bounds = Some(window.window_bounds());

        // No project open → the Projects landing view.
        if self.repo_root.is_none() {
            // Drive the welcome-hero shimmer with a continuous repaint while it's shown.
            if self.recents.paths.is_empty() {
                window.request_animation_frame();
                self.welcome_frame = self.welcome_frame.wrapping_add(1);
            } else if !self.projects_search_focused && !self.onboarding.open {
                // Auto-focus the search box the first time the landing list appears.
                self.projects_search_focused = true;
                let handle = self.project_search.read(cx).focus_handle.clone();
                window.defer(cx, move |window, _cx| window.focus(&handle));
            }
            return self.render_projects(ui, fs, cx);
        }
        // A project is open → re-arm the landing auto-focus for next time it's shown.
        self.projects_search_focused = false;

        // A terminal tab closed last frame (^D / × on the focused tab) → focus the new active
        // tab now that it's in the window tree, so typing works without a mouse click.
        #[cfg(feature = "terminal")]
        if self.term.panel.focus_pending {
            self.term.panel.focus_pending = false;
            if self.term.panel.open {
                self.focus_active_terminal(window, cx);
            } else {
                // Last tab closed → panel hid; return focus to the app root so editor/tree
                // shortcuts work without a click.
                window.focus(&self.focus_handle);
            }
        }

        // FPS monitor: drive a continuous repaint so the number reflects the real frame cost
        // (catches drops), and measure the gap between renders.
        if self.fps.show {
            window.request_animation_frame();
            let now = std::time::Instant::now();
            if let Some(last) = self.fps.last {
                let dt = now.duration_since(last).as_secs_f32();
                if dt > 0.0 {
                    let inst = 1.0 / dt;
                    self.fps.value = if self.fps.value <= 0.0 {
                        inst
                    } else {
                        self.fps.value * 0.8 + inst * 0.2
                    };
                }
            }
            self.fps.last = Some(now);
            // The overlay reads `fps.shown`, snapshotted from the live EMA on a ~5/sec throttle
            // so the number is readable rather than blurring every frame.
            let due = self
                .fps
                .file_last
                .is_none_or(|t| now.duration_since(t).as_secs_f32() >= 0.2);
            if due {
                self.fps.shown = self.fps.value;
                self.fps.file_last = Some(now);
            }
        }

        // Left activity-rail icon button: folder = Browse, git = Commit (IntelliJ-style).
        let mode_btn = |icon: &'static str,
                        label: &'static str,
                        active: bool,
                        to: Mode,
                        cx: &mut Context<Self>| {
            let t = theme::get();
            div()
                .id(icon)
                .size(px(38.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .when(active, |d| d.bg(t.bg_light))
                .when(!active, |d| d.hover(|d| d.bg(t.bg_mid)))
                .cursor_pointer()
                .tooltip(move |_w, cx| cx.new(|_| Tip(label.into())).into())
                .child(svg().path(icon).size(px(22.0)).text_color(if active {
                    t.text
                } else {
                    t.line_number
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.switch_mode(to, cx)),
                )
        };

        // Native-blended title strip: a draggable frame-colored bar; the macOS traffic
        // lights sit at its left (titlebar is transparent), so we inset past them.
        let titlebar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(40.0))
            .pl(px(84.0))
            .gap_2()
            .bg(theme::get().titlebar_bg)
            // With ≤1 project open there's no tab strip, so show the project title (chip +
            // name + chevron) right of the traffic lights.
            .when(self.open_projects.len() <= 1, |d| {
                d.child(self.render_titlebar_project(cx))
            })
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .window_control_area(gpui::WindowControlArea::Drag)
                    // Double-click the title bar to zoom (maximize), matching macOS — not
                    // fullscreen.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_this, e: &gpui::MouseDownEvent, window, _cx| {
                            if e.click_count >= 2 {
                                window.zoom_window();
                            }
                        }),
                    ),
            );

        // Left activity rail with the mode icons.
        let rail = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .w(px(RAIL_W))
            // Fixed-width: never let a wide editor (many tabs) shrink the rail via flex.
            .flex_none()
            .h_full()
            // Align the first button with the top of the island (same inset as `body`'s pt),
            // and a matching bottom inset so the bottom-pinned terminal toggle sits symmetric.
            .pt(px(theme::FRAME_GAP))
            .pb(px(theme::FRAME_GAP))
            .bg(theme::get().frame_bg)
            .child(mode_btn(
                "icons/folder.svg",
                "Explorer",
                self.mode == Mode::Browse,
                Mode::Browse,
                cx,
            ))
            .child(mode_btn(
                "icons/git-branch.svg",
                "Commit & Git",
                self.mode == Mode::Commit,
                Mode::Commit,
                cx,
            ))
            .child(mode_btn(
                "icons/history.svg",
                "History",
                self.mode == Mode::History,
                Mode::History,
                cx,
            ));
        // Terminal toggle — pinned to the BOTTOM of the rail (IDE-style tool strip), only
        // when a project is open (the shell roots at the project). A flex spacer between the
        // top nav and this button pushes it down to the bottom edge.
        #[cfg(feature = "terminal")]
        let rail = if self.repo_root.is_some() {
            let t = theme::get();
            let active = self.term.panel.open;
            rail.child(div().flex_1().min_h_0()).child(
                div()
                    .id("rail-terminal")
                    .size(px(38.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .when(active, |d| d.bg(t.bg_light))
                    .when(!active, |d| d.hover(|d| d.bg(t.bg_mid)))
                    .cursor_pointer()
                    .tooltip(move |_w, cx| cx.new(|_| Tip("Terminal  (⌃`)".into())).into())
                    .child(
                        svg()
                            .path("icons/terminal.svg")
                            .size(px(22.0))
                            .text_color(if active { t.text } else { t.line_number }),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, window, cx| {
                            this.act_toggle_terminal(&ToggleTerminal, window, cx);
                        }),
                    ),
            )
        } else {
            rail
        };

        let inner = if self.diff_view_open {
            self.render_diff_view(ui, fs, window, cx)
        } else {
            match self.mode {
                Mode::Commit => self.render_commit(ui, fs, window, cx),
                Mode::Browse => self.render_browse(ui, fs, window, cx),
                Mode::History => self.render_history(ui, fs, window, cx),
            }
        };
        // Frame gap around the island panels (rail provides the left chrome).
        let body = div()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0() // contain the editor/tab strip so it scrolls instead of pushing the rail
            // No left pad: the rail's own right margin is the gap to the island.
            .pr(px(theme::FRAME_GAP))
            .pt(px(theme::FRAME_GAP))
            .pb(px(theme::FRAME_GAP))
            .bg(theme::get().frame_bg)
            .child(inner);

        // Right column = body (fills) with the terminal panel docked at its bottom. Keeping
        // the panel here (NOT a sibling of the full-height rail) means the rail spans the whole
        // window height, so its bottom-pinned terminal toggle stays put when the panel opens.
        // `relative` so the tab-overflow button (added below) anchors to the body region — it
        // sits BELOW any banners (which are flex-laid-out above main_row), so it tracks them.
        let right_col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .relative();
        // Maximized terminal hides the body (tree + editor) and fills the whole right column.
        #[cfg(feature = "terminal")]
        let term_max =
            self.term.panel.open && self.repo_root.is_some() && self.term.panel.maximized;
        #[cfg(not(feature = "terminal"))]
        let term_max = false;
        let right_col = right_col.when(!term_max, |c| c.child(body));
        #[cfg(feature = "terminal")]
        let right_col = if self.term.panel.open && self.repo_root.is_some() {
            right_col.child(self.render_terminal_panel(ui, cx))
        } else {
            right_col
        };
        // Tab chooser: floated over the body's top-right so it paints above the tab strip's
        // scroll layer. As a child of `right_col` (laid out below the banners) its `top` is
        // relative to the body, so an update/project-tabs banner can't misalign it.
        let right_col = if self.repo_root.is_some()
            && self.mode == Mode::Browse
            && !self.browse.open_tabs.is_empty()
        {
            right_col.child(self.render_tab_overflow_button(cx))
        } else {
            right_col
        };

        let main_row = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h_0()
            .child(rail)
            .child(right_col);

        // `root` is reassigned only under the `terminal` feature (the panel's actions); without
        // it the `mut` is unused — allow that precisely, so `RUSTFLAGS=-D warnings` stays green
        // in trim builds.
        #[cfg_attr(not(feature = "terminal"), allow(unused_mut))]
        let mut root = div()
            .key_context("Kyde")
            .track_focus(&self.focus_handle)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::get().frame_bg)
            // While dragging any divider, pin its resize cursor (row for vertical, col for
            // horizontal) across the whole window so it doesn't flicker as the pointer sweeps
            // over rows/editor.
            .when_some(self.divider_drag.map(|(k, _)| k.vertical()), |d, vert| {
                if vert {
                    d.cursor_row_resize()
                } else {
                    d.cursor_col_resize()
                }
            })
            .on_action(cx.listener(Self::act_go_to_file))
            .on_action(cx.listener(Self::act_find_in_files))
            .on_action(cx.listener(Self::act_actions))
            .on_action(cx.listener(Self::act_new_scratch))
            .on_action(cx.listener(Self::act_delete_file))
            .on_action(cx.listener(Self::act_close_tab))
            .on_action(cx.listener(Self::act_save))
            .on_action(cx.listener(Self::act_commit))
            .on_action(cx.listener(Self::act_mode_commit))
            .on_action(cx.listener(Self::act_mode_browse))
            .on_action(cx.listener(Self::act_sort_lines))
            .on_action(cx.listener(Self::act_sort_keys))
            .on_action(cx.listener(Self::act_nav_back))
            .on_action(cx.listener(Self::act_nav_forward))
            .on_action(cx.listener(Self::act_copy_files))
            .on_action(cx.listener(Self::act_cut_files))
            .on_action(cx.listener(Self::act_paste_files))
            .on_action(cx.listener(Self::open_keymap))
            .on_action(cx.listener(Self::open_recent_project))
            .on_action(cx.listener(Self::act_open_project))
            .on_action(cx.listener(Self::act_diff_next))
            .on_action(cx.listener(Self::act_diff_prev))
            .on_action(cx.listener(Self::act_toggle_fps))
            .on_action(cx.listener(Self::act_escape))
            .on_action(cx.listener(Self::act_confirm))
            .on_action(cx.listener(Self::act_clear_data))
            .on_action(cx.listener(Self::act_open_plugins))
            .on_action(cx.listener(Self::act_open_changelog))
            .on_action(cx.listener(Self::act_find))
            .on_action(cx.listener(Self::act_replace))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_prev))
            .on_action(cx.listener(Self::close_find))
            .on_action(cx.listener(Self::replace_one))
            .on_action(cx.listener(Self::replace_all));
        #[cfg(feature = "terminal")]
        {
            root = root
                .on_action(cx.listener(Self::act_toggle_terminal))
                .on_action(cx.listener(Self::act_new_terminal_tab))
                .on_action(cx.listener(Self::act_close_terminal_tab));
        }
        let root = root
            .on_mouse_move(
                cx.listener(move |this, e: &gpui::MouseMoveEvent, window, cx| {
                    // All dividers share one handler: `drag_divider` tracks the active one 1:1.
                    let sz = window.viewport_size();
                    let (vw, vh) = (f32::from(sz.width), f32::from(sz.height));
                    if this.drag_divider(e.position, vw, vh) {
                        cx.notify();
                    } else if let Some(drag) = this.sb_drag.clone() {
                        // Drag a scrollbar thumb: move the dragged view's content so the thumb
                        // tracks the cursor. Works for any view via the carried scroll handle.
                        let vp = drag.handle.bounds().size;
                        let max = drag.handle.max_offset();
                        let mut o = drag.handle.offset();
                        if drag.horizontal {
                            let (vp_w, max_w) = (f32::from(vp.width), f32::from(max.width));
                            let thumb = (vp_w * vp_w / (vp_w + max_w)).max(28.0);
                            let range = (vp_w - thumb).max(1.0);
                            let d = (f32::from(e.position.x) - drag.start_cursor) * (max_w / range);
                            o.x = px((drag.start_off - d).clamp(-max_w, 0.0));
                        } else {
                            let (vp_h, max_h) = (f32::from(vp.height), f32::from(max.height));
                            let thumb = (vp_h * vp_h / (vp_h + max_h)).max(28.0);
                            let range = (vp_h - thumb).max(1.0);
                            let d = (f32::from(e.position.y) - drag.start_cursor) * (max_h / range);
                            o.y = px((drag.start_off - d).clamp(-max_h, 0.0));
                        }
                        drag.handle.set_offset(o);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    let was_divider = this.divider_drag.take().is_some();
                    let was_sb = this.sb_drag.take().is_some();
                    if was_divider || was_sb {
                        cx.notify();
                    }
                }),
            )
            .child(titlebar)
            // Project tabs sit directly under the title bar, above all other UI — but only
            // once more than one project is open.
            .when(self.open_projects.len() > 1, |d| {
                d.child(self.render_project_tabs(ui, cx))
            })
            // Update-available banner sits directly under the titlebar (only when behind).
            .when(self.update_available.is_some(), |d| {
                d.child(self.render_update_banner(ui, cx))
            })
            // Merge-in-progress banner — TOP of the window, big and conflict-tinted, so
            // the way back into the conflicts window is always one click away (covers
            // merges started outside kyde too, e.g. a conflicted pull in a terminal).
            .when(self.merge.in_progress.is_some(), |d| {
                d.child(self.render_merge_banner(ui, cx))
            })
            .child(main_row);
        let mut root = root
            // Git-op error + crash banners at the bottom (just above the status bar).
            .when(self.op_error.is_some(), |d| {
                d.child(self.render_op_error_banner(ui, cx))
            })
            // Merge success note ("Merged X into Y") — neutral, ×-dismissed.
            .when(self.merge.note.is_some(), |d| {
                d.child(self.render_merge_note(ui, cx))
            })
            .when(self.pending_crash.is_some(), |d| {
                d.child(self.render_crash_banner(ui, cx))
            })
            .child(self.render_status_bar(ui, cx));

        if self.branch.popup_open {
            root = root.child(self.render_branch_popup(ui, fs, cx));
        }
        if self.worktree.popup_open {
            root = root.child(self.render_worktree_popup(ui, fs, cx));
        }
        // Rollback / Push / Diff are now separate native windows (`ModalWindow`), not overlays.
        if self.delete_target.is_some() {
            root = root.child(self.render_delete_modal(ui, cx));
        }
        if self.name_prompt.is_some() {
            root = root.child(self.render_name_prompt(ui, cx));
        }
        if self.finder.open {
            root = root.child(self.render_finder(ui, fs, cx));
        }
        if self.onboarding.open {
            root = root.child(self.render_onboarding(ui, fs, cx));
        }
        // Rollback/Push/Local-History menus belong to their own modal windows (rendered
        // in those bodies), not the main window.
        if matches!(self.context_menu.as_ref().map(|m| &m.target), Some(t) if !matches!(t, MenuTarget::RollbackFile(_) | MenuTarget::PushFile(_) | MenuTarget::LhRow | MenuTarget::LhPath(..)))
        {
            root = root.child(self.render_context_menu(cx));
        }
        if self.fps.show {
            let t = theme::get();
            root = root.child(
                div()
                    .absolute()
                    .top(px(44.0))
                    .right(px(10.0))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(t.panel_bg)
                    .border_1()
                    .border_color(t.divider)
                    .text_color(t.text)
                    .font_family(theme::font::FAMILY)
                    .text_size(px(11.0))
                    .child(SharedString::from(format!("{:.0} fps", self.fps.shown))),
            );
        }
        root.into_any_element()
    }
}

impl Kyde {
    /// Width of the editor island = window − rail − body right-pad − (tree + divider, or the
    /// collapsed strip + gap). Used to size the browse editor and the markdown split.
    pub(crate) fn editor_island_w(&self, window: &Window) -> f32 {
        let win_w = f32::from(window.viewport_size().width);
        let left_chrome = if self.browse.tree_collapsed {
            RAIL_W + theme::FRAME_GAP + 30.0 + theme::FRAME_GAP
        } else {
            RAIL_W + theme::FRAME_GAP + self.browse.tree_width + theme::FRAME_GAP
        };
        (win_w - left_chrome).max(100.0)
    }

    /// Wrap any scrollable `content` pane with always-visible scrollbars in **reserved gutters**
    /// (a thin column on the right for vertical, a thin row at the bottom for horizontal), each
    /// shown only when that axis overflows. Reusable across views (browse editor, file tree, …):
    /// the caller passes the pane's `ScrollHandle`, the total width available to the pane, and a
    /// `SbView` tag (which reframe-dims slot to debounce against).
    ///
    /// The pane is sized to an EXPLICIT width (`avail_w − v-bar`) rather than `flex_1`: a
    /// `flex_1` pane holding content-wide children refuses to shrink below that content (gpui
    /// doesn't reset min-content through `overflow_scroll`), so it would eat the whole area and
    /// push the right gutter off-screen.
    ///
    /// Thumbs are draggable: `on_mouse_down` arms `sb_drag` with this view's handle, the root
    /// `on_mouse_move` scrolls it.
    pub(crate) fn with_scrollbars(
        &mut self,
        content: gpui::Stateful<gpui::Div>,
        scroll: ScrollHandle,
        avail_w: f32,
        view: SbView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let max = scroll.max_offset();
        let vp = scroll.bounds().size;
        let off = scroll.offset();
        const BAR: f32 = 12.0; // gutter thickness
        let has_v = max.height > px(1.0);
        let has_h = max.width > px(1.0);
        // Explicit pane width so the v-gutter fits beside it instead of being pushed off-edge.
        let pane_w = (avail_w - if has_v { BAR } else { 0.0 }).max(50.0);
        // `min_h_0` so the scroll pane takes the flex viewport height, not its (taller) content
        // height — otherwise its measured bounds are wrong and the content can over-scroll.
        let pane = content
            .w(px(pane_w))
            .min_w_0()
            .min_h_0()
            .flex_none()
            .h_full();
        // Scroll metrics are only valid after the pane has painted, so the first frame after
        // open/resize reads stale (often zero) dims and the bars don't show. Track the inputs
        // that change the bars — pane width AND the painted viewport/overflow — and request one
        // more frame when any differ from what we drew with. Settles the frame after layout
        // stabilises, so it does not loop.
        let dims = (px(pane_w), vp.width, vp.height, max.width, max.height);
        if self.scroll_dims.get(&view) != Some(&dims) {
            self.scroll_dims.insert(view, dims);
            let entity = cx.entity();
            window.on_next_frame(move |_, cx| entity.update(cx, |_, cx| cx.notify()));
        }
        // Use the known `pane_w` (not the post-paint `vp.width`, which lags a frame) for the
        // horizontal track; `vp.height` is stable since the pane is always full height.
        let vp_h = f32::from(vp.height);
        // Track lengths shrink to leave room for the perpendicular bar's corner.
        let v_track = vp_h - if has_h { BAR } else { 0.0 };
        let h_track = pane_w - if has_v { BAR } else { 0.0 };

        // A thumb: `horizontal` picks the axis; positioned with a small offset inside its own
        // BAR-thick gutter (the gutter itself is edge-placed by flex). Captures `scroll` so the
        // drag it arms targets this view.
        let scroll_ref = &scroll;
        let thumb = |horizontal: bool, len: f32, pos: f32| {
            let sc = scroll_ref.clone();
            let base = div()
                .absolute()
                .rounded_full()
                .bg(t.line_number)
                .opacity(0.5)
                .hover(|s| s.opacity(0.85))
                .cursor_pointer();
            // Thinner than the gutter and centred in it, so there's an even margin on both
            // sides of the thumb (BAR − THUMB split in half) instead of it hugging one edge.
            const THUMB: f32 = 6.0;
            let m = (BAR - THUMB) / 2.0;
            let positioned = if horizontal {
                base.id("sb-h")
                    .top(px(m))
                    .left(px(pos))
                    .h(px(THUMB))
                    .w(px(len))
            } else {
                base.id("sb-v")
                    .left(px(m))
                    .top(px(pos))
                    .w(px(THUMB))
                    .h(px(len))
            };
            positioned
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        let cursor = if horizontal {
                            f32::from(e.position.x)
                        } else {
                            f32::from(e.position.y)
                        };
                        let start_off = if horizontal {
                            f32::from(sc.offset().x)
                        } else {
                            f32::from(sc.offset().y)
                        };
                        this.sb_drag = Some(crate::SbDrag {
                            handle: sc.clone(),
                            horizontal,
                            start_cursor: cursor,
                            start_off,
                        });
                        cx.notify();
                    }),
                )
                .into_any_element()
        };

        // Inset the thumb travel by `END` at both ends of the track so it can't run all the way
        // into the rounded-island corners / top controls — it stops short top and bottom.
        const END: f32 = 8.0;
        let v_gutter = has_v.then(|| {
            let (thumb_h, top) =
                scrollbar_thumb(v_track, f32::from(max.height), f32::from(off.y), END);
            div()
                .w(px(BAR))
                .flex_none()
                .h_full()
                .relative()
                .child(thumb(false, thumb_h, top))
        });
        let h_gutter = has_h.then(|| {
            let (thumb_w, left) =
                scrollbar_thumb(h_track, f32::from(max.width), f32::from(off.x), END);
            div()
                .h(px(BAR))
                .flex_none()
                .w_full()
                .relative()
                .child(thumb(true, thumb_w, left))
        });

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(pane)
                    .children(v_gutter),
            )
            .children(h_gutter)
            .into_any_element()
    }

    /// Right-click menu, positioned at the cursor. Transparent backdrop closes it.
    pub(crate) fn render_context_menu(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(menu) = &self.context_menu else {
            return div().into_any_element();
        };
        let t = theme::get();
        // Owned-label menu row (for runtime-built labels); `item` is the &'static convenience.
        // Each row gets a leading icon picked from its label (a fixed-width slot so labels with
        // no icon still align). Strips a trailing "✓ " / "…" when matching.
        let item_owned = |label: SharedString| {
            let icon = menu_icon(&label);
            let slot = div().flex_none().size(px(16.0)).flex().items_center();
            let slot = match icon {
                Some(path) => {
                    slot.child(svg().path(path).size(px(15.0)).text_color(t.secondary_text))
                }
                None => slot,
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                // Default arrow cursor over menu rows (don't inherit the editor's I-beam).
                .cursor(gpui::CursorStyle::Arrow)
                .text_color(t.text)
                .hover(|s| s.bg(t.selected_bg))
                .child(slot)
                .child(label)
        };
        let item = |label: &'static str| item_owned(SharedString::from(label));

        let mut panel = div()
            .cursor(gpui::CursorStyle::Arrow)
            .min_w(px(160.0))
            .flex()
            .flex_col()
            .py_1()
            .bg(t.bg_mid)
            .border_1()
            .border_color(t.divider)
            .rounded_md()
            .shadow_lg()
            .font_family(theme::font::UI_FAMILY)
            .text_size(px(theme::get().ui_font_size));

        panel = match &menu.target {
            MenuTarget::EditorGit(p, can_sort_lines, can_sort_keys) => {
                let (pc, pr) = (p.clone(), p.clone());
                // Buffer sort ops first (they act on the click point / selection),
                // then the git commands.
                if *can_sort_lines {
                    panel = panel.child(item("Sort Lines").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.sort_selected_lines(cx)),
                    ));
                }
                if *can_sort_keys {
                    panel = panel.child(item("Sort Object Keys").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.sort_object_keys_at_caret(cx)),
                    ));
                }
                // Git commands only when the project is a git repo (issue #66).
                if self.is_git {
                    panel = panel.child(item("Commit").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.menu_commit_path(pc.clone(), cx)),
                    ));
                    if self.has_changes_under(p) {
                        panel = panel.child(item("Rollback").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                this.open_rollback_path(pr.clone(), cx);
                            }),
                        ));
                    }
                }
                // Local History for the edited file (issue #7) — hidden when disabled.
                if self.lh.store.is_some() {
                    let pl = p.clone();
                    panel = panel.child(item("Local History").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.open_local_history(pl.clone(), cx);
                        }),
                    ));
                }
                // Git remote ops, WebStorm-style (Fetch/Pull always, Push when ahead).
                if self.is_git {
                    panel = panel
                        .child(item("Fetch").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.do_fetch(cx)),
                        ))
                        .child(item("Pull").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.do_pull(cx)),
                        ));
                    if self.sync.ahead.unwrap_or(0) > 0 {
                        panel = panel.child(item("Push").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.open_push_modal(cx)),
                        ));
                    }
                }
                panel
            }
            MenuTarget::BrowseFile(p, is_dir) => {
                let is_dir = *is_dir;
                let (pc, pr, pv) = (p.clone(), p.clone(), p.clone());
                // New File: create inside this folder, or in a file's parent folder.
                let new_dir = if is_dir {
                    p.clone()
                } else {
                    p.parent()
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or_default()
                };
                // A "<label> ▸" row that reveals a flyout column of items on hover/click
                // (IntelliJ-style; only one submenu open at a time). `flyout` is the
                // caller's column of `item(..)` rows — this adds the header, chevron, and
                // the right-hanging panel chrome.
                let submenu_row = |kind: Submenu,
                                   label: &'static str,
                                   icon_label: &'static str,
                                   flyout: gpui::Div| {
                    let open = menu.submenu == Some(kind);
                    let slot = {
                        let s = div().flex_none().size(px(16.0)).flex().items_center();
                        match menu_icon(icon_label) {
                            Some(path) => s.child(
                                svg().path(path).size(px(15.0)).text_color(t.secondary_text),
                            ),
                            None => s,
                        }
                    };
                    let mut wrap = div().relative().child(
                        div()
                            .id(SharedString::from(format!("menu-sub-{label}")))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_1()
                            .cursor(gpui::CursorStyle::Arrow)
                            .text_color(t.text)
                            .when(open, |d| d.bg(t.selected_bg))
                            .hover(|s| s.bg(t.selected_bg))
                            .child(slot)
                            .child(div().flex_1().child(label))
                            .child(
                                div().flex_none().flex().items_center().child(
                                    svg()
                                        .path("icons/chevron-right.svg")
                                        .size(px(14.0))
                                        .text_color(t.line_number),
                                ),
                            )
                            // Hover opens it (stays until you pick an item / dismiss); click
                            // toggles.
                            .on_hover(cx.listener(move |this, hovered: &bool, _w, cx| {
                                if *hovered {
                                    this.open_menu_submenu(kind, cx);
                                }
                            }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    cx.stop_propagation();
                                    this.toggle_menu_submenu(kind, cx);
                                }),
                            ),
                    );
                    if open {
                        wrap = wrap.child(
                            flyout
                                .absolute()
                                .left_full()
                                .top_0()
                                .ml(px(4.0))
                                .min_w(px(150.0))
                                .flex()
                                .flex_col()
                                .py_1()
                                .bg(t.bg_mid)
                                .border_1()
                                .border_color(t.divider)
                                .rounded_md()
                                .shadow_lg(),
                        );
                    }
                    wrap
                };

                // New ▸ — File / Scratch File / Directory.
                let (nd_file, nd_dir) = (new_dir.clone(), new_dir);
                let new_flyout = div()
                    .child(item("File").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, window, cx| {
                            this.start_new_file(nd_file.clone(), window, cx);
                        }),
                    ))
                    // Scratch file: language picked in the finder overlay; a scratch lives
                    // outside the tree, so it's not rooted at the clicked folder.
                    .child(item("Scratch File").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, window, cx| {
                            this.open_finder(FinderMode::Scratch, window, cx);
                        }),
                    ))
                    .child(item("Directory").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, window, cx| {
                            this.start_new_folder(nd_dir.clone(), window, cx);
                        }),
                    ));
                panel = panel.child(submenu_row(Submenu::New, "New", "New File…", new_flyout));
                panel = panel.child(ui::menu_divider());

                // Cut / Copy / Paste (issue #67). Copy/Cut act on the whole cmd-selection
                // when the clicked row is part of it, else just this row. Paste targets a
                // folder (or the clicked file's folder), pulling from kyde's clipboard, then
                // Finder file copies, then image data.
                {
                    let clip_paths = |this: &Kyde, row: &std::path::Path| -> Vec<PathBuf> {
                        if this.browse.multi_selected.iter().any(|s| s == row)
                            && this.browse.multi_selected.len() > 1
                        {
                            this.browse.multi_selected.clone()
                        } else {
                            vec![row.to_path_buf()]
                        }
                    };
                    let (pcut, pcopy) = (p.clone(), p.clone());
                    panel = panel
                        .child(item("Cut").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                let paths = clip_paths(this, &pcut);
                                this.clip_paths(paths, true, cx);
                            }),
                        ))
                        .child(item("Copy").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                let paths = clip_paths(this, &pcopy);
                                this.clip_paths(paths, false, cx);
                            }),
                        ));
                    // Paste destination: the folder itself, else the file's parent folder.
                    let dest = if is_dir {
                        p.clone()
                    } else {
                        p.parent()
                            .map(std::path::Path::to_path_buf)
                            .unwrap_or_default()
                    };
                    panel = panel.child(item("Paste").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.paste_into(dest.clone(), cx);
                        }),
                    ));
                }
                // Exactly two cmd-selected files → side-by-side compare (issue #42).
                if self.browse.multi_selected.len() == 2 {
                    let (a, b) = (
                        self.browse.multi_selected[0].clone(),
                        self.browse.multi_selected[1].clone(),
                    );
                    panel = panel.child(item("Compare Selected").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.open_compare(a.clone(), b.clone(), cx);
                        }),
                    ));
                }
                panel = panel.child(ui::menu_divider());

                // Rename / Delete — act on the file/folder itself. Rename covers both files
                // and folders (a folder rename carries its whole subtree).
                {
                    let (pn, pd) = (p.clone(), p.clone());
                    panel = panel
                        .child(item("Rename…").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, window, cx| {
                                this.start_rename(pn.clone(), window, cx);
                            }),
                        ))
                        .child(item("Delete…").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| this.open_delete(pd.clone(), cx)),
                        ));
                }

                // Local History + Git group (each conditional) — its own divided section.
                if self.lh.store.is_some() || self.is_git {
                    panel = panel.child(ui::menu_divider());
                }
                // Local History (issue #7) — file OR folder scope; hidden when disabled.
                if self.lh.store.is_some() {
                    let pl = p.clone();
                    panel = panel.child(item("Local History").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.open_local_history(pl.clone(), cx);
                        }),
                    ));
                }

                // Git ▸ — commit / rollback / history / fetch / pull / push, grouped.
                // The whole submenu is hidden for a plain (non-git) folder (issue #66).
                if self.is_git {
                    let ph = p.clone();
                    let mut git_flyout = div();
                    if self.has_changes_under(p) {
                        git_flyout = git_flyout
                            .child(item("Commit").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    this.menu_commit_path(pc.clone(), cx);
                                }),
                            ))
                            .child(item("Rollback").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    this.open_rollback_path(pr.clone(), cx);
                                }),
                            ));
                    }
                    git_flyout = git_flyout
                        .child(item("Git History").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                this.enter_history_for(ph.clone(), cx);
                            }),
                        ))
                        .child(item("Fetch").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.do_fetch(cx)),
                        ))
                        .child(item("Pull").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.do_pull(cx)),
                        ));
                    if self.sync.ahead.unwrap_or(0) > 0 {
                        git_flyout = git_flyout.child(item("Push").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.open_push_modal(cx)),
                        ));
                    }
                    panel = panel.child(submenu_row(Submenu::Git, "Git", "Commit", git_flyout));
                }

                // Reveal in Finder — its own trailing group.
                panel
                    .child(ui::menu_divider())
                    .child(item("Reveal in Finder").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.reveal_in_os(&pv, cx)),
                    ))
            }
            MenuTarget::Tab(idx) => {
                let idx = *idx;
                let reveal = self.browse.open_tabs.get(idx).cloned();
                panel = panel
                    .child(item("Close").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.close_tab(idx, cx)),
                    ))
                    .child(item("Close Others").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.close_other_tabs(idx, cx)),
                    ))
                    .child(item("Close Tabs to the Right").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.close_tabs_right(idx, cx)),
                    ));
                // Compare this tab's file against the ACTIVE tab's (issue #42) —
                // only offered on a non-active tab, where the pair is meaningful.
                if let (Some(tab), Some(active)) = (
                    self.browse.open_tabs.get(idx).cloned(),
                    self.browse.open_path.clone(),
                ) {
                    if tab != active {
                        panel = panel.child(item("Compare with Current Tab").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                // Active tab on the left (the "current" baseline),
                                // the clicked tab on the right.
                                this.open_compare(active.clone(), tab.clone(), cx);
                            }),
                        ));
                    }
                }
                if let Some(tab) = self.browse.open_tabs.get(idx).cloned() {
                    if self.lh.store.is_some() {
                        panel = panel.child(item("Local History").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                this.open_local_history(tab.clone(), cx);
                            }),
                        ));
                    }
                }
                panel.child(item("Reveal in Finder").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        if let Some(p) = reveal.clone() {
                            this.reveal_in_os(&p, cx);
                        }
                    }),
                ))
            }
            MenuTarget::CommitPath(path, _is_dir) => {
                let path = path.clone();
                // Folders + files both get Rollback. (Staging is implicit — checking a file
                // in the tree includes it in the commit; no separate Stage item.)
                let pr = path.clone();
                panel = panel.child(item("Rollback").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.open_rollback_path(pr.clone(), cx)),
                ));
                if self.sync.ahead.unwrap_or(0) > 0 {
                    panel = panel.child(item("Push").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.open_push_modal(cx)),
                    ));
                }
                panel
            }
            MenuTarget::RollbackFile(idx) => {
                let idx = *idx;
                panel.child(item("View Diff").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.menu_show_diff(idx, cx)),
                ))
            }
            MenuTarget::PushFile(idx) => {
                let idx = *idx;
                panel.child(item("View Diff").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.push_show_diff(idx, cx)),
                ))
            }
            // Local History menus (timeline row / changed-files row) — the feature
            // module owns its items (`lh_menu_items`), this match only dispatches.
            t @ (MenuTarget::LhRow | MenuTarget::LhPath(..)) => {
                self.lh_menu_items(panel, t, &item, cx)
            }
            MenuTarget::TabList => {
                if self.browse.open_tabs.is_empty() {
                    panel = panel.child(item("No open tabs"));
                }
                for (i, p) in self.browse.open_tabs.iter().enumerate() {
                    let active = self.browse.open_path.as_ref() == Some(p);
                    let name: SharedString = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into();
                    panel = panel.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_1()
                            .cursor(gpui::CursorStyle::Arrow)
                            .text_color(if active { t.text } else { t.line_number })
                            .hover(|s| s.bg(t.selected_bg))
                            .child(div().flex_none().child(badge_inner(file_badge(p), 0.0)))
                            .child(div().min_w_0().truncate().child(name))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    if let Some(p) = this.browse.open_tabs.get(i).cloned() {
                                        this.open_file(p, cx);
                                        this.close_menu(cx);
                                    }
                                }),
                            ),
                    );
                }
                panel
            }
            // Same compare options as the header dropdown, applied to the right-clicked commit.
            MenuTarget::HistoryCompare(idx) => {
                let idx = *idx;
                let cur = self.history.compare;
                for mode in CompareMode::ALL {
                    let label = if mode == cur {
                        SharedString::from(format!("✓ {}", mode.label()))
                    } else {
                        SharedString::from(mode.label())
                    };
                    panel = panel.child(item_owned(label).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.history_compare_commit(idx, mode, cx);
                        }),
                    ));
                }
                panel
            }
            // A branch row in the branch popup: merge it into the current branch, or
            // check it out (same as a left-click).
            MenuTarget::Branch(name) => {
                let cur = self.current_branch.clone().unwrap_or_else(|| "HEAD".into());
                let (nm_merge, nm_co) = (name.clone(), name.clone());
                // Checked out in another worktree → Checkout jumps there (say so);
                // hidden entirely inside a linked worktree with no jump target (the
                // worktree is pinned to its branch — merge is still allowed).
                let elsewhere = self
                    .other_worktree_for_branch(name)
                    .and_then(|w| w.path.file_name())
                    .map(|s| s.to_string_lossy().into_owned());
                let can_checkout = elsewhere.is_some() || !self.in_linked_worktree();
                if can_checkout {
                    let label = match elsewhere {
                        Some(wt) => format!("Checkout  (jumps to “{wt}”)"),
                        None => "Checkout".to_string(),
                    };
                    panel = panel.child(item_owned(SharedString::from(label)).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, window, cx| {
                            this.context_menu = None;
                            this.checkout_branch(nm_co.clone(), window, cx);
                        }),
                    ));
                }
                panel.child(
                    item_owned(SharedString::from(format!("Merge “{name}” into “{cur}”")))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                this.menu_merge_branch(nm_merge.clone(), cx);
                            }),
                        ),
                )
            }
            // Same compare options again, from a file row — the file stays selected after.
            MenuTarget::HistoryFile(idx) => {
                let idx = *idx;
                let cur = self.history.compare;
                for mode in CompareMode::ALL {
                    let label = if mode == cur {
                        SharedString::from(format!("✓ {}", mode.label()))
                    } else {
                        SharedString::from(mode.label())
                    };
                    panel = panel.child(item_owned(label).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.history_compare_file(idx, mode, cx);
                        }),
                    ));
                }
                panel
            }
        };

        // Backdrop: any click (incl. right elsewhere) dismisses the menu. `occlude()` makes it
        // absorb ALL mouse events (incl. hover/move), so hovering the menu — or anywhere over
        // the backdrop — doesn't bleed through to the rows painted behind it.
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.close_menu(cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _e, _w, cx| this.close_menu(cx)),
            )
            // `anchored` positions the panel at the cursor but flips/snaps it back inside the
            // window when it would overflow an edge (so a menu near the bottom/right opens
            // upward/leftward instead of being clipped). `deferred` paints it above the rest.
            .child({
                // Branch actions fly out LEFT of the branch popup: `at` is the popup's
                // left edge, so anchor the menu's top-RIGHT corner there (every other
                // menu hangs from its top-left at the cursor).
                let mut anchored = gpui::anchored().position(menu.at);
                if matches!(menu.target, MenuTarget::Branch(_)) {
                    anchored = anchored.anchor(gpui::Corner::TopRight);
                }
                gpui::deferred(anchored.snap_to_window_with_margin(px(8.0)).child(panel))
                    .with_priority(1)
            })
            .into_any_element()
    }
}
