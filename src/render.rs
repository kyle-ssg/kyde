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
            } else if !self.projects_search_focused && !self.onboarding_open {
                // Auto-focus the search box the first time the landing list appears.
                self.projects_search_focused = true;
                let handle = self.project_search.read(cx).focus_handle.clone();
                window.defer(cx, move |window, _cx| window.focus(&handle));
            }
            return self.render_projects(ui, fs, cx);
        }
        // A project is open → re-arm the landing auto-focus for next time it's shown.
        self.projects_search_focused = false;

        // FPS monitor: drive a continuous repaint so the number reflects the real frame cost
        // (catches drops), and measure the gap between renders.
        if self.show_fps {
            window.request_animation_frame();
            let now = std::time::Instant::now();
            if let Some(last) = self.fps_last {
                let dt = now.duration_since(last).as_secs_f32();
                if dt > 0.0 {
                    let inst = 1.0 / dt;
                    self.fps_value = if self.fps_value <= 0.0 {
                        inst
                    } else {
                        self.fps_value * 0.8 + inst * 0.2
                    };
                }
            }
            self.fps_last = Some(now);
            // The overlay reads `fps_shown`, snapshotted from the live EMA on a ~5/sec throttle
            // so the number is readable rather than blurring every frame.
            let due = self
                .fps_file_last
                .is_none_or(|t| now.duration_since(t).as_secs_f32() >= 0.2);
            if due {
                self.fps_shown = self.fps_value;
                self.fps_file_last = Some(now);
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
                    cx.listener(move |this, _e, _w, cx| match to {
                        Mode::Commit => this.enter_commit(cx),
                        Mode::History => this.enter_history(cx),
                        Mode::Browse => {
                            this.mode = Mode::Browse;
                            this.diff_view_open = false;
                            cx.notify();
                        }
                    }),
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
            .bg(theme::get().frame_bg)
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
            let active = self.term_open;
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
                            this.act_toggle_terminal(&ToggleTerminal, window, cx)
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
        let right_col = div().flex().flex_col().flex_1().min_w_0().min_h_0();
        // Maximized terminal hides the body (tree + editor) and fills the whole right column.
        #[cfg(feature = "terminal")]
        let term_max = self.term_open && self.repo_root.is_some() && self.term_maximized;
        #[cfg(not(feature = "terminal"))]
        let term_max = false;
        let right_col = right_col.when(!term_max, |c| c.child(body));
        #[cfg(feature = "terminal")]
        let right_col = if self.term_open && self.repo_root.is_some() {
            right_col.child(self.render_terminal_panel(ui, cx))
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
            .on_action(cx.listener(Self::open_keymap))
            .on_action(cx.listener(Self::open_recent_project))
            .on_action(cx.listener(Self::act_open_project))
            .on_action(cx.listener(Self::act_diff_next))
            .on_action(cx.listener(Self::act_diff_prev))
            .on_action(cx.listener(Self::act_toggle_fps))
            .on_action(cx.listener(Self::act_escape))
            .on_action(cx.listener(Self::act_clear_data))
            .on_action(cx.listener(Self::act_open_plugins))
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
                .on_action(cx.listener(Self::act_new_terminal_tab));
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
            .child(main_row);
        let mut root = root
            // Git-op error + crash banners at the bottom (just above the status bar).
            .when(self.op_error.is_some(), |d| {
                d.child(self.render_op_error_banner(ui, cx))
            })
            .when(self.pending_crash.is_some(), |d| {
                d.child(self.render_crash_banner(ui, cx))
            })
            .child(self.render_status_bar(ui, cx));

        // Tab chooser: floated at root so it paints above the tab strip's scroll layer.
        // Shown whenever tabs are open. (True overflow-only gating isn't reliable here —
        // gpui's `max_offset` is measured during the scroll element's paint, but this runs
        // before that paint, so it's always a frame stale and the button never appears.)
        if self.repo_root.is_some() && self.mode == Mode::Browse && !self.open_tabs.is_empty() {
            root = root.child(self.render_tab_overflow_button(cx));
        }
        if self.branch_popup_open {
            root = root.child(self.render_branch_popup(ui, fs, cx));
        }
        // Rollback / Push / Diff are now separate native windows (`ModalWindow`), not overlays.
        if self.delete_target.is_some() {
            root = root.child(self.render_delete_modal(ui, cx));
        }
        if self.name_prompt.is_some() {
            root = root.child(self.render_name_prompt(ui, cx));
        }
        if self.finder_open {
            root = root.child(self.render_finder(ui, fs, cx));
        }
        if self.onboarding_open {
            root = root.child(self.render_onboarding(ui, fs, cx));
        }
        // Rollback/Push file menus belong to their own modal windows (rendered in those
        // bodies), not the main window.
        if matches!(self.context_menu.as_ref().map(|m| &m.target), Some(t) if !matches!(t, MenuTarget::RollbackFile(_) | MenuTarget::PushFile(_)))
        {
            root = root.child(self.render_context_menu(cx));
        }
        if self.show_fps {
            let t = theme::get();
            root = root.child(
                div()
                    .absolute()
                    .top(px(44.0))
                    .right(px(10.0))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(gpui::rgba(0x000000CC))
                    .text_color(t.text)
                    .font_family(theme::font::FAMILY)
                    .text_size(px(11.0))
                    .child(SharedString::from(format!("{:.0} fps", self.fps_shown))),
            );
        }
        root.into_any_element()
    }
}

impl Kyde {
    fn render_projects(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // No recent projects → the animated welcome hero (no search box, nothing to search).
        if self.recents.paths.is_empty() {
            return self.render_welcome(ui, fs, cx);
        }
        let query = self.project_search.read(cx).text().to_lowercase();

        // Primary = filled accent + white text. Secondary = transparent bg, divider
        // border, secondary text.
        let button = |label: &'static str, accent: bool, _cx: &mut Context<Self>| {
            let t = theme::get();
            div()
                .px_4()
                .py_1p5()
                .rounded_md()
                .border_1()
                .font_weight(FontWeight::SEMIBOLD)
                .when(accent, |d| d.bg(t.primary).text_color(t.primary_text))
                .when(!accent, |d| {
                    d.border_color(t.divider).text_color(t.secondary_text)
                })
                .child(label)
        };

        // A draggable title strip holds the macOS traffic lights on its own line, so the search
        // row sits cleanly *below* them (not tucked under the close/minimise buttons).
        let titlebar = div().flex().flex_none().h(px(38.0)).w_full().child(
            div()
                .size_full()
                .window_control_area(gpui::WindowControlArea::Drag),
        );
        // One row below the titlebar: search box fills the left, New Project / Open inline right.
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme::get().divider)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_color(theme::get().line_number)
                    .child(
                        svg()
                            .path("icons/search.svg")
                            .size(px(15.0))
                            .text_color(theme::get().line_number),
                    )
                    .child(div().flex_1().child(self.project_search.clone())),
            )
            .child(button("New Project", false, cx).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.pick_folder(cx)),
            ))
            .child(button("Open", true, cx).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.pick_folder(cx)),
            ));

        let body = self.render_recents_list(&query, cx);

        let mut root = div()
            .key_context("Kyde")
            .track_focus(&self.focus_handle)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::get().main_bg)
            .font_family(ui)
            .text_size(fs)
            .child(titlebar)
            .child(header)
            .child(body)
            // Git-op error + crash banners pinned to the bottom of the window.
            .when(self.op_error.is_some(), |d| {
                d.child(self.render_op_error_banner(ui, cx))
            })
            .when(self.pending_crash.is_some(), |d| {
                d.child(self.render_crash_banner(ui, cx))
            });

        if self.onboarding_open {
            root = root.child(self.render_onboarding(ui, fs, cx));
        }
        root.into_any_element()
    }

    /// The scrollable recent-projects list (Projects screen, when there are recents).
    fn render_recents_list(&self, query: &str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let rows = self
            .recents
            .paths
            .iter()
            .filter(|p| {
                if query.is_empty() {
                    return true;
                }
                let name = projects::name_of(p).to_lowercase();
                name.contains(query) || p.to_string_lossy().to_lowercase().contains(query)
            })
            .map(|p| {
                let name = projects::name_of(p);
                let icon_color = gpui::rgb(projects::color_for(&name));
                let pretty = projects::pretty_path(p);
                let pclone = p.clone();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .hover(|s| s.bg(theme::get().caret_row))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(34.0))
                            .rounded_md()
                            .bg(icon_color)
                            .text_color(gpui::white())
                            .child(SharedString::from(projects::initials(&name))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::get().text)
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                div()
                                    .text_color(theme::get().line_number)
                                    .child(SharedString::from(pretty)),
                            ),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.open_project(pclone.clone(), cx)),
                    )
            });
        div()
            .id("recents")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .flex_1()
            .children(rows)
            .into_any_element()
    }

    /// First-run / no-recents welcome: an animated 3D "KY" (ANSI-Shadow blocks with a
    /// diagonal shimmer sweeping the faces), a tagline, and New Project / Open Folder.
    fn render_welcome(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        const ART: &[&str] = &[
            "██╗  ██╗██╗   ██╗██████╗ ███████╗",
            "██║ ██╔╝╚██╗ ██╔╝██╔══██╗██╔════╝",
            "█████╔╝  ╚████╔╝ ██║  ██║█████╗  ",
            "██╔═██╗   ╚██╔╝  ██║  ██║██╔══╝  ",
            "██║  ██╗   ██║   ██████╔╝███████╗",
            "╚═╝  ╚═╝   ╚═╝   ╚═════╝ ╚══════╝",
        ];
        let frame = self.welcome_frame as f32;
        let mono = gpui::Font {
            family: theme::font::FAMILY.into(),
            features: Default::default(),
            fallbacks: None,
            weight: FontWeight::BOLD,
            style: Default::default(),
        };
        let shadow: gpui::Hsla = gpui::rgb(0x223056).into();
        let art_lines: Vec<gpui::AnyElement> = ART
            .iter()
            .enumerate()
            .map(|(row, line)| {
                let runs: Vec<gpui::TextRun> = line
                    .chars()
                    .enumerate()
                    .map(|(col, ch)| {
                        let color: gpui::Hsla = if ch == '█' {
                            // diagonal highlight band sweeping left→right over the faces
                            let phase = (col as f32 * 0.5 + row as f32 * 0.45) - frame * 0.14;
                            let b = phase.sin() * 0.5 + 0.5;
                            lerp_rgb(0x2E5BD0, 0xCFE0FF, b).into()
                        } else if ch == ' ' {
                            gpui::rgba(0x00000000).into()
                        } else {
                            shadow
                        };
                        gpui::TextRun {
                            len: ch.len_utf8(),
                            font: mono.clone(),
                            color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }
                    })
                    .collect();
                gpui::StyledText::new(SharedString::from(*line))
                    .with_runs(runs)
                    .into_any_element()
            })
            .collect();

        let new_btn = btn_primary("welcome-new", "New Project")
            .w_full()
            .flex()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.pick_folder(cx)),
            );
        let open_btn = btn_secondary("welcome-open", "Open Folder")
            .w_full()
            .flex()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.pick_folder(cx)),
            );

        let hero = div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_6()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .text_size(px(16.0))
                    .line_height(px(18.0))
                    .children(art_lines),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .w(px(220.0))
                    .mt_4()
                    .child(new_btn)
                    .child(open_btn),
            );

        let mut root = div()
            .key_context("Kyde")
            .track_focus(&self.focus_handle)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.frame_bg)
            .font_family(ui)
            .text_size(fs)
            // Draggable strip over the traffic lights (the window has a transparent titlebar).
            .child(
                div()
                    .h(px(40.0))
                    .flex_none()
                    .w_full()
                    .window_control_area(gpui::WindowControlArea::Drag),
            )
            .child(hero)
            // Git-op error + crash banners pinned to the bottom of the window.
            .when(self.op_error.is_some(), |d| {
                d.child(self.render_op_error_banner(ui, cx))
            })
            .when(self.pending_crash.is_some(), |d| {
                d.child(self.render_crash_banner(ui, cx))
            });
        if self.onboarding_open {
            root = root.child(self.render_onboarding(ui, fs, cx));
        }
        root.into_any_element()
    }

    fn render_commit(
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
                self.tree_row(
                    cx,
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

    /// Left column of the Push tab: a flat list of the files a push would send (click → diff)
    /// + a footer with the Push button. No checkboxes — a push is all-or-nothing.
    fn render_push_left(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
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

    /// History (git log) view: a commit list (left), the selected commit's changed files
    /// (middle), and a read-only side-by-side diff (right). A branch dropdown picks which
    /// ref to log; a segmented control picks what the commit is diffed against.
    fn render_history(
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

    /// Side-by-side diff = two editors in one rounded island: left is the read-only base
    /// (HEAD/index), right is the editable working copy (live-saved). A draggable divider
    /// sets the 50/50 split. Both syntax-highlight when the language pack is installed.
    /// Full-screen Show-Diff view in the MAIN window (rollback / push diff). A header with a
    /// Back button + the file path over the inline `render_diff`, so it reuses every editor
    /// feature — find (⌘F), divider drag, scrollbars, change navigation. Escape / Back exits.
    fn render_diff_view(
        &mut self,
        ui: &'static str,
        fs: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let title: SharedString = self
            .diff_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Diff".to_string())
            .into();
        let back = div()
            .id("diff-back")
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_0p5()
            .rounded_md()
            .cursor_pointer()
            .text_color(t.secondary_text)
            .hover(|s| s.bg(t.bg_mid).text_color(t.text))
            .child("‹ Back")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.close_diff_view(cx)),
            );
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .flex_none()
            .px_1()
            .pb(px(theme::FRAME_GAP))
            .font_family(ui)
            .text_size(px(t.ui_font_size))
            .child(back)
            .child(div().min_w_0().truncate().text_color(t.text).child(title));
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(header);
        // The find/replace bar (⌘F / ⌘R), targeting whichever diff pane is focused.
        if self.find_open {
            col = col.child(self.render_find_bar(ui, cx));
        }
        col.child(self.render_diff(ui, fs, Some(window), cx))
            .into_any_element()
    }

    /// IntelliJ-style side-by-side diff: aligned rows, with a center gutter showing the old
    /// and new line numbers, a `»` chevron (revert the hunk) and a checkbox (stage it).
    fn render_diff(
        &mut self,
        _ui: &'static str,
        fs: gpui::Pixels,
        mut window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let island = || {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .bg(t.main_bg)
                .rounded(px(theme::ISLAND_RADIUS))
                .overflow_hidden()
                .font_family(theme::font::FAMILY)
                .text_size(fs)
                .text_color(t.text)
        };

        // The Diff modal window renders the same `diff_left`/`diff_right` editors; rendering one
        // editor entity in two windows desyncs scroll + garbles layout. So while the modal is
        // open, the INLINE diff (the only caller that passes `Some(window)`) yields the panes to
        // it and shows a placeholder. The modal itself passes `None`, so it renders normally.
        if window.is_some() && self.diff_modal_open {
            return island()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.line_number)
                .child("Viewing diff in window…")
                .into_any_element();
        }

        // Image file selected → preview it centered + scaled (same as Browse), not a text diff.
        if let Some(rel) = self.diff_image.clone() {
            let abs = self.repo_root.as_ref().map(|r| r.join(&rel)).unwrap_or(rel);
            return island()
                .id("diff-image-scroll")
                .overflow_scroll()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .child(img(abs).max_w_full().max_h_full())
                .into_any_element();
        }

        let Some(d) = self
            .current_diff
            .as_ref()
            .filter(|_| self.diff_path.is_some())
        else {
            return island()
                .flex()
                .justify_center()
                .items_center()
                .text_color(t.line_number)
                .child("Select a file")
                .into_any_element();
        };

        // Center gutter: a `»` (revert this hunk) on each hunk's first row, sharing
        // `diff_scroll` so it tracks the editors. Chevrons are positioned ABSOLUTELY at
        // `row * row_h` inside a fixed-height column — a flex column of per-row divs let
        // empty rows collapse (gpui ignores `.h()` on a childless div), which bunched every
        // chevron toward the top instead of onto its hunk row.
        let row_h = px(editor::line_height_px());
        let rows = aligned_rows(d);
        let total_h = row_h * rows.len() as f32;
        // Read-only diffs (push view) show a committed change with no working-tree edit,
        // so there's nothing to revert — drop the gutter chevrons.
        let chevrons: Vec<gpui::AnyElement> = if self.diff_readonly {
            Vec::new()
        } else {
            rows.iter()
                .enumerate()
                .filter_map(|(i, r)| {
                    let hi = r.hunk_start.then_some(r.hunk).flatten()?;
                    Some(
                        // Position the row's line box exactly over the editor line (`line_height`
                        // = `row_h`), so the `»` baselines with the line's text instead of being
                        // half-centered a few px high.
                        div()
                            .absolute()
                            .top(row_h * i as f32)
                            .left_0()
                            .right_0()
                            .h(row_h)
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .child(
                                // Flex-center the glyph in a fixed box so it's truly centered
                                // (a line-height hack left it sitting low).
                                div()
                                    .id(SharedString::from(format!("revert-{hi}")))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(24.0))
                                    .rounded_sm()
                                    .text_size(px(20.0))
                                    .line_height(px(20.0))
                                    .text_color(t.line_number)
                                    .hover(|s| s.bg(t.bg_light).text_color(t.primary))
                                    .cursor_pointer()
                                    .child("»")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _e, _w, cx| {
                                            this.diff_revert_hunk(hi, cx)
                                        }),
                                    ),
                            )
                            .into_any_element(),
                    )
                })
                .collect()
        };

        // Pane = shared VERTICAL scroll (`diff_scroll`, keeps the two sides' rows aligned)
        // wrapping an INDEPENDENT horizontal scroll around a content-width editor, so long
        // lines scroll sideways per pane without breaking row alignment.
        let frac = self.diff_split.clamp(0.15, 0.85);
        let lw = self.diff_left.read(cx).content_width();
        let rw = self.diff_right.read(cx).content_width();
        let pane_scroll =
            |id: &'static str, scroll: &ScrollHandle, w: f32, editor: gpui::AnyElement| {
                div()
                    .id(id)
                    .h_full()
                    .w_full()
                    .overflow_scroll()
                    .track_scroll(scroll)
                    .child(div().w(px(w)).child(editor))
            };
        let left_inner = pane_scroll(
            "diff-left-scroll",
            &self.diff_scroll,
            lw,
            self.diff_left.clone().into_any_element(),
        );
        let right_inner = pane_scroll(
            "diff-right-scroll",
            &self.diff_scroll,
            rw,
            self.diff_right.clone().into_any_element(),
        );
        // One shared horizontal scrollbar driven by `diff_scroll`; its travel is the wider
        // pane's content. Placed on the island (full width) below.
        let h_bar = self.diff_hscrollbar(
            &self.diff_scroll.clone(),
            lw.max(rw),
            SbView::DiffLeftH,
            window.as_deref_mut(),
            cx,
        );
        // New file (empty left) or deleted file (empty right): show ONLY the populated side,
        // full-width. A side-by-side with one empty pane is noise — and the empty pane drives
        // the shared scroll handle's bounds to ~0, which blanks the viewport-culled editor on a
        // large file. Full-width, the surviving editor owns the layout and paints normally.
        let left_empty = self.diff_left.read(cx).text().is_empty();
        let right_empty = self.diff_right.read(cx).text().is_empty();
        if left_empty != right_empty {
            let inner = if left_empty { right_inner } else { left_inner };
            let scrollbar = self.diff_vscrollbar(total_h, window.as_deref_mut(), cx);
            return island()
                .relative()
                .flex()
                .flex_row()
                .child(div().relative().flex_1().min_w_0().h_full().child(inner))
                .children(h_bar)
                .children(scrollbar)
                .children(self.render_diff_nav(cx))
                .into_any_element();
        }

        // Left pane width: when we know the viewport (the inline diff, the only resizable
        // caller — the modal passes `None`), size it to an explicit pixel width from the SAME
        // formula the resize handler inverts, and let the right pane flex to fill the rest. The
        // modal falls back to proportional flex_basis (its divider isn't dragged through the
        // main-window handler). Both keep `min_w_0` so neither pane can be pushed off-window.
        let left_px = window.as_deref().map(|w| {
            (frac * (full_island_w(f32::from(w.viewport_size().width)) - DIFF_GUTTER_W)).max(40.0)
        });
        let left = div().relative().min_w_0().h_full();
        let left = match left_px {
            Some(w) => left.w(px(w)).flex_none(),
            None => left.flex_basis(gpui::relative(frac)).flex_shrink(),
        }
        .child(left_inner);
        let right = div().relative().min_w_0().h_full();
        let right = match left_px {
            Some(_) => right.flex_1(),
            None => right.flex_basis(gpui::relative(1.0 - frac)).flex_shrink(),
        }
        .child(right_inner);
        // The gutter (chevrons) shares the editors' vertical scroll by translating its content
        // by the SAME offset; it also doubles as the draggable divider (drag to resize the
        // split). Clicks on a `»` still revert their hunk (chevrons are children).
        let scroll_y = self.diff_scroll.offset().y;
        let gutter = div()
            .id("diff-gutter")
            .w(px(DIFF_GUTTER_W))
            .flex_none()
            .h_full()
            .overflow_hidden()
            .bg(t.diff_separator_bg)
            // Thin divider lines on both edges so the center gutter reads as the
            // pane separator (otherwise it blends into the panes at most font sizes).
            .border_l(px(1.0))
            .border_r(px(1.0))
            .border_color(t.divider)
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                    this.start_divider_drag(Divider::DiffPane, e.position, window);
                    cx.notify();
                }),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(total_h)
                    .top(scroll_y)
                    .children(chevrons),
            );

        // Single vertical scrollbar overlaid on the right edge of the island; both panes
        // share `diff_scroll`, so one bar drives the whole diff. `total_h` is the exact
        // aligned-row content height (both panes are padded to it), so the bar is driven by
        // that rather than the shared handle's `max_offset` — which reflects whichever pane
        // painted last and is ~0 when one side is empty (e.g. an all-added/untracked file).
        let scrollbar = self.diff_vscrollbar(total_h, window, cx);

        island()
            .relative()
            .flex()
            .flex_row()
            .child(left)
            .child(gutter)
            .child(right)
            .children(h_bar)
            .children(scrollbar)
            .children(self.render_diff_nav(cx))
            .into_any_element()
    }

    /// A small floating control at the diff's top-right: the change count + prev/next arrows
    /// that jump between hunks. Shown whenever the current diff has at least one change.
    fn render_diff_nav(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let n = self
            .current_diff
            .as_ref()
            .map(|d| d.hunks.len())
            .unwrap_or(0);
        // Nothing to navigate with a single (or zero) change — hide the control.
        if n < 2 {
            return None;
        }
        let t = theme::get();
        let ui = theme::font::UI_FAMILY;
        let label = format!("{n} changes");
        let arrow = |id: &'static str, icon: &'static str, tip: &'static str, next: bool| {
            div()
                .id(id)
                .size(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(t.bg_light))
                .tooltip(move |_w, cx| cx.new(|_| Tip(tip.into())).into())
                .child(svg().path(icon).size(px(14.0)).text_color(t.secondary_text))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.diff_nav_hunk(next, cx);
                    }),
                )
        };
        Some(
            div()
                .absolute()
                .top(px(8.0))
                // Clear the 12px vertical scrollbar gutter on the right edge.
                .right(px(16.0))
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                // Swallow clicks on the control (label + padding included) so they don't reach
                // the editor underneath and start a text selection.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .rounded_md()
                .border_1()
                .border_color(t.divider)
                .bg(t.bg_mid)
                .pl_2()
                .pr_1()
                .py_0p5()
                .font_family(ui)
                .text_size(px(t.ui_font_size))
                .text_color(t.secondary_text)
                .child(div().mr_1().child(label))
                .child(arrow(
                    "diff-prev",
                    "icons/arrow-up.svg",
                    "Previous change",
                    false,
                ))
                .child(arrow(
                    "diff-next",
                    "icons/arrow-down.svg",
                    "Next change",
                    true,
                ))
                .into_any_element(),
        )
    }

    /// A vertical scrollbar thumb overlaid on the right edge of the diff island, driven by
    /// `diff_scroll` (both panes share it). Returns `None` when the diff fits. Absolutely
    /// positioned — unlike `with_scrollbars` it doesn't need a concrete pane width, so it
    /// works over the diff's flex 50/50 split. `window` (when `Some`, i.e. the inline commit
    /// view) requests one settle frame so the bar appears on first paint, before any scroll;
    /// the modal diff passes `None` and the bar shows after the first scroll/repaint.
    fn diff_vscrollbar(
        &mut self,
        total_h: gpui::Pixels,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let t = theme::get();
        let scroll = self.diff_scroll.clone();
        // Viewport height is reliable from the shared handle (both panes are `h_full` in the
        // same island, so they paint the same visible height). The scroll *distance* is the
        // content height beyond the viewport — computed from `total_h`, not `max_offset`.
        let vp_h = scroll.bounds().size.height;
        let off = scroll.offset();
        let max_scroll = (total_h - vp_h).max(px(0.0));
        const BAR: f32 = 12.0;

        // Scroll metrics are zero until the panes have painted, so the first frame after a
        // file loads can't know whether a bar is needed. Track the painted dims and ask for
        // one more frame when they change, so the bar settles in without a scroll/resize.
        if let Some(window) = window {
            let dims: ScrollDims = (px(0.0), px(0.0), vp_h, px(0.0), total_h);
            if self.scroll_dims.get(&SbView::Diff) != Some(&dims) {
                self.scroll_dims.insert(SbView::Diff, dims);
                let entity = cx.entity();
                window.on_next_frame(move |_, cx| entity.update(cx, |_, cx| cx.notify()));
            }
        }

        if max_scroll <= px(1.0) {
            return None; // fits — nothing to scroll
        }

        const END: f32 = 8.0;
        const THUMB: f32 = 6.0;
        let (thumb_h, top) = scrollbar_thumb(
            f32::from(vp_h),
            f32::from(max_scroll),
            f32::from(off.y),
            END,
        );
        let m = (BAR - THUMB) / 2.0;
        let sc = scroll.clone();
        let thumb = div()
            .id("diff-sb-v")
            .absolute()
            .top(px(top))
            .left(px(m))
            .w(px(THUMB))
            .h(px(thumb_h))
            .rounded_full()
            .bg(t.line_number)
            .opacity(0.5)
            .hover(|s| s.opacity(0.85))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                    // Don't let the editor/tree underneath also handle this — it would start a
                    // text selection / move the caret behind the scrollbar.
                    cx.stop_propagation();
                    this.sb_drag = Some(crate::SbDrag {
                        handle: sc.clone(),
                        horizontal: false,
                        start_cursor: f32::from(e.position.y),
                        start_off: f32::from(sc.offset().y),
                    });
                    cx.notify();
                }),
            );
        Some(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(BAR))
                // Swallow clicks anywhere in the scrollbar gutter (track included) so they never
                // reach the editor underneath and start a text selection.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(thumb)
                .into_any_element(),
        )
    }

    /// Horizontal scrollbar overlaid at the bottom of one diff pane, driven by `scroll` (that
    /// pane's independent horizontal handle) and `content_w` (its widest line). Returns `None`
    /// when the lines fit. The caller overlays it on a `relative` pane wrapper.
    fn diff_hscrollbar(
        &mut self,
        scroll: &ScrollHandle,
        content_w: f32,
        view: SbView,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let t = theme::get();
        let vp_w = scroll.bounds().size.width;
        let off = scroll.offset();
        let max_scroll = px(content_w) - vp_w;
        const BAR: f32 = 12.0;
        if let Some(window) = window {
            let dims: ScrollDims = (vp_w, px(content_w), px(0.0), px(0.0), px(0.0));
            if self.scroll_dims.get(&view) != Some(&dims) {
                self.scroll_dims.insert(view, dims);
                let entity = cx.entity();
                window.on_next_frame(move |_, cx| entity.update(cx, |_, cx| cx.notify()));
            }
        }
        if max_scroll <= px(1.0) {
            return None; // lines fit — no horizontal scroll
        }
        const END: f32 = 8.0;
        const THUMB: f32 = 6.0;
        let (thumb_w, left) = scrollbar_thumb(
            f32::from(vp_w),
            f32::from(max_scroll),
            f32::from(off.x),
            END,
        );
        let m = (BAR - THUMB) / 2.0;
        let sc = scroll.clone();
        let id = if matches!(view, SbView::DiffLeftH) {
            "diff-sb-h-l"
        } else {
            "diff-sb-h-r"
        };
        let thumb = div()
            .id(id)
            .absolute()
            .left(px(left))
            .top(px(m))
            .h(px(THUMB))
            .w(px(thumb_w))
            .rounded_full()
            .bg(t.line_number)
            .opacity(0.5)
            .hover(|s| s.opacity(0.85))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.sb_drag = Some(crate::SbDrag {
                        handle: sc.clone(),
                        horizontal: true,
                        start_cursor: f32::from(e.position.x),
                        start_off: f32::from(sc.offset().x),
                    });
                    cx.notify();
                }),
            );
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(BAR))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(thumb)
                .into_any_element(),
        )
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

    /// Shared tree row used by Browse, Commit, and Rollback — so items look identical
    /// everywhere. `checkbox: None` = no checkbox; `Some(checked)` shows one. The three
    /// closures wire per-site behavior (activate / toggle-check / context-menu).
    #[allow(clippy::too_many_arguments)]
    fn tree_row(
        &self,
        cx: &mut Context<Self>,
        path: &std::path::Path,
        is_dir: bool,
        expanded: bool,
        depth: usize,
        selected: bool,
        name: SharedString,
        name_color: gpui::Rgba,
        checkbox: Option<bool>,
        on_activate: impl Fn(&mut Self, &gpui::MouseDownEvent, &mut Window, &mut Context<Self>)
            + 'static,
        on_check: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        on_context: impl Fn(&mut Self, gpui::Point<Pixels>, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let indent = px(8.0 + depth as f32 * 14.0);

        // Caret column (chevron for dirs, empty spacer for files) — fixed width so the
        // checkbox/badge/name columns align across rows.
        let caret = div()
            .w(px(16.0))
            .flex_none()
            .text_color(t.line_number)
            // Larger than the tree's text size for a chunkier chevron.
            .text_size(px(theme::get().ui_font_size + 5.0))
            .when(is_dir, |d| d.child(if expanded { "▾" } else { "▸" }));

        // A real drawn checkbox (rounded square; filled with a check svg when ticked),
        // placed AFTER the caret. Its own click toggles, without firing the row.
        let checkbox_el = checkbox.map(|checked| {
            let mut b = div()
                .flex_none()
                .size(px(15.0))
                .mr(px(6.0))
                .rounded_sm()
                .border_1()
                .flex()
                .items_center()
                .justify_center();
            b = if checked {
                b.bg(t.primary).border_color(t.primary).child(
                    svg()
                        .path("icons/check.svg")
                        .size(px(11.0))
                        .text_color(t.primary_text),
                )
            } else {
                b.border_color(t.line_number)
            };
            b.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| {
                    cx.stop_propagation();
                    on_check(this, cx);
                }),
            )
        });

        let badge = div()
            .w(px(22.0))
            .flex_none()
            .mr(px(4.0))
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_end()
            .child(if is_dir {
                svg()
                    .path("icons/folder.svg")
                    .size(px(16.0))
                    .text_color(gpui::rgb(0x9AA0A6))
                    .into_any_element()
            } else {
                badge_inner(file_badge(path), 2.0)
            });

        let content = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_1()
            .min_w_0()
            .pl(indent)
            .child(caret)
            .when_some(checkbox_el, |d, cb| d.child(cb))
            .child(badge)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(name_color)
                    .child(name),
            );

        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(38.0))
            // Set the row's font here (not on the container) so every tree — browse, commit,
            // rollback — renders identically regardless of which island hosts it.
            .text_size(px(theme::get().ui_font_size + 3.0))
            // Never let the flex column shrink a row: with many rows (root expanded) the
            // shrink would squash every row below its height, making a collapsed root's rows
            // look taller. `flex_none` keeps them a fixed height and lets the list scroll.
            .flex_none()
            .mx(px(6.0))
            .pr_1()
            .rounded_md()
            .cursor_pointer()
            .when(selected, |d| d.bg(t.selected_bg))
            // No hover tint on the active row (it would override its selected colour).
            .when(!selected && !self.dragging(Divider::Tree), |d| {
                d.hover(|d| d.bg(t.bg_mid))
            })
            .child(content)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseDownEvent, w, cx| {
                    on_activate(this, e, w, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                    on_context(this, e.position, cx)
                }),
            )
            .into_any_element()
    }

    /// The flattened Browse tree rows: the repo root, its expanded descendants (only when
    /// the root is open), then a virtual "Scratches" folder + its files at the bottom.
    /// Depths are offset by one so everything nests under the root row. The "Scratches"
    /// sentinel path can't collide with a real file.
    fn browse_visible_rows(&self) -> Vec<tree::Row> {
        let mut visible = vec![tree::Row {
            path: PathBuf::new(),
            is_dir: true,
            depth: 0,
        }];
        if self.expanded.contains(&PathBuf::new()) {
            for mut r in self.file_tree.visible(&self.expanded) {
                r.depth += 1;
                visible.push(r);
            }
        }
        let scratch_group = scratch_group_path();
        if !self.scratches.is_empty() {
            visible.push(tree::Row {
                path: scratch_group.clone(),
                is_dir: true,
                depth: 0,
            });
            if self.expanded.contains(&scratch_group) {
                for s in &self.scratches {
                    visible.push(tree::Row {
                        path: s.clone(),
                        is_dir: false,
                        depth: 1,
                    });
                }
            }
        }
        visible
    }

    /// The Browse right-hand pane: the no-file shortcuts screen when no tab is open,
    /// otherwise the tab bar + optional install/find banners + the editor area (inline
    /// image preview, font view, code editor, or the editor | markdown-preview split).
    fn render_editor_pane(
        &mut self,
        ui: &'static str,
        fs: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let right = if self.open_tabs.is_empty() {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .child(self.render_no_file(ui, cx))
        } else {
            let image = self.open_path.clone().filter(|p| is_image(p));
            let editor_area = if let Some(rel) = &image {
                // Inline image preview, centered, scaled to fit the pane.
                let abs = self
                    .repo_root
                    .as_ref()
                    .map(|r| r.join(rel))
                    .unwrap_or_else(|| rel.clone());
                div()
                    .id("image-scroll")
                    .overflow_scroll()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_4()
                    .child(img(abs).max_w_full().max_h_full())
                    .into_any_element()
            } else if self.open_path.as_ref().is_some_and(|p| is_font_file(p)) {
                self.render_font_view(cx).into_any_element()
            } else {
                // Outer flex item is `flex_1 min_w_0 overflow_hidden` — it shrinks to its flex
                // share regardless of the (content-wide) editor element inside. The INNER
                // `overflow_scroll` div holds the wide editor and scrolls it. Decoupling the
                // two is what lets a side-by-side pane (markdown preview) keep its 50%: a
                // content-wide element directly on the flex item refuses to shrink below it.
                // Single scroll div holding the content-wide editor element — the explicit width
                // (set in `editor_with_scrollbars`) goes on THIS div, the same one that has
                // `overflow_scroll` and the editor as a direct child. (An extra wrapper level
                // around it lets the content's min-width win and the pane refuses to shrink.)
                // The editor sits inside an explicit-width wrapper (its content width) so long
                // lines overflow this `overflow_scroll` viewport and the horizontal scrollbar
                // appears; `min_h_full` keeps the click target covering short files.
                let content_w = self.file_editor.read(cx).content_width();
                let editor_pane = div()
                    .id("editor-scroll")
                    .overflow_scroll()
                    .track_scroll(&self.file_scroll)
                    // A click in the empty area below a short file forwards to the editor,
                    // so the caret jumps to the last line (the editor consumes its own text
                    // clicks via `stop_propagation`, so this only fires for the gap).
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                            let fe = this.file_editor.clone();
                            fe.update(cx, |ed, cx| {
                                ed.click_at(e.position, e.modifiers.shift, window, cx)
                            });
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(content_w))
                            .min_h_full()
                            .child(self.file_editor.clone()),
                    );
                let island_w = self.editor_island_w(window);
                let editor = self.with_scrollbars(
                    editor_pane,
                    self.file_scroll.clone(),
                    island_w,
                    SbView::Editor,
                    window,
                    cx,
                );
                // Markdown + the markdown plugin installed → editor | live preview side by side.
                let md = self
                    .open_path
                    .as_ref()
                    .is_some_and(|p| matches!(Lang::from_path(p), Lang::Markdown))
                    && self.plugins.is_installed("markdown");
                if md {
                    let text = self.file_editor.read(cx).text().to_string();
                    // Two side-by-side panes, each with its own reusable scrollbars: the code
                    // editor (left, `md_editor_w` wide, drag-resizable divider) and the rendered
                    // preview (right, the remaining width). Each is wrapped in an explicit-width
                    // container so the split keeps its proportions.
                    let preview_w = (island_w - self.md_editor_w - theme::FRAME_GAP).max(120.0);
                    let code_pane = div()
                        .id("md-editor-scroll")
                        .overflow_scroll()
                        .track_scroll(&self.md_editor_scroll)
                        .child(
                            div()
                                .flex_none()
                                .w(px(content_w))
                                .min_h_full()
                                .child(self.file_editor.clone()),
                        );
                    // `flex flex_col` is load-bearing: the `with_scrollbars` output is `flex_1`,
                    // which only resolves to the pane height when its parent is a flex column.
                    // Without it the pane grew to content height → never overflowed → no thumb.
                    let left = div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .w(px(self.md_editor_w))
                        .h_full()
                        .min_h_0()
                        .child(self.with_scrollbars(
                            code_pane,
                            self.md_editor_scroll.clone(),
                            self.md_editor_w,
                            SbView::MdEditor,
                            window,
                            cx,
                        ));
                    // Persistent selectable rendered-markdown view (keeps its text selection
                    // across frames); re-parses only when the source actually changes.
                    // Directory of the open markdown file, for resolving relative image paths.
                    let base_dir = self.open_path.as_ref().and_then(|p| {
                        self.repo_root
                            .as_ref()
                            .map(|r| r.join(p))
                            .and_then(|abs| abs.parent().map(|d| d.to_path_buf()))
                    });
                    let mv = self
                        .md_view
                        .get_or_insert_with(|| {
                            cx.new(|cx| mdview::MarkdownView::new(&text, base_dir.clone(), cx))
                        })
                        .clone();
                    mv.update(cx, |v, cx| v.set_text(&text, base_dir.clone(), cx));
                    let preview_pane = div()
                        .id("md-preview-scroll")
                        .overflow_y_scroll()
                        .track_scroll(&self.md_preview_scroll)
                        .child(mv);
                    let right = div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .w(px(preview_w))
                        .h_full()
                        .min_h_0()
                        .child(self.with_scrollbars(
                            preview_pane,
                            self.md_preview_scroll.clone(),
                            preview_w,
                            SbView::MdPreview,
                            window,
                            cx,
                        ));
                    div()
                        .id("md-split")
                        .flex_1()
                        .flex()
                        .flex_row()
                        .min_h_0()
                        .child(left)
                        .child(
                            div()
                                .id("md-divider")
                                .w(px(theme::FRAME_GAP))
                                .flex_none()
                                .h_full()
                                .cursor_col_resize()
                                .bg(theme::get().divider)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                                        this.start_divider_drag(
                                            Divider::MdSplit,
                                            e.position,
                                            window,
                                        );
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(right)
                        .into_any_element()
                } else {
                    editor
                }
            };
            // Right-click in the editor pane shows git commands only (Commit / Rollback /
            // Push) for the open file — no file-management items.
            let menu_path = self.open_path.clone();
            let editor_area = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(editor_area)
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                        if let Some(p) = menu_path.clone() {
                            this.open_menu(e.position, MenuTarget::EditorGit(p), cx);
                        }
                    }),
                );
            let mut r = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                // Without min_w_0 the wide editor content (long code lines) lets this column
                // grow past the island, and the island's overflow_hidden then clips its right
                // edge — which is exactly where the tab-bar's trailing controls live, so the
                // `▾` overflow button vanished once tabs/content got wide.
                .min_w_0()
                .child(self.render_tab_bar(ui, fs, cx));
            // Install banner only applies to text/code files, not image previews.
            if image.is_none() {
                if let Some(pack) = self.pending_pack() {
                    r = r.child(self.render_install_banner(pack, ui, fs, cx));
                }
                if self.find_open {
                    r = r.child(self.render_find_bar(ui, cx));
                }
            }
            r.child(editor_area)
        };
        right.into_any_element()
    }

    fn render_browse(
        &mut self,
        ui: &'static str,
        fs: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // Row-hover suppression during a tree resize now lives in `tree_row` (it reads
        // `dragging(Divider::Tree)`), so the cursor sweeping over rows mid-drag doesn't flicker.
        // Show the repo root as the top row; everything else nests one level under it,
        // and collapsing the root hides the whole tree.
        let root_name = self
            .repo_root
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        let visible = self.browse_visible_rows();
        let scratch_group = scratch_group_path();
        // O(1) status lookup per row (was `self.files.iter().find()` — O(changed) per row,
        // i.e. O(rows×changed) for the whole tree on every frame).
        let status_by_path: std::collections::HashMap<&PathBuf, FileStatus> =
            self.files.iter().map(|f| (&f.path, f.status)).collect();
        let rows: Vec<gpui::AnyElement> = visible
            .into_iter()
            .map(|r| {
                let is_root = r.path.as_os_str().is_empty();
                let name: SharedString = if is_root {
                    root_name.clone().into()
                } else if r.path == scratch_group {
                    "Scratches".into()
                } else {
                    r.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into()
                };
                let expanded = self.expanded.contains(&r.path);
                let selected = self.selected_path.as_ref() == Some(&r.path);
                let (p_act, p_ctx) = (r.path.clone(), r.path.clone());
                let is_dir = r.is_dir;
                // Color changed files by their git status (modified/added/…), matching the
                // commit view; unchanged files use the normal text color.
                let name_color = (!is_dir)
                    .then(|| status_by_path.get(&r.path))
                    .flatten()
                    .map(|&s| status_color(s))
                    .unwrap_or(theme::get().text);
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
                    move |this, e, window, cx| {
                        // Folders toggle expansion. Files (VS Code-style): single click opens in
                        // the temporary *preview* tab (reused by the next single-click), double
                        // click opens a permanent tab.
                        this.selected_path = Some(p_act.clone());
                        // Focus the app root so the "Kyde"-context Backspace (delete) binding
                        // is live on the selected row. (open_file/preview_file re-focus the
                        // editor below, which is what we want there.)
                        window.focus(&this.focus_handle);
                        if is_dir {
                            this.toggle_dir(p_act.clone(), cx);
                        } else if e.click_count >= 2 {
                            this.open_file(p_act.clone(), cx);
                        } else {
                            this.preview_file(p_act.clone(), cx);
                        }
                        cx.notify();
                    },
                    |_this, _cx| {},
                    move |this, pos, cx| {
                        this.open_menu(pos, MenuTarget::BrowseFile(p_ctx.clone(), is_dir), cx);
                    },
                )
            })
            .collect();
        let t = theme::get();
        // Header strip with a `−` minimize button at the top-right.
        // Collapse button — absolutely positioned in the tree's top-right corner so it floats
        // over the rows instead of consuming a whole header line.
        let tree_minimize = div()
            .id("tree-minimize")
            .absolute()
            .top(px(4.0))
            // Clear the 12px scrollbar gutter on the right so the bar never sits under it.
            .right(px(8.0))
            .flex()
            .items_center()
            .justify_center()
            .size(px(22.0))
            .rounded_md()
            .text_size(px(16.0))
            .text_color(t.line_number)
            .bg(t.panel_bg)
            .hover(|s| s.bg(t.bg_light).text_color(t.text))
            .cursor_pointer()
            .child("−")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.tree_collapsed = true;
                    cx.notify();
                }),
            );
        let tree = div()
            .flex()
            .flex_col()
            .relative()
            .w(px(self.tree_width))
            .flex_none()
            .h_full()
            .py_1()
            .bg(t.panel_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .text_color(t.text)
            .font_family(ui)
            // A touch larger than the editor/code size for readability.
            .text_size(px(theme::get().ui_font_size + 3.0))
            .child({
                let rows_pane = div()
                    .id("browse-tree")
                    .overflow_y_scroll()
                    .track_scroll(&self.tree_scroll)
                    .flex()
                    .flex_col()
                    .children(rows);
                self.with_scrollbars(
                    rows_pane,
                    self.tree_scroll.clone(),
                    self.tree_width,
                    SbView::Tree,
                    window,
                    cx,
                )
            })
            .child(tree_minimize);
        // Collapsed: a thin strip with an expand button where the tree was.
        let collapsed_strip = div()
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
                    .id("tree-expand")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.0))
                    .rounded_md()
                    .text_size(px(15.0))
                    .text_color(t.line_number)
                    .hover(|s| s.bg(t.bg_light).text_color(t.text))
                    .cursor_pointer()
                    .child("»")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.tree_collapsed = false;
                            cx.notify();
                        }),
                    ),
            );

        // The frame-colored gap between the two islands doubles as the resize handle.
        // No hover/active tint — just the resize cursor.
        let divider = div()
            .id("browse-divider")
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

        // No tabs open → show useful shortcuts instead of the empty editor.
        let right = self.render_editor_pane(ui, fs, window, cx);

        let editor_island = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            // `relative` so the floated tab-overflow button anchors to the island's right
            // edge; `overflow_hidden` keeps it (and the tab strip) clipped to the island.
            .bg(theme::get().main_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .overflow_hidden()
            .child(right);

        let mut row = div().flex().flex_row().flex_1();
        row = if self.tree_collapsed {
            row.child(collapsed_strip)
                .child(div().w(px(theme::FRAME_GAP)).flex_none().h_full())
        } else {
            row.child(tree).child(divider)
        };
        row.child(editor_island).into_any_element()
    }

    /// Width of the editor island = window − rail − body right-pad − (tree + divider, or the
    /// collapsed strip + gap). Used to size the browse editor and the markdown split.
    fn editor_island_w(&self, window: &Window) -> f32 {
        let win_w = f32::from(window.viewport_size().width);
        let left_chrome = if self.tree_collapsed {
            RAIL_W + theme::FRAME_GAP + 30.0 + theme::FRAME_GAP
        } else {
            RAIL_W + theme::FRAME_GAP + self.tree_width + theme::FRAME_GAP
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
    fn with_scrollbars(
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

    fn render_font_view(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let t = theme::get();
        let base = div()
            .id("font-view")
            .overflow_y_scroll()
            .flex_1()
            .flex()
            .flex_col()
            .bg(t.main_bg);
        if !self.plugins.is_installed("font") {
            return base
                .items_center()
                .justify_center()
                .gap_3()
                .font_family(theme::font::UI_FAMILY)
                .child(
                    div()
                        .text_color(t.text)
                        .child("Install Font Preview support to view this file."),
                )
                .child(
                    btn_primary("install-font", "Install Font Preview").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.install_pack("font", cx)),
                    ),
                );
        }
        let Some((_, family)) = self.font_preview.clone() else {
            return base
                .items_center()
                .justify_center()
                .font_family(theme::font::UI_FAMILY)
                .child(
                    div()
                        .text_color(t.line_number)
                        .child("Could not read this font file."),
                );
        };
        let quote = "Me fail English? That's unpossible.";
        let fam = family.clone();
        let line = move |sz: f32| {
            div()
                .font_family(fam.clone())
                .text_size(px(sz))
                .text_color(t.text)
                .child(quote)
        };
        base.p_6()
            .gap_3()
            .child(
                div()
                    .font_family(theme::font::UI_FAMILY)
                    .text_color(t.line_number)
                    .text_size(px(12.0))
                    .child(family.clone()),
            )
            .child(line(13.0))
            .child(line(18.0))
            .child(line(24.0))
            .child(line(32.0))
            .child(line(48.0))
            .child(
                div()
                    .font_family(family)
                    .text_size(px(20.0))
                    .text_color(t.secondary_text)
                    .child("ABCDEFG abcdefg 0123456789 !?&@#"),
            )
    }

    /// Rendered Markdown preview (right pane of the side-by-side markdown view). Live —
    /// re-renders from the editor's current text each frame.
    // Superseded by the selectable `mdview::MarkdownView`, but kept (another change adds image
    // rendering here); left in place rather than deleted.
    #[allow(dead_code)]
    fn render_markdown_preview(&self, text: &str) -> gpui::Stateful<gpui::Div> {
        let t = theme::get();
        let mk_font = |bold: bool, italic: bool, code: bool| gpui::Font {
            family: if code {
                theme::font::FAMILY
            } else {
                theme::font::UI_FAMILY
            }
            .into(),
            features: Default::default(),
            fallbacks: None,
            weight: if bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if italic {
                gpui::FontStyle::Italic
            } else {
                gpui::FontStyle::Normal
            },
        };
        let styled = move |spans: &[markdown::Span], color: gpui::Hsla| -> gpui::StyledText {
            let mut s = String::new();
            let mut runs = Vec::new();
            for sp in spans {
                let c: gpui::Hsla = if sp.code {
                    gpui::rgb(0xC9CDD6).into()
                } else {
                    color
                };
                runs.push(gpui::TextRun {
                    len: sp.text.len(),
                    font: mk_font(sp.bold, sp.italic, sp.code),
                    color: c,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
                s.push_str(&sp.text);
            }
            gpui::StyledText::new(SharedString::from(s)).with_runs(runs)
        };
        let blocks = markdown::parse(text).into_iter().map(|b| match b {
            markdown::Block::Heading(level, spans) => {
                let size = match level {
                    1 => 26.0,
                    2 => 21.0,
                    3 => 18.0,
                    _ => 15.0,
                };
                div()
                    .pt_2()
                    .text_size(px(size))
                    .child(styled(&spans, t.text.into()))
                    .into_any_element()
            }
            markdown::Block::Paragraph(spans) => div()
                .text_size(px(14.0))
                .child(styled(&spans, t.text.into()))
                .into_any_element(),
            markdown::Block::Code(code) => div()
                .font_family(theme::font::FAMILY)
                .text_size(px(13.0))
                .text_color(t.text)
                .bg(t.bg_mid)
                .rounded_md()
                .p_3()
                .child(SharedString::from(code))
                .into_any_element(),
            markdown::Block::ListItem(depth, spans) => div()
                .flex()
                .flex_row()
                .pl(px(depth as f32 * 14.0))
                .text_size(px(14.0))
                .child(div().pr_2().text_color(t.line_number).child("•"))
                .child(styled(&spans, t.text.into()))
                .into_any_element(),
            markdown::Block::Quote(spans) => div()
                .border_l_2()
                .border_color(t.divider)
                .pl_3()
                .text_size(px(14.0))
                .child(styled(&spans, t.secondary_text.into()))
                .into_any_element(),
            markdown::Block::Rule => div().h(px(1.0)).bg(t.divider).my_2().into_any_element(),
            markdown::Block::Image { src, .. } => {
                // Remote (http/https) → loaded via gpui's asset loader, which needs an
                // HttpClient wired at startup — only present under the `remote-images`
                // feature; without it the fetch fails and nothing renders (no crash).
                // Local → resolved relative to the open file's directory, then the repo
                // root, and read straight off disk (no deps, always works).
                let source: gpui::ImageSource =
                    if src.starts_with("http://") || src.starts_with("https://") {
                        src.clone().into()
                    } else {
                        let rel = src.trim_start_matches("./");
                        let joined = match self.open_path.as_ref().and_then(|p| p.parent()) {
                            Some(dir) => dir.join(rel),
                            None => PathBuf::from(rel),
                        };
                        self.repo_root
                            .as_ref()
                            .map(|r| r.join(&joined))
                            .unwrap_or(joined)
                            .into()
                    };
                div()
                    .py_1()
                    .child(img(source).max_w_full().max_h(px(480.0)))
                    .into_any_element()
            }
        });
        div()
            .id("md-preview")
            .overflow_y_scroll()
            .track_scroll(&self.md_preview_scroll)
            .flex()
            .flex_col()
            .gap_2()
            .p_5()
            .bg(t.main_bg)
            .font_family(theme::font::UI_FAMILY)
            .children(blocks)
    }

    /// Find / replace bar shown atop the editor (cmd-f / cmd-r). Live-highlights matches;
    /// enter / cmd-g cycle, the buttons replace.
    fn render_find_bar(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let count = self.find_matches.len();
        let label = if count == 0 {
            "No results".to_string()
        } else {
            format!("{}/{}", self.find_idx + 1, count)
        };
        let input_box = |child: gpui::Entity<CodeEditor>| {
            div()
                .flex_1()
                .min_w_0()
                .h(px(26.0))
                .px_2()
                .flex()
                .items_center()
                .bg(t.main_bg)
                .border_1()
                .border_color(t.divider)
                .rounded_md()
                .child(child)
        };
        let btn = |glyph: &str, id: &'static str| {
            div()
                .id(SharedString::from(id))
                .size(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .text_color(t.secondary_text)
                .hover(|s| s.bg(t.bg_light).text_color(t.text))
                .cursor_pointer()
                .child(SharedString::from(glyph.to_string()))
        };

        let find_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(input_box(self.find_query.clone()))
            .child(
                div()
                    .flex_none()
                    .text_color(t.line_number)
                    .text_size(px(theme::get().ui_font_size - 1.0))
                    .child(label),
            )
            .child(btn("‹", "find-prev").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, w, cx| this.find_prev(&FindPrev, w, cx)),
            ))
            .child(btn("›", "find-next").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, w, cx| this.find_next(&FindNext, w, cx)),
            ))
            .child(btn("×", "find-close").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, w, cx| this.close_find(&CloseFind, w, cx)),
            ));

        let mut col = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1p5()
            .bg(t.panel_bg)
            .border_b_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size))
            .child(find_row);

        if self.find_replace {
            let replace_row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(input_box(self.replace_query.clone()))
                .child(
                    div()
                        .id("replace-one")
                        .px_2()
                        .h(px(24.0))
                        .flex()
                        .items_center()
                        .rounded_md()
                        .text_color(t.secondary_text)
                        .hover(|s| s.bg(t.bg_light).text_color(t.text))
                        .cursor_pointer()
                        .child("Replace")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, w, cx| this.replace_one(&ReplaceOne, w, cx)),
                        ),
                )
                .child(
                    div()
                        .id("replace-all")
                        .px_2()
                        .h(px(24.0))
                        .flex()
                        .items_center()
                        .rounded_md()
                        .text_color(t.secondary_text)
                        .hover(|s| s.bg(t.bg_light).text_color(t.text))
                        .cursor_pointer()
                        .child("All")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, w, cx| this.replace_all(&ReplaceAll, w, cx)),
                        ),
                );
            col = col.child(replace_row);
        }
        col.into_any_element()
    }

    /// Editor tab strip: one tab per open file, left→right in open order. Click activates,
    /// the `×` closes, right-click opens the tab context menu (close / others / to the right).
    /// Project tabs strip: one pill per open project, above all other UI (under the title
    /// bar). Click switches projects; the `×` closes one. Rendered only when >1 is open.
    fn render_project_tabs(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let tabs = self.open_projects.iter().enumerate().map(|(i, root)| {
            let active = self.repo_root.as_ref() == Some(root);
            let name: SharedString = crate::projects::name_of(root).into();
            let grp = SharedString::from(format!("projgrp-{i}"));
            let close = div()
                .id(SharedString::from(format!("project-close-{i}")))
                .flex_none()
                .w(px(18.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .text_size(px(15.0))
                .text_color(t.line_number)
                .hover(|d| d.bg(t.bg_light).text_color(t.text))
                .when(!active, |d| {
                    d.opacity(0.0).group_hover(grp.clone(), |s| s.opacity(1.0))
                })
                .child("×")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener({
                        let root = root.clone();
                        move |this, _e, _w, cx| {
                            cx.stop_propagation();
                            this.close_project(root.clone(), cx);
                        }
                    }),
                );
            div()
                .id(SharedString::from(format!("project-tab-{i}")))
                .group(grp.clone())
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .h(px(28.0))
                .flex_none()
                .max_w(px(220.0))
                .rounded_md()
                .border_1()
                .cursor_pointer()
                .when(active, |d| {
                    d.bg(gpui::rgba(0x3574F026)).border_color(t.primary)
                })
                .when(!active, |d| {
                    d.border_color(gpui::rgba(0x00000000))
                        .hover(|d| d.bg(t.bg_mid))
                })
                .text_color(if active { t.text } else { t.line_number })
                .child(div().min_w_0().truncate().child(name))
                .child(close)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener({
                        let root = root.clone();
                        move |this, _e, _w, cx| {
                            if this.repo_root.as_ref() != Some(&root) {
                                this.open_project(root.clone(), cx);
                            }
                        }
                    }),
                )
        });
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .flex_none()
            .w_full()
            .px_2()
            .pb(px(theme::FRAME_GAP))
            .bg(t.frame_bg)
            .font_family(ui)
            .text_size(px(t.ui_font_size))
            .children(tabs)
            .into_any_element()
    }

    fn render_tab_bar(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let dirty = self.file_editor.read(cx).dirty;
        let tabs = self.open_tabs.iter().enumerate().map(|(i, p)| {
            let active = self.open_path.as_ref() == Some(p);
            // The preview (temporary) tab renders in italics, like VS Code.
            let preview = self.preview_tab.as_ref() == Some(p);
            let name: SharedString = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into();
            // Share the git text-status color (modified/added/…) like the tree rows.
            let status_col = self
                .files
                .iter()
                .find(|f| &f.path == p)
                .map(|f| status_color(f.status));
            let icon = div().flex_none().child(badge_inner(file_badge(p), 0.0));
            // Active+dirty → a dot in place of the close affordance; otherwise an `×`.
            let grp = SharedString::from(format!("tabgrp-{i}"));
            let close = div()
                .id(SharedString::from(format!("tab-close-{i}")))
                .flex_none()
                .w(px(18.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .text_size(px(15.0))
                .text_color(t.line_number)
                .hover(|d| d.bg(t.bg_light).text_color(t.text))
                // Inactive tabs hide the close until the tab is hovered.
                .when(!active, |d| {
                    d.opacity(0.0).group_hover(grp.clone(), |s| s.opacity(1.0))
                })
                .child(if active && dirty { "●" } else { "×" })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.close_tab(i, cx);
                    }),
                );
            // Rounded pill per tab: active = accent border + faint accent fill; inactive =
            // transparent (no bg/border) until hovered. border_1 stays so widths don't shift.
            div()
                .id(SharedString::from(format!("tab-{i}")))
                .group(grp.clone())
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .h(px(28.0))
                .flex_none()
                .rounded_md()
                .border_1()
                .cursor_pointer()
                // Preview (temporary) tab → the same outline + translucent-fill treatment as an
                // active tab, but in grey instead of the accent blue, so it reads as "tentative".
                // A permanent active tab keeps the blue; inactive permanent tabs are bare.
                .when(preview, |d| {
                    d.bg(gpui::rgba(0x8A909022))
                        .border_color(gpui::rgb(0x6B7079))
                })
                .when(active && !preview, |d| {
                    d.bg(gpui::rgba(0x3574F026)).border_color(t.primary)
                })
                .when(!active && !preview, |d| {
                    d.border_color(gpui::rgba(0x00000000))
                        .hover(|d| d.bg(t.bg_mid))
                })
                .text_color(if active { t.text } else { t.line_number })
                .child(icon)
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .when_some(status_col, |d, c| d.text_color(c))
                        .child(name),
                )
                .child(close)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        if let Some(p) = this.open_tabs.get(i).cloned() {
                            this.open_file(p, cx);
                            cx.notify();
                        }
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                        this.open_menu(e.position, MenuTarget::Tab(i), cx);
                    }),
                )
        });

        // The `▾` overflow chooser is NOT rendered here — it's floated on the editor island
        // (see `render_browse`) so it's pinned to the island's right edge and stays visible
        // however wide the tab strip grows. We only reserve room for it on the right (`pr`)
        // so the last tab can scroll out from under it.
        div()
            .id("tab-bar")
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .h(px(38.0))
            .bg(t.panel_bg)
            // Match the editor island's top corners (gpui clips rectangular, so the
            // top strip must round itself or it squares off the island corners).
            .rounded_t(px(theme::ISLAND_RADIUS))
            .border_b_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(fs)
            .child(
                div()
                    .id("tabs-scroll")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    // Keep the last tab clear of the button's solid right edge; the rest of
                    // the button is a transparent fade tabs can scroll under.
                    .pr(px(34.0))
                    .flex_1()
                    .min_w_0()
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scroll)
                    // A plain mouse has only a vertical wheel; map it to horizontal so the
                    // tab strip scrolls. Native horizontal (trackpad) stays with overflow_x.
                    .on_scroll_wheel(cx.listener(|this, e: &gpui::ScrollWheelEvent, _w, cx| {
                        let d = e.delta.pixel_delta(px(18.0));
                        if d.y.abs() > px(0.0) {
                            let mut off = this.tab_scroll.offset();
                            off.x += d.y;
                            this.tab_scroll.set_offset(off);
                            cx.notify();
                        }
                    }))
                    .children(tabs),
            )
            .into_any_element()
    }

    /// The `▾` tab-overflow chooser, floated absolutely at the top-right of the editor
    /// island so it's pinned to the island's right edge and stays visible no matter how
    /// wide the tab strip grows (the strip can overflow + scroll under it). Click → a
    /// dropdown listing every open tab. Rendered only when tabs are open.
    fn render_tab_overflow_button(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        // Tabs scroll *under* this; a left→right fade (transparent → panel) dissolves them
        // into the button instead of a hard bordered box, so it reads as part of the strip.
        let fade_from = gpui::Rgba {
            a: 0.0,
            ..t.panel_bg
        };
        // Positioned at the ROOT level (not inside the editor island) so it paints ABOVE the
        // tab strip's scroll layer — a child of the island was drawn *under* the scrolling
        // tabs and vanished once they filled the right edge. Coords match the tab bar: y =
        // titlebar(40) + body top pad(FRAME_GAP); x = body right pad(FRAME_GAP).
        div()
            .absolute()
            .top(px(40.0 + theme::FRAME_GAP))
            .right(px(theme::FRAME_GAP))
            // Above the tab bar's 1px bottom border; match its rounded top-right corner.
            .h(px(37.0))
            .rounded_tr(px(theme::ISLAND_RADIUS))
            .flex()
            .items_center()
            .justify_end()
            .w(px(56.0))
            .pr(px(6.0))
            .occlude()
            .bg(gpui::linear_gradient(
                90.0,
                gpui::linear_color_stop(fade_from, 0.0),
                gpui::linear_color_stop(t.panel_bg, 0.55),
            ))
            .child(
                div()
                    .id("tabs-overflow")
                    .size(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .flex_none()
                    .cursor_pointer()
                    .hover(|d| d.bg(t.bg_mid))
                    .child(
                        svg()
                            .path("icons/chevron-down.svg")
                            .size(px(15.0))
                            .text_color(t.line_number),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &gpui::MouseDownEvent, _w, cx| {
                            // Drop the dropdown down-left of the cursor (panel ≥180px wide,
                            // button hugs the right edge) so it never lands off-screen-right.
                            let at = gpui::point(
                                (e.position.x - px(180.0)).max(px(8.0)),
                                e.position.y + px(8.0),
                            );
                            this.open_menu(at, MenuTarget::TabList, cx);
                        }),
                    ),
            )
            .into_any_element()
    }

    /// Shown in the editor pane when no file is open: the handful of shortcuts that
    /// actually get you somewhere (keys reflect the active keymap).
    fn render_no_file(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let key = |name: &str| {
            self.keymap
                .key_for(name)
                .map(|k| pretty_key(&k))
                .unwrap_or_default()
        };
        let row = |label: &'static str, accel: String| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(div().text_color(t.text).child(label))
                .child(
                    div()
                        .text_color(t.line_number)
                        .child(SharedString::from(accel)),
                )
        };

        let _ = cx;
        div()
            .flex()
            .flex_col()
            .flex_1()
            .gap_4()
            .px_12()
            .justify_center()
            // No bg: the rounded editor island behind provides the surface, so the
            // panel's corners stay rounded (gpui clips rectangular, not rounded).
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size + 2.0))
            .child(row("Go to File", key("go_to_file")))
            .child(row("Commit", key("commit")))
            .child(row("Commit view", key("mode_commit")))
            .child(row("Keymap / Settings", key("open_keymap")))
            .child(
                div()
                    .text_color(t.line_number)
                    .child("Select a file from the tree to open it"),
            )
            .child(
                div()
                    .text_color(t.line_number)
                    .child("Right-click a file to Commit or Rollback"),
            )
            .into_any_element()
    }

    /// Top-of-editor prompt offering to install syntax support for the open file.
    /// IntelliJ-style: a thin bar with a primary (#3473EE) Install button.
    /// Top-of-window banner shown only when a newer release exists. The action is
    /// "Update & Relaunch" when running from a `.app` bundle (downloads + swaps in place),
    /// else "Download" (opens the release page) — see `do_update`.
    fn render_update_banner(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let Some(rel) = self.update_available.as_ref() else {
            return div().into_any_element();
        };
        let can_swap = update::running_bundle().is_some() && !rel.zip_url.is_empty();
        let action_label = if self.updating {
            "Updating…"
        } else if can_swap {
            "Update & Relaunch"
        } else {
            "Download"
        };
        let msg: SharedString = format!("Update available — v{}", rel.version).into();

        // ↑ badge
        let badge = div()
            .flex_none()
            .size(px(18.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(t.primary)
            .child(
                div()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(gpui::white())
                    .child("↑"),
            );

        let updating = self.updating;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .bg(t.bg_mid)
            .border_b_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(px(t.ui_font_size))
            .text_color(t.text)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(badge)
                    .child(msg),
            )
            // Spacer pushes the actions to the right.
            .child(div().flex_1().min_w_0())
            .child(
                btn_primary("update-now", action_label)
                    .when(updating, |d| d.opacity(0.6))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.do_update(cx)),
                    ),
            )
            .child(btn_secondary("update-dismiss", "Dismiss").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.dismiss_update(cx)),
            ))
            .into_any_element()
    }

    fn render_install_banner(
        &self,
        pack: &'static highlight::Pack,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let ext = self
            .open_path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("");

        // ⓘ info badge
        let info = div()
            .flex_none()
            .size(px(18.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(t.primary)
            .child(
                div()
                    .text_size(px(11.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(gpui::white())
                    .child("i"),
            );

        let link = |label: SharedString, id: &'static str| {
            div()
                .id(id)
                .flex_none()
                .text_color(t.primary)
                .cursor_pointer()
                .hover(|s| s.text_color(t.text))
                .child(label)
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_4()
            .px_3()
            .py_2()
            .bg(t.bg_mid)
            .border_b_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(fs)
            .text_color(t.text)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(info)
                    .child(SharedString::from(format!("Plugins supporting *.{ext}"))),
            )
            .child(
                link(
                    format!("Install {} plugin", pack.name).into(),
                    "install-plugin",
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        this.install_open_pack(cx);
                        cx.notify();
                    }),
                ),
            )
            .child(link("Ignore extension".into(), "ignore-ext").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.ignore_open_pack(cx)),
            ))
            .into_any_element()
    }

    fn render_finder(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        // gpui only tracks hover on *identified* elements, so each row needs an `.id()` or the
        // `.hover(...)` style never applies (the tree rows that hover all carry an id).
        let row_base = |i: usize, sel: bool| {
            div()
                .id(("finder-row", i))
                .flex()
                .flex_row()
                .items_center()
                // Inset the rounded pill 2px from the panel edges so the hover/selected
                // background doesn't run into the corners.
                .mx(px(2.0))
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .text_color(t.text)
                .when(sel, |d| d.bg(t.selected_bg))
                // The finder panel itself is `bg_mid`, so hover must be a lighter shade
                // (`bg_light`) to be visible — `bg_mid` on `bg_mid` shows nothing.
                .when(!sel, |d| d.hover(|s| s.bg(t.bg_light)))
        };
        let rows: Vec<gpui::AnyElement> = match self.finder_mode {
            FinderMode::Files => self
                .finder_results
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let sel = i == self.finder_selected;
                    let name: SharedString = p.to_string_lossy().into_owned().into();
                    let pc = p.clone();
                    let icon = div()
                        .w(px(20.0))
                        .flex_none()
                        .mr(px(8.0))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_end()
                        .child(badge_inner(file_badge(p), 0.0));
                    row_base(i, sel)
                        .child(icon)
                        .child(div().min_w_0().truncate().child(name))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, window, cx| {
                                this.open_file(pc.clone(), cx);
                                this.mode = Mode::Browse;
                                this.finder_open = false;
                                window.focus(&this.focus_handle);
                                cx.notify();
                            }),
                        )
                        .into_any_element()
                })
                .collect(),
            FinderMode::Content => self
                .content_results
                .iter()
                .enumerate()
                .map(|(i, hit)| {
                    let sel = i == self.finder_selected;
                    let path = hit.path.clone();
                    let line = hit.line;
                    let loc: SharedString =
                        format!("{}:{}", hit.path.to_string_lossy(), hit.line).into();
                    // The matched line, trimmed + capped so a huge minified line can't blow up.
                    let snippet: SharedString = hit
                        .text
                        .trim_start()
                        .chars()
                        .take(200)
                        .collect::<String>()
                        .into();
                    let icon = div()
                        .w(px(20.0))
                        .flex_none()
                        .mr(px(8.0))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_end()
                        .child(badge_inner(file_badge(&path), 0.0));
                    row_base(i, sel)
                        .child(icon)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(theme::font::FAMILY)
                                .child(snippet),
                        )
                        .child(
                            div()
                                .flex_none()
                                .ml(px(10.0))
                                .max_w(px(220.0))
                                .truncate()
                                .text_color(t.line_number)
                                .child(loc),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, window, cx| {
                                this.finder_open = false;
                                this.open_file_at_line(path.clone(), line, window, cx);
                            }),
                        )
                        .into_any_element()
                })
                .collect(),
            FinderMode::Actions => self
                .action_results
                .iter()
                .enumerate()
                .map(|(row, &ai)| {
                    let (label, kind, key_action) = PALETTE[ai];
                    let sel = row == self.finder_selected;
                    // Right-aligned shortcut chip, when this action has a bound key.
                    let shortcut = (!key_action.is_empty())
                        .then(|| self.keymap.key_for(key_action))
                        .flatten()
                        .map(|k| {
                            div()
                                .px_2()
                                .bg(t.bg_mid)
                                .rounded_md()
                                .text_color(t.line_number)
                                .child(SharedString::from(pretty_key(&k)))
                        });
                    row_base(row, sel)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(label)),
                        )
                        .children(shortcut)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, window, cx| {
                                this.run_palette(kind, window, cx)
                            }),
                        )
                        .into_any_element()
                })
                .collect(),
            FinderMode::Scratch => self
                .action_results
                .iter()
                .enumerate()
                .map(|(row, &li)| {
                    let (label, ext) = scratch::LANGS[li];
                    let sel = row == self.finder_selected;
                    let icon = div()
                        .w(px(20.0))
                        .flex_none()
                        .mr(px(8.0))
                        .flex()
                        .items_center()
                        .justify_end()
                        .child(badge_inner(
                            file_badge(std::path::Path::new(&format!("x.{ext}"))),
                            0.0,
                        ));
                    row_base(row, sel)
                        .child(icon)
                        .child(SharedString::from(label))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| this.create_scratch(ext, cx)),
                        )
                        .into_any_element()
                })
                .collect(),
        };

        let modal = div()
            .key_context("FileFinder")
            .on_action(cx.listener(Self::finder_up))
            .on_action(cx.listener(Self::finder_down))
            .on_action(cx.listener(Self::finder_confirm))
            .on_action(cx.listener(Self::finder_close))
            .w(px(640.0))
            .max_h(px(440.0))
            .flex()
            .flex_col()
            .bg(theme::get().bg_mid)
            .border_1()
            .border_color(theme::get().bg_light)
            .rounded_md()
            .shadow_lg()
            .font_family(ui)
            .text_size(fs)
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(theme::get().divider)
                    .child(self.finder_query.clone()),
            )
            .when(
                matches!(self.finder_mode, FinderMode::Content)
                    && !self.finder_query.read(cx).text().is_empty(),
                |d| {
                    let n = self.content_results.len();
                    let files = self
                        .content_results
                        .iter()
                        .map(|h| &h.path)
                        .collect::<std::collections::HashSet<_>>()
                        .len();
                    let label: SharedString = if n == 0 {
                        "No matches".into()
                    } else {
                        format!(
                            "{n} match{} in {files} file{}",
                            if n == 1 { "" } else { "es" },
                            if files == 1 { "" } else { "s" }
                        )
                        .into()
                    };
                    d.child(
                        div()
                            .px_3()
                            .py_1()
                            .border_b_1()
                            .border_color(t.divider)
                            .text_color(t.line_number)
                            .child(label),
                    )
                },
            )
            .child(
                div()
                    .id("finder-results")
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    // 2px vertical gap between the rounded rows, matching their 2px side inset.
                    .gap(px(2.0))
                    .children(rows),
            )
            // Clicks inside the panel must not reach the backdrop (which would close the finder
            // right after a row's handler ran — breaking palette actions that re-open a finder).
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

        overlay(cx, true).child(modal).into_any_element()
    }

    /// The language-plugin manager: every installable pack with its monogram badge,
    /// approximate compiled footprint, install state, and an Install/Uninstall button.
    /// Filtered live by the search box.
    /// Body of the Language-Plugins modal window (hosted by `ModalWindow`, native titlebar).
    pub(crate) fn render_plugins_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        let q = self.plugins_query.read(cx).text().to_lowercase();
        let q = q.trim();
        let mut packs: Vec<_> = highlight::PACKS
            .iter()
            .filter(|p| q.is_empty() || p.name.to_lowercase().contains(q) || p.id.contains(q))
            .collect();
        packs.sort_by_key(|p| p.name.to_lowercase());
        let rows: Vec<gpui::AnyElement> = packs
            .into_iter()
            .map(|p| {
                let installed = self.plugins.is_installed(p.id);
                let id = p.id;
                // Larger badge: the rows are two lines tall (name + size), so size the
                // language monogram to roughly match.
                let badge = badge_inner(
                    file_badge(std::path::Path::new(&format!("x.{}", pack_ext(p.id)))),
                    14.0,
                );
                let bid = SharedString::from(format!("pkg-{id}"));
                let btn = if installed {
                    btn_secondary(bid, "Uninstall")
                } else {
                    btn_primary(bid, "Install")
                }
                .flex_none()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.toggle_plugin(id, cx)),
                );
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .hover(|s| s.bg(t.bg_mid))
                    .child(
                        div()
                            .w(px(40.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(badge),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_color(t.text).child(SharedString::from(p.name)))
                            .child(
                                div()
                                    .text_color(t.line_number)
                                    .text_size(px(11.0))
                                    .child(SharedString::from(pack_size(p.id))),
                            ),
                    )
                    .child(btn)
                    .into_any_element()
            })
            .collect();

        // Fills the modal window (chrome + native titlebar come from `ModalWindow`).
        div()
            .size_full()
            .flex()
            .flex_col()
            .font_family(ui)
            .child(div().px_2().py_2().child(self.plugins_query.clone()))
            // Divider under the search input.
            .child(div().h(px(1.0)).bg(t.divider))
            .child(
                div()
                    .id("plugins-list")
                    .overflow_y_scroll()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .p_1()
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_onboarding(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let preset_card = |preset: Preset, cx: &mut Context<Self>| {
            let selected = self.onboarding_choice == preset;
            let sample: Vec<gpui::AnyElement> = keymap::ACTIONS
                .iter()
                .map(|a| {
                    let key = match preset {
                        Preset::VSCode => a.vscode,
                        _ => a.webstorm,
                    };
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .gap_4()
                        .child(SharedString::from(a.label))
                        .child(
                            div()
                                .px_2()
                                .bg(theme::get().bg_mid)
                                .rounded_md()
                                .text_color(theme::get().line_number)
                                .child(SharedString::from(pretty_key(key))),
                        )
                        .into_any_element()
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .gap_2()
                .w(px(300.0))
                .p_3()
                .rounded_lg()
                // Selection = thick accent border + a cool same-family gradient.
                .border_2()
                .border_color(if selected {
                    theme::get().primary
                } else {
                    theme::get().bg_light
                })
                .when(selected, |d| {
                    d.bg(gpui::linear_gradient(
                        145.0,
                        gpui::linear_color_stop(gpui::rgb(0x232838), 0.0),
                        gpui::linear_color_stop(gpui::rgb(0x2E3A5C), 1.0),
                    ))
                })
                .when(!selected, |d| d.bg(theme::get().panel_bg))
                .cursor_pointer()
                .child(
                    div()
                        .text_color(theme::get().text)
                        .child(SharedString::from(preset.label())),
                )
                .children(sample)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        this.onboarding_choice = preset;
                        cx.notify();
                    }),
                )
        };

        let panel = div()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(theme::get().bg_mid)
            .border_1()
            .border_color(theme::get().bg_light)
            .rounded_md()
            .shadow_lg()
            .font_family(ui)
            .text_size(fs)
            .text_color(theme::get().text)
            .child(
                div()
                    .text_color(theme::get().text)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Choose your keymap"),
            )
            .child(
                div()
                    .text_color(theme::get().line_number)
                    .child(if self.onboarding_forced {
                        "Pick a keymap to get started. You can change it later in Kyde → Settings."
                    } else {
                        "Reopen any time from Kyde → Settings (⌘,)."
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_4()
                    .child(preset_card(Preset::WebStorm, cx))
                    .child(preset_card(Preset::VSCode, cx)),
            )
            .child(self.render_shell_command_row(cx))
            // Single primary action, bottom-right: confirm the highlighted choice.
            .child(
                div().flex().flex_row().justify_end().mt_2().child(
                    div()
                        .px_5()
                        .py_1p5()
                        .rounded_md()
                        .bg(theme::get().primary)
                        .text_color(gpui::white())
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .child("Continue")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| {
                                // Apply the shell-command checkbox before closing: only
                                // when ticked and a name is actually free to claim.
                                if this.onboarding_install_cmd
                                    && matches!(shellcmd::state(), shellcmd::State::Available(_))
                                {
                                    if let Err(e) = shellcmd::install() {
                                        this.shell_cmd_error = Some(e);
                                    }
                                }
                                let choice = this.onboarding_choice;
                                this.choose_preset(choice, cx);
                            }),
                        ),
                ),
            );

        // Clicks inside the panel must not reach the backdrop.
        let panel = panel.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

        overlay(cx, !self.onboarding_forced)
            .child(panel)
            .into_any_element()
    }

    /// One row in the keymap picker: a checkbox to install a `ky`/`kyde` shell
    /// launcher (symlink into ~/.local/bin, VSCode-style). Shown on both first
    /// run and reopened Settings; the symlink is created when Continue confirms.
    /// Renders nothing when we can't resolve a location (`Unavailable`).
    fn render_shell_command_row(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let st = shellcmd::state();
        if matches!(st, shellcmd::State::Unavailable) {
            return div().into_any_element();
        }

        // Visual state of the box: installed → always on (and locked); a free
        // name → reflects the pending checkbox; taken → off (and locked).
        let (checked, enabled) = match &st {
            shellcmd::State::Installed(_) => (true, false),
            shellcmd::State::Available(_) => (self.onboarding_install_cmd, true),
            _ => (false, false),
        };
        let label = match &st {
            shellcmd::State::Installed(n) => {
                format!("Shell command installed — run `{n}` in any terminal")
            }
            shellcmd::State::Available(n) => {
                format!("Install `{n}` command — open Kyde from any terminal")
            }
            _ => "`ky` and `kyde` are already taken on your PATH — skipped".to_string(),
        };

        let checkbox = div()
            .size_4()
            .rounded_sm()
            .border_1()
            .border_color(if checked { t.primary } else { t.bg_light })
            .when(checked, |d| d.bg(t.primary))
            .flex()
            .items_center()
            .justify_center()
            .when(checked, |d| {
                d.child(
                    div()
                        .text_color(gpui::white())
                        .text_size(px(11.0))
                        .child("✓"),
                )
            });

        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .pt_3()
            .mt_1()
            .border_t_1()
            .border_color(t.divider)
            .child(checkbox)
            .child(
                div()
                    .text_color(if enabled {
                        t.secondary_text
                    } else {
                        t.line_number
                    })
                    .child(SharedString::from(label)),
            );

        if enabled {
            row = row.cursor_pointer().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.onboarding_install_cmd = !this.onboarding_install_cmd;
                    cx.notify();
                }),
            );
        }

        let mut col = div().flex().flex_col().gap_1().child(row);
        if let Some(err) = &self.shell_cmd_error {
            col = col.child(
                div()
                    .text_color(t.status_deleted)
                    .text_size(px(12.0))
                    .child(SharedString::from(err.clone())),
            );
        }
        col.into_any_element()
    }

    /// Bottom status bar — currently just the branch switcher, anchored bottom-right.
    fn render_status_bar(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        // All bottom-bar text is this muted grey; icons keep their own colours.
        let bar_text = gpui::rgb(0x808289);
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
        let crumb = self.selected_path.as_ref().or(self.open_path.as_ref());
        if let Some(rel) = crumb.filter(|p| !p.as_os_str().is_empty()) {
            // Real filesystem check (not "is it the open file") — single-click selection
            // can point at any file or folder, so the icon must follow the path itself.
            let is_file = self
                .repo_root
                .as_ref()
                .map(|root| root.join(rel).is_file())
                .unwrap_or(false);
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
        let pushing = self.pushing;
        let ahead = self.ahead.unwrap_or(0);
        let tip_text: SharedString = self
            .push_msg
            .clone()
            .map(SharedString::from)
            .unwrap_or_else(|| "Push to origin".into());
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
            .text_color(if self.push_msg.is_some() {
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
        let pulling = self.pulling;
        let behind = self.behind.unwrap_or(0);
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
                    .when(self.behind.unwrap_or(0) > 0 || self.pulling, |d| {
                        d.child(pull_btn)
                    })
                    // Only show Push when there's actually something to push (or one's in flight).
                    .when(self.ahead.unwrap_or(0) > 0 || self.pushing, |d| {
                        d.child(push_btn)
                    })
                    .child(chip),
            )
            .into_any_element()
    }

    /// Branch switcher popup: search box, New Branch, Recent, then All Branches.
    /// Anchored bottom-right above the status bar; transparent backdrop closes it.
    fn render_branch_popup(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let q = self.branch_query.read(cx).text().to_string();
        let ql = q.trim().to_lowercase();
        let current = self.current_branch.clone();
        let matches = |b: &str| ql.is_empty() || b.to_lowercase().contains(&ql);

        let recent: Vec<String> = self
            .branch_list
            .iter()
            .filter(|b| current.as_deref() != Some(b.as_str()))
            .filter(|b| matches(b))
            .take(5)
            .cloned()
            .collect();
        let mut all: Vec<String> = self
            .branch_list
            .iter()
            .filter(|b| matches(b))
            .cloned()
            .collect();
        all.sort_by_key(|b| b.to_lowercase());

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
        let behind = self.behind.unwrap_or(0);
        let pull_label = if self.pulling {
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
            .child(div().flex_1().min_w_0().child(self.branch_query.clone()));

        // Branch tree: Recent + Local sections as expandable roots; `/` → folders.
        // While searching, force everything open so matches are visible.
        let rows = branch_rows(&recent, &all, &self.branch_expanded, !ql.is_empty());
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
                    this.branch_popup_open = false;
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
                                    this.toggle_branch_node(k.clone(), cx)
                                }),
                            )
                            .into_any_element()
                    }
                    BranchNode::Leaf { full } => {
                        let is_current = current.as_deref() == Some(full.as_str());
                        let nm = full.clone();
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .h(px(28.0))
                            .pl(indent)
                            .pr_2()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|s| s.bg(t.selected_bg))
                            .text_color(t.text)
                            .child(div().flex_none().text_color(t.line_number).child("⎇"))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(r.label)),
                            )
                            .when(is_current, |d| {
                                d.child(div().flex_none().text_color(t.line_number).child("✓"))
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, window, cx| {
                                    this.checkout_branch(nm.clone(), window, cx)
                                }),
                            )
                            .into_any_element()
                    }
                }
            })
            .collect()
    }

    /// Right-click menu, positioned at the cursor. Transparent backdrop closes it.
    fn render_context_menu(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
            MenuTarget::EditorGit(p) => {
                let (pc, pr) = (p.clone(), p.clone());
                panel = panel.child(item("Commit").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.menu_commit_path(pc.clone(), cx)),
                ));
                if self.has_changes_under(p) {
                    panel = panel.child(item("Rollback").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.open_rollback_path(pr.clone(), cx)
                        }),
                    ));
                }
                // Git remote ops, WebStorm-style (Fetch/Pull always, Push when ahead).
                panel = panel
                    .child(item("Fetch").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.do_fetch(cx)),
                    ))
                    .child(item("Pull").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.do_pull(cx)),
                    ));
                if self.ahead.unwrap_or(0) > 0 {
                    panel = panel.child(item("Push").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.open_push_modal(cx)),
                    ));
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
                    p.parent().map(|d| d.to_path_buf()).unwrap_or_default()
                };
                panel = panel.child(item("New File…").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, window, cx| {
                        this.start_new_file(new_dir.clone(), window, cx)
                    }),
                ));
                // Rename applies to files (not folders, for now).
                if !is_dir {
                    let pn = p.clone();
                    panel = panel.child(item("Rename…").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, window, cx| {
                            this.start_rename(pn.clone(), window, cx)
                        }),
                    ));
                }
                // Commit/Rollback only make sense when there are changes under the path.
                if self.has_changes_under(p) {
                    panel = panel
                        .child(item("Commit").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                this.menu_commit_path(pc.clone(), cx)
                            }),
                        ))
                        .child(item("Rollback").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                this.open_rollback_path(pr.clone(), cx)
                            }),
                        ));
                }
                // Git History for this path (recursive for a folder, file-scoped for a file).
                let ph = p.clone();
                panel = panel.child(item("Git History").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.enter_history_for(ph.clone(), cx)),
                ));
                // Git remote ops, WebStorm-style: Fetch/Pull always, Push when ahead.
                panel = panel
                    .child(item("Fetch").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.do_fetch(cx)),
                    ))
                    .child(item("Pull").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.do_pull(cx)),
                    ));
                if self.ahead.unwrap_or(0) > 0 {
                    panel = panel.child(item("Push").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.open_push_modal(cx)),
                    ));
                }
                let pd = p.clone();
                panel
                    .child(item("Reveal in Finder").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.reveal_in_os(&pv, cx)),
                    ))
                    .child(item("Delete…").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.open_delete(pd.clone(), cx)),
                    ))
            }
            MenuTarget::Tab(idx) => {
                let idx = *idx;
                let reveal = self.open_tabs.get(idx).cloned();
                panel
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
                    ))
                    .child(item("Reveal in Finder").on_mouse_down(
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
                if self.ahead.unwrap_or(0) > 0 {
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
            MenuTarget::TabList => {
                if self.open_tabs.is_empty() {
                    panel = panel.child(item("No open tabs"));
                }
                for (i, p) in self.open_tabs.iter().enumerate() {
                    let active = self.open_path.as_ref() == Some(p);
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
                                    if let Some(p) = this.open_tabs.get(i).cloned() {
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
                let cur = self.history_compare;
                for mode in CompareMode::ALL {
                    let label = if mode == cur {
                        SharedString::from(format!("✓ {}", mode.label()))
                    } else {
                        SharedString::from(mode.label())
                    };
                    panel = panel.child(item_owned(label).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.history_compare_commit(idx, mode, cx)
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
            .child(
                gpui::deferred(
                    gpui::anchored()
                        .position(menu.at)
                        .snap_to_window_with_margin(px(8.0))
                        .child(panel),
                )
                .with_priority(1),
            )
            .into_any_element()
    }

    /// Floating "Show Diff" viewer over the Commit view (IntelliJ-style).
    /// Show-Diff window body (own native window; titlebar shows the file path). Just the
    /// side-by-side diff filling the window.
    pub(crate) fn render_diff_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let fs = px(theme::get().editor_font_size);
        let t = theme::get();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.main_bg)
            .font_family(ui)
            .text_size(fs)
            .text_color(t.text)
            .child(self.render_diff(ui, fs, None, cx))
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

    /// "Create New Branch" dialog body (own native window). Name field (`branch_query`) +
    /// Checkout / Overwrite toggles + Cancel / Create.
    pub(crate) fn render_new_branch_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        let (checkout, overwrite) = (self.new_branch_checkout, self.new_branch_overwrite);

        let name_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_4()
            .pt_4()
            .child(div().flex_none().text_color(t.text).child("Branch Name:"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(t.primary)
                    .bg(t.main_bg)
                    .font_family(theme::font::FAMILY)
                    .child(self.branch_query.clone()),
            );

        let check = |label: &'static str, on: bool, toggle: fn(&mut Self, &mut Context<Self>)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .child(checkbox_box(on))
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| toggle(this, cx)),
                )
        };
        let checks = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_6()
            .px_4()
            .pt_3()
            .child(check("Checkout branch", checkout, |this, cx| {
                this.new_branch_checkout = !this.new_branch_checkout;
                cx.notify();
            }))
            .child(check("Overwrite existing branch", overwrite, |this, cx| {
                this.new_branch_overwrite = !this.new_branch_overwrite;
                cx.notify();
            }));

        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .px_4()
            .pb_4()
            .pt_4()
            .child(
                btn_secondary("newbranch-cancel", "Cancel")
                    .py_1p5()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.close_modal_window(ModalKind::NewBranch, cx)
                        }),
                    ),
            )
            .child(btn_primary("create", "Create").py_1p5().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.do_create_branch(cx)),
            ));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.panel_bg)
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size))
            .text_color(t.text)
            .child(name_row)
            .child(checks)
            .child(div().flex_1()) // spacer → footer sits at the bottom
            .child(footer)
            .into_any_element()
    }

    fn render_delete_modal(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let Some((path, is_dir)) = self.delete_target.clone() else {
            return div().into_any_element();
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let kind = if is_dir { "folder" } else { "file" };

        let cancel = btn_secondary("delete-cancel", "Cancel")
            .px_3()
            .py_1p5()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.delete_target = None;
                    cx.notify();
                }),
            );
        let confirm = div()
            .id("delete-confirm")
            .px_3()
            .py_1p5()
            .rounded_md()
            .bg(t.status_deleted)
            .text_color(t.primary_text)
            .cursor_pointer()
            .hover(|s| s.opacity(0.9))
            .child("Delete")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.do_delete(cx)),
            );

        let panel = div()
            .w(px(420.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(t.frame_bg)
            .border_1()
            .border_color(t.divider)
            .rounded(px(theme::ISLAND_RADIUS))
            .shadow_lg()
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size + 1.0))
            .occlude()
            .child(div().text_color(t.text).child(format!("Delete {kind}?")))
            .child(
                div()
                    .text_color(t.secondary_text)
                    .text_size(px(theme::get().ui_font_size))
                    .child(format!(
                        "“{name}” will be permanently deleted from disk. This can't be undone."
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(cancel)
                    .child(confirm),
            );
        overlay(cx, true).child(panel).into_any_element()
    }

    /// Body of the "Clear Data & Restart" confirmation modal window. Destructive: wipes the
    /// whole config dir and restarts.
    pub(crate) fn render_clear_data_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        let cancel = btn_secondary("clear-cancel", "Cancel").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| this.close_modal_window(ModalKind::ClearData, cx)),
        );
        let confirm = btn_primary("clear-confirm", "Clear & Restart").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| this.do_clear_data(cx)),
        );
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size + 1.0))
            .child(div().text_color(t.text).child("Clear all data & restart?"))
            .child(
                div()
                    .flex_1()
                    .text_color(t.secondary_text)
                    .text_size(px(theme::get().ui_font_size))
                    .child(
                        "Uninstalls all language plugins and clears cached settings (keymap, \
                         theme, recent projects, preferences). Kyde will restart. Can't be undone.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(cancel)
                    .child(confirm),
            )
            .into_any_element()
    }

    /// New-file / rename modal: a single-line name input + Create/Rename & Cancel.
    /// Enter confirms / Esc cancels via the "FileFinder" key context (the input is
    /// single-line, so those keys bubble up to this wrapper).
    fn render_name_prompt(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let Some(prompt) = self.name_prompt.clone() else {
            return div().into_any_element();
        };
        let (title, action) = match &prompt {
            NamePrompt::NewFile(_) => ("New file", "Create"),
            NamePrompt::Rename(_) => ("Rename", "Rename"),
        };

        let cancel = btn_secondary("name-cancel", "Cancel")
            .px_3()
            .py_1p5()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, window, cx| this.cancel_name_prompt(window, cx)),
            );
        let confirm = btn_primary("name-confirm", action)
            .px_3()
            .py_1p5()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, window, cx| this.confirm_name_prompt(window, cx)),
            );

        let panel =
            div()
                .key_context("FileFinder")
                .on_action(cx.listener(|this, _: &FinderConfirm, window, cx| {
                    this.confirm_name_prompt(window, cx)
                }))
                .on_action(cx.listener(|this, _: &FinderClose, window, cx| {
                    this.cancel_name_prompt(window, cx)
                }))
                .w(px(420.0))
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .bg(t.frame_bg)
                .border_1()
                .border_color(t.divider)
                .rounded(px(theme::ISLAND_RADIUS))
                .shadow_lg()
                .font_family(ui)
                .text_size(px(theme::get().ui_font_size + 1.0))
                .occlude()
                .child(div().text_color(t.text).child(title))
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(t.main_bg)
                        .border_1()
                        .border_color(t.divider)
                        .child(self.name_input.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .child(cancel)
                        .child(confirm),
                );
        overlay(cx, true).child(panel).into_any_element()
    }

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
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        // Same folder tree as the Commit view, with rollback checkboxes.
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
                self.tree_row(
                    cx,
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

    /// Font specimen viewer: each bundled family at every available weight, previewing a
    /// full sentence at a large size so weight differences are obvious.
    /// Body of the Fonts modal window (hosted by `ModalWindow`, native titlebar).
    pub(crate) fn render_fonts_body(&mut self, _cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        const QUOTE: &str = "Me fail English? That's unpossible.";
        let line = |family: &'static str, label: &'static str, weight: FontWeight| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .py_2()
                .child(
                    div()
                        .font_family(ui)
                        .text_color(t.line_number)
                        .text_size(px(11.0))
                        .child(SharedString::from(label)),
                )
                .child(
                    div()
                        .font_family(family)
                        .font_weight(weight)
                        .text_size(px(26.0))
                        .text_color(t.text)
                        .child(QUOTE),
                )
        };
        let header = |title: &'static str| {
            div()
                .px_3()
                .pt_3()
                .pb_1()
                .font_family(ui)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.secondary_text)
                .text_size(px(12.0))
                .child(SharedString::from(title))
        };
        // Fills the modal window (chrome + native "Fonts" titlebar from `ModalWindow`).
        div()
            .size_full()
            .flex()
            .flex_col()
            .font_family(ui)
            .child(
                div()
                    .id("fonts-list")
                    .overflow_y_scroll()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .pb_2()
                    .child(header("Inter — UI"))
                    .child(line(
                        theme::font::UI_FAMILY,
                        "Regular · 400",
                        FontWeight::NORMAL,
                    ))
                    .child(line(
                        theme::font::UI_FAMILY,
                        "Medium · 500",
                        FontWeight::MEDIUM,
                    ))
                    .child(line(
                        theme::font::UI_FAMILY,
                        "SemiBold · 600",
                        FontWeight::SEMIBOLD,
                    ))
                    .child(line(theme::font::UI_FAMILY, "Bold · 700", FontWeight::BOLD))
                    .child(header("JetBrains Mono — Code"))
                    .child(line(
                        theme::font::FAMILY,
                        "Regular · 400",
                        FontWeight::NORMAL,
                    ))
                    .child(line(
                        theme::font::FAMILY,
                        "SemiBold · 600",
                        FontWeight::SEMIBOLD,
                    ))
                    .child(line(theme::font::FAMILY, "Bold · 700", FontWeight::BOLD)),
            )
            .into_any_element()
    }

    /// Bottom terminal panel: a drag-resize divider, a tab strip (one tab per shell +
    /// a new-tab button), and the active terminal filling the rest. Inset to align
    /// under the islands (left of the activity rail).
    #[cfg(feature = "terminal")]
    fn render_terminal_panel(
        &mut self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        // Tab strip: heading + one chip per shell + a new-tab button.
        let mut strip = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .h(px(34.0))
            .px_3()
            .flex_none()
            .child(
                div()
                    .font_family(ui)
                    .text_size(px(13.0))
                    .text_color(t.text)
                    .child("Terminal"),
            );
        for (i, view) in self.term_tabs.iter().enumerate() {
            let active = i == self.term_active;
            let mut title = view.read(cx).title.clone();
            if view.read(cx).exited {
                title.push_str(" (exited)");
            }
            strip = strip.child(
                div()
                    .id(("term-tab", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .when(active, |d| d.bg(t.selected_bg))
                    .when(!active, |d| d.hover(|d| d.bg(t.bg_mid)))
                    .cursor_pointer()
                    .font_family(ui)
                    .text_size(px(12.0))
                    .text_color(if active {
                        t.primary_text
                    } else {
                        t.secondary_text
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, window, cx| {
                            this.term_active = i;
                            this.focus_active_terminal(window, cx);
                            cx.notify();
                        }),
                    )
                    .child(title)
                    .child(
                        div()
                            .id(("term-tab-x", i))
                            .px_1()
                            .rounded_sm()
                            .hover(|d| d.bg(t.bg_light))
                            .child("×")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    cx.stop_propagation();
                                    this.close_terminal_tab(i, cx);
                                }),
                            ),
                    ),
            );
        }
        strip = strip.child(
            div()
                .id("term-tab-new")
                .size(px(22.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .hover(|d| d.bg(t.bg_mid))
                .cursor_pointer()
                .text_color(t.secondary_text)
                .tooltip(move |_w, cx| cx.new(|_| Tip("New terminal tab".into())).into())
                .child("+")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, window, cx| {
                        this.new_terminal_tab(cx);
                        this.focus_active_terminal(window, cx);
                        cx.notify();
                    }),
                ),
        );
        // Spacer pushes the minimize + maximize/restore toggles to the right edge of the strip.
        let maxed = self.term_maximized;
        strip = strip
            .child(div().flex_1().min_w_0())
            // Minimize: hide the panel (reopen via the rail icon or ⌃`). A `−` glyph styled
            // like the tree / commit-panel collapse buttons.
            .child(
                div()
                    .id("term-minimize")
                    .size(px(22.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .text_size(px(16.0))
                    .text_color(t.line_number)
                    .hover(|d| d.bg(t.bg_light).text_color(t.text))
                    .cursor_pointer()
                    .tooltip(move |_w, cx| cx.new(|_| Tip("Minimize terminal".into())).into())
                    .child("−")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.term_open = false;
                            this.term_maximized = false;
                            cx.notify();
                        }),
                    ),
            )
            .child(
                div()
                    .id("term-maximize")
                    .size(px(22.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    // Same colour + hover as the minimize button next to it.
                    .text_color(t.line_number)
                    .hover(|d| d.bg(t.bg_light).text_color(t.text))
                    .cursor_pointer()
                    .tooltip(move |_w, cx| {
                        let label = if maxed {
                            "Restore terminal"
                        } else {
                            "Maximize terminal"
                        };
                        cx.new(|_| Tip(label.into())).into()
                    })
                    .child(
                        svg()
                            .path(if maxed {
                                "icons/minimize-2.svg"
                            } else {
                                "icons/maximize-2.svg"
                            })
                            .size(px(15.0))
                            .text_color(t.line_number),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, window, cx| {
                            this.term_maximized = !this.term_maximized;
                            // Persist so the terminal reopens maximized next time.
                            crate::save_ui_bool("terminal_maximized", this.term_maximized);
                            this.focus_active_terminal(window, cx);
                            cx.notify();
                        }),
                    ),
            );

        // The active terminal widget (entity → element).
        let body = self
            .term_tabs
            .get(self.term_active)
            .map(|v| v.clone().into_any_element())
            .unwrap_or_else(|| div().into_any_element());

        let island = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.main_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .overflow_hidden()
            .child(strip)
            .child(div().flex_1().min_h_0().child(body));

        // A thin top divider whose drag resizes the panel (the shared `Divider::Term` drag).
        let divider = div()
            .h(px(6.0))
            .flex_none()
            .cursor_row_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                    this.start_divider_drag(Divider::Term, e.position, window);
                    cx.notify();
                }),
            );

        div()
            .flex()
            .flex_col()
            // Maximized → fill the right column; otherwise a fixed, drag-resizable height.
            .when(self.term_maximized, |d| d.flex_1().min_h_0())
            .when(!self.term_maximized, |d| {
                d.flex_none().h(px(self.term_height))
            })
            // Inside the right column already (right of the full-height rail) → no left pad;
            // aligns with the body island above it.
            .pr(px(theme::FRAME_GAP))
            // Maximized has no body above it, so it needs the top frame gap the body used to
            // provide; docked mode gets its top spacing from the resize divider instead.
            .when(self.term_maximized, |d| d.pt(px(theme::FRAME_GAP)))
            .pb(px(theme::FRAME_GAP))
            .bg(t.frame_bg)
            // No resize divider when maximized (it fills the column).
            .when(!self.term_maximized, |d| d.child(divider))
            .child(island)
            .into_any_element()
    }
}

