//! Projects landing view (recents, welcome hero) + project open/switch/session plumbing.
//! Crate-root child module.

use crate::*;

impl Kyde {
    pub(crate) fn render_projects(
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

    /// Editor tab strip: one tab per open file, left→right in open order. Click activates,
    /// the `×` closes, right-click opens the tab context menu (close / others / to the right).
    /// The single-project title shown in the title bar, just right of the traffic lights
    /// (only when ≤1 project is open — with more, the project-tabs strip takes over). Colored
    /// initials chip + name + a chevron; clicking opens another project (`pick_folder`).
    pub(crate) fn render_titlebar_project(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(root) = self.repo_root.as_ref() else {
            return div().into_any_element();
        };
        let t = theme::get();
        let name = projects::name_of(root);
        let chip_color = gpui::rgb(projects::color_for(&name));
        div()
            .id("titlebar-project")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .flex_none()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .hover(|d| d.bg(t.bg_mid))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(22.0))
                    .flex_none()
                    .rounded_md()
                    .bg(chip_color)
                    .text_color(gpui::white())
                    .text_size(px(10.0))
                    .font_weight(FontWeight::BOLD)
                    .child(SharedString::from(projects::initials(&name))),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text)
                    .child(SharedString::from(name)),
            )
            .child(
                div()
                    .text_color(t.line_number)
                    .text_size(px(10.0))
                    .child("▾"),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.pick_folder(cx)),
            )
            .into_any_element()
    }

    /// Project tabs strip: one pill per open project, above all other UI (under the title
    /// bar). Click switches projects; the `×` closes one. Rendered only when >1 is open.
    pub(crate) fn render_project_tabs(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
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

    /// Open a folder as the active project (or switch to it if already open): record it in
    /// recents, add a project tab, and load its state. Each open project is a tab above the
    /// UI; switching preserves the one you're leaving (see `ProjectSession`).
    pub(crate) fn open_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.recents.touch(&path);
        self.recents.save();
        // Keep the Dock + menu-bar "Recent Projects" lists in sync with the new order.
        cx.set_dock_menu(dock_menu(&self.recents));
        cx.set_menus(crate::app_menus(&self.recents));
        // Stash the project we're leaving so a later switch back restores it.
        self.save_active_session();
        if !self.open_projects.contains(&path) {
            self.open_projects.push(path.clone());
        }
        self.load_project_state(path, cx);
        cx.notify();
    }

    /// Snapshot the active project's UI state into `project_sessions` (no-op on the landing
    /// view). Called before switching away so switching back restores it.
    fn save_active_session(&mut self) {
        if let Some(root) = self.repo_root.clone() {
            self.project_sessions.insert(
                root,
                crate::ProjectSession {
                    mode: self.mode,
                    open_path: self.open_path.clone(),
                    open_tabs: self.open_tabs.clone(),
                    preview_tab: self.preview_tab.clone(),
                    selected: self.selected,
                    expanded: self.expanded.clone(),
                },
            );
        }
    }

    /// Make `path` the active project, restoring its saved session if we have one (which file
    /// was open, the editor tabs, tree expansion, mode) or starting fresh otherwise.
    fn load_project_state(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.repo_root = Some(path.clone());
        match self.project_sessions.remove(&path) {
            Some(s) => {
                self.mode = s.mode;
                self.expanded = s.expanded;
                self.open_tabs = s.open_tabs;
                self.preview_tab = s.preview_tab;
                self.selected = s.selected;
                self.refresh();
                // Reload the file that was open into the editor; else leave it empty.
                // Restore as permanent — `open_file` would clear the preview slot, so save it
                // first and put it back (the restored active file may itself be the preview).
                let preview = self.preview_tab.clone();
                match s.open_path {
                    Some(p) => self.open_file(p, cx),
                    None => self.open_path = None,
                }
                self.preview_tab = preview;
            }
            None => {
                self.mode = Mode::Browse; // open into the code view, not git
                self.open_path = None;
                self.open_tabs.clear();
                self.preview_tab = None;
                self.selected = None;
                self.expanded.clear();
                self.expanded.insert(PathBuf::new()); // root folder visible by default
                self.refresh();
            }
        }
    }

    /// Close an open-project tab. Switches to a neighbour if it was active; closing the last
    /// one returns to the Projects landing view.
    pub(crate) fn close_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let Some(idx) = self.open_projects.iter().position(|p| p == &root) else {
            return;
        };
        let was_active = self.repo_root.as_ref() == Some(&root);
        self.open_projects.remove(idx);
        self.project_sessions.remove(&root);
        if !was_active {
            cx.notify();
            return;
        }
        if self.open_projects.is_empty() {
            // Back to the landing view.
            self.repo_root = None;
            self.open_path = None;
            self.open_tabs.clear();
            self.preview_tab = None;
            self.selected = None;
        } else {
            // Prefer the tab that shifted into this slot, else the previous one.
            let next = self.open_projects[idx.min(self.open_projects.len() - 1)].clone();
            self.load_project_state(next, cx);
        }
        cx.notify();
    }

    /// Open a project chosen from the Dock's "Recent Projects" submenu.
    pub(crate) fn open_recent_project(
        &mut self,
        a: &OpenRecentProject,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_project(PathBuf::from(&a.0), cx);
    }

    /// File → Open… — pick a folder and open it as a new project tab.
    pub(crate) fn act_open_project(
        &mut self,
        _: &OpenProject,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pick_folder(cx);
    }

    /// Native folder picker for the "Open" / "New Project" buttons.
    pub(crate) fn pick_folder(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths.into_iter().next() {
                    this.update(cx, |this, cx| this.open_project(p, cx)).ok();
                }
            }
        })
        .detach();
    }
}