/// Representative file extension for a pack id, so `file_badge` can pick the language
/// monogram chip shown in the plugin manager.
fn pack_ext(id: &str) -> &'static str {
    match id {
        "json" => "json",
        "typescript" => "ts",
        "javascript" => "js",
        "rust" => "rs",
        "markdown" => "md",
        "shell" => "sh",
        "css" => "css",
        "scss" => "scss",
        "yaml" => "yml",
        "toml" => "toml",
        "python" => "py",
        "html" => "html",
        "go" => "go",
        "env" => "env",
        "gitignore" => "gitignore",
        "font" => "ttf",
        _ => "txt",
    }
}

/// Approximate compiled footprint of a pack's grammar (tree-sitter parse tables linked
/// into the binary). These ship in the binary rather than being downloaded, so this is the
/// resident size each adds — a rough, static figure, not an exact per-build measurement.
fn pack_size(id: &str) -> &'static str {
    match id {
        "json" => "~55 KB",
        "typescript" => "~2.6 MB",
        "javascript" => "~1.1 MB",
        "rust" => "~1.6 MB",
        "markdown" => "~210 KB",
        "shell" => "~480 KB",
        "css" => "~260 KB",
        "scss" => "shares CSS grammar",
        "yaml" => "~150 KB",
        "toml" => "~120 KB",
        "python" => "~900 KB",
        "html" => "~120 KB",
        "go" => "~700 KB",
        "env" | "gitignore" => "built-in (no grammar)",
        "font" => "preview only",
        _ => "—",
    }
}

/// Standard **secondary** button (transparent fill + divider border + secondary text).
/// Caller chains `.on_mouse_down(...)`. See the "Buttons" UI principle in CLAUDE.md.
fn btn_secondary(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    div()
        .id(id)
        .px_4()
        // 4px (was 6px) vertical pad → ~4px shorter button universally.
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(t.divider)
        .text_color(t.secondary_text)
        .cursor_pointer()
        .hover(|s| s.bg(t.bg_mid))
        .child(label.into())
}

/// Icon path for a context-menu row, keyed off its label (so call sites stay `item("…")`).
/// Tolerates a leading "✓ " (compare modes) and a trailing "…". `None` → no icon (e.g. tab
/// file-name rows), which still reserves the icon slot so labels line up.
fn menu_icon(label: &str) -> Option<&'static str> {
    let l = label.trim_start_matches("✓ ").trim_end_matches('…').trim();
    Some(match l {
        "Commit" => "icons/git-commit.svg",
        "Rollback" => "icons/rotate-ccw.svg",
        "Fetch" => "icons/arrow-down-to-line.svg",
        "Pull" => "icons/arrow-down.svg",
        "Push" => "icons/arrow-up.svg",
        "New File" => "icons/file-plus.svg",
        "Rename" => "icons/pencil.svg",
        "Delete" => "icons/trash.svg",
        "Git History" => "icons/history.svg",
        "Reveal in Finder" => "icons/folder.svg",
        "View Diff" | "Show Diff" => "icons/file-lines.svg",
        _ if l.starts_with("Close") => "icons/x.svg",
        _ if l.starts_with("Compare") => "icons/git-branch.svg",
        _ => return None,
    })
}

/// One pill of a tab strip (e.g. the git view's Commit/Push tabs), IntelliJ-style: active =
/// subtle filled bg + faint border; inactive = transparent with a hover bg. A `count` badge
/// shows when > 0 (accent-filled on the active tab). Caller chains `.on_mouse_down(...)`.
fn tab_pill(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    count: usize,
    active: bool,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    let mut d = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when(active, |d| {
            d.bg(t.bg_light)
                .border_1()
                .border_color(t.divider)
                .text_color(t.text)
        })
        .when(!active, |d| {
            d.text_color(t.line_number).hover(|d| d.bg(t.bg_mid))
        })
        .child(label.into());
    if count > 0 {
        d = d.child(
            div()
                .flex_none()
                .px(px(5.0))
                .rounded_sm()
                .bg(if active { t.primary } else { t.bg_light })
                .text_size(px(10.0))
                .text_color(if active {
                    t.primary_text
                } else {
                    t.secondary_text
                })
                .child(SharedString::from(count.to_string())),
        );
    }
    d
}

/// Standard **primary** button (accent fill + primary text). Caller chains `.on_mouse_down`.
fn btn_primary(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    btn_primary_state(id, label, false)
}

/// `btn_primary` with an explicit `disabled` state. gpui allows only ONE `.hover()` per
/// element (a second one panics: "hover style already set"), so the disabled look — dimmed,
/// and *staying* dimmed on hover — must be baked into the single hover here rather than
/// chained on by the caller.
fn btn_primary_state(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    disabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    let b = div()
        .id(id)
        .px_4()
        // 4px (was 6px) vertical pad → ~4px shorter button universally.
        .py_1()
        .rounded_md()
        .bg(t.primary)
        .text_color(t.primary_text);
    let b = if disabled {
        b.opacity(0.6).cursor_default().hover(|s| s.opacity(0.6))
    } else {
        b.cursor_pointer().hover(|s| s.opacity(0.9))
    };
    b.child(label.into())
}

/// Linearly interpolate two `0xRRGGBB` colors (`t` in 0..1) → opaque `Rgba`. Used for the
/// welcome-screen ASCII shimmer.
fn lerp_rgb(a: u32, b: u32, t: f32) -> gpui::Rgba {
    let t = t.clamp(0.0, 1.0);
    let chan = |hex: u32, shift: u32| ((hex >> shift) & 0xFF) as f32 / 255.0;
    let mix = |x: f32, y: f32| x + (y - x) * t;
    gpui::Rgba {
        r: mix(chan(a, 16), chan(b, 16)),
        g: mix(chan(a, 8), chan(b, 8)),
        b: mix(chan(a, 0), chan(b, 0)),
        a: 1.0,
    }
}

/// Scrollbar thumb length + position for a track, kept **panic-safe** at any window size.
/// `track` = usable track length (px), `max` = max scroll offset (px), `off` = current
/// (negative) scroll offset (px), `end` = inset at each track end (px). Returns
/// `(thumb_len, thumb_pos)`, both clamped within the track.
///
/// Why this exists: the thumb length used to be `(…).clamp(28.0, track - 2*end)` inline.
/// `f32::clamp` PANICS when `min > max`, and `track - 2*end` drops below 28 once the window
/// is shrunk past ~44px — so resizing tiny aborted the process (SIGABRT). Pinning the min
/// under the max here makes it impossible. Pure so it can be unit-tested (below).
fn scrollbar_thumb(track: f32, max: f32, off: f32, end: f32) -> (f32, f32) {
    let hi = (track - 2.0 * end).max(8.0);
    let len = if max > 0.0 {
        (track * track / (track + max)).clamp(28.0_f32.min(hi), hi)
    } else {
        hi
    };
    let span = (track - len - 2.0 * end).max(0.0);
    let frac = if max > 0.0 {
        (-off / max).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (len, end + frac * span)
}

#[cfg(test)]
mod tests {
    use super::scrollbar_thumb;

    /// Regression guard for the resize-to-tiny SIGABRT: `scrollbar_thumb` must never panic
    /// and must stay within the track for any size — including tracks far below the thumb
    /// min, zero/huge content, and out-of-range offsets. (The old inline `clamp(28, track-16)`
    /// aborted here.) Keep this — it's the whole reason the helper is pure. See CLAUDE.md.
    #[test]
    fn scrollbar_thumb_never_panics_when_tiny() {
        let tracks = [-50.0, 0.0, 1.0, 10.0, 28.0, 43.9, 44.0, 200.0, 5000.0];
        let maxes = [0.0, 0.5, 1.0, 50.0, 100_000.0];
        let offs = [-1e9, -100.0, 0.0, 50.0, 1e9];
        for &tr in &tracks {
            for &mx in &maxes {
                for &of in &offs {
                    let (len, pos) = scrollbar_thumb(tr, mx, of, 8.0);
                    assert!(len.is_finite() && len > 0.0, "len {len} (track {tr})");
                    assert!(pos.is_finite() && pos >= 0.0, "pos {pos} (track {tr})");
                    // Thumb cannot start past the end of the track.
                    assert!(pos <= tr.max(8.0) + 1.0, "pos {pos} past track {tr}");
                }
            }
        }
    }
}
