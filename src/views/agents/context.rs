//! The active worktree, shared by every view: the worktree selected in the Agents view is the
//! open project, so Explorer / Commit / History show its code; opening another of an agent
//! space's worktrees selects it in the Agents view. The title bar names it (project ›
//! worktree, each a dropdown to switch), with the way back to its agent; ⌥⌘O flips between
//! the agent and its code.

use super::*;

impl Kyde {
    /// The open project when it is a row of the Agents sidebar: `(space, worktree)`.
    pub(crate) fn agents_row_of_project(&self) -> Option<(PathBuf, PathBuf)> {
        let root = self.repo_root.as_ref()?;
        let space = self.agents.cfg.space_of(root)?;
        Some((space.path.clone(), root.clone()))
    }

    /// The name a project gets when `root` is one of an agent space's rows: the space's (the
    /// worktree crumb says which worktree).
    pub(crate) fn agents_space_label(&self, root: &Path) -> Option<String> {
        self.agents
            .cfg
            .space_of(root)
            .map(|s| self.agents_space_name(s))
    }

    /// Worktree `wt` was selected: it becomes the open project, in the view on screen (a
    /// worktree still being made waits for its folder).
    pub(crate) fn agents_sync_project(&mut self, wt: PathBuf, cx: &mut Context<Self>) {
        if self.repo_root.as_ref() == Some(&wt)
            || self.agents.cfg.space_of(&wt).is_none()
            || !wt.is_dir()
        {
            return;
        }
        let mode = self.mode;
        self.open_project(wt, cx);
        if mode == Mode::Agents {
            self.mode = Mode::Agents;
        }
    }

    /// A project was opened: when it's an agent worktree, the Agents view selects it — without
    /// starting an agent there.
    pub(crate) fn agents_track_project(&mut self, cx: &mut Context<Self>) {
        let Some((_, wt)) = self.agents_row_of_project() else {
            return;
        };
        if self.agents.selected.as_ref() != Some(&wt) {
            self.agents_reveal(&wt);
            self.agents.selected = Some(wt.clone());
            self.agents.cfg.selected = Some(wt);
            self.agents_save(cx);
        }
    }

    /// Show worktree `wt`'s code, opened as a project, in `mode` — else the view it was last
    /// in there, else the Explorer.
    pub(crate) fn agents_open_code(
        &mut self,
        wt: PathBuf,
        mode: Option<Mode>,
        cx: &mut Context<Self>,
    ) {
        if self.agents_only() {
            self.agents_set_only(false, cx);
        }
        if self.repo_root.as_ref() == Some(&wt) {
            // Already open (it may just have become an agent's): Agents selects it too.
            self.agents_track_project(cx);
        } else {
            self.open_project(wt, cx);
        }
        let last = (self.mode != Mode::Agents).then_some(self.mode);
        self.switch_mode(mode.or(last).unwrap_or(Mode::Browse), cx);
    }

    /// Back to worktree `wt`'s agent tabs in the Agents view.
    pub(crate) fn agents_back_to(&mut self, wt: PathBuf, cx: &mut Context<Self>) {
        self.switch_mode(Mode::Agents, cx);
        self.agents.side_tab = SideTab::Code;
        self.agents.gh_scope = None;
        self.agents.sub_shown = None;
        self.agents_select(wt, cx);
    }

    /// ⌥⌘O: from an agent to its worktree's code, and from an agent worktree's code back to
    /// its agent. Anywhere else, the Agents view.
    pub(crate) fn act_agent_toggle_code(
        &mut self,
        _: &AgentToggleCode,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode == Mode::Agents {
            let wt = self.agents.selected.clone();
            if let Some(wt) = wt.filter(|w| self.agents.cfg.space_of(w).is_some()) {
                self.agents_open_code(wt, None, cx);
            }
        } else if let Some((_, wt)) = self.agents_row_of_project() {
            self.agents_back_to(wt, cx);
        } else {
            self.switch_mode(Mode::Agents, cx);
        }
    }

    /// Make `path` (an agent worktree) the one everything shows: in the Agents view, as a click
    /// on its row does; in the code views, its code in the same view.
    pub(crate) fn agents_switch_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.mode == Mode::Agents {
            self.agents_select(path, cx);
        } else {
            let mode = self.mode;
            self.agents_open_code(path, Some(mode), cx);
        }
    }

    /// A folder you chose to open (`open_folder`): it joins the Agents view as a space (unless
    /// it's already one, or one's worktree) and becomes what everything shows.
    pub(crate) fn agents_open_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.agents_ensure_space(&path, cx);
        self.agents_switch_to(path, cx);
    }

    /// The title bar's worktree crumb, after the project's: `› ● title ▾  ⑂ branch` — a
    /// dropdown of the space's worktrees — and, over the code, the way back to its agent.
    /// Nothing when the open project isn't an agent worktree.
    pub(crate) fn render_worktree_crumb(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let (space, wt) = self.agents_row_of_project()?;
        let t = theme::get();
        let title = self.agents_wt_title(&space, &wt);
        let branch = self
            .current_branch
            .clone()
            .or_else(|| self.agents.branches.get(&wt).cloned())
            .filter(|b| *b != title);
        let crumb = div()
            .id("titlebar-worktree")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .min_w_0()
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .hover(|d| d.bg(t.bg_mid))
            .tooltip(ui::tip("Switch worktree"))
            .child(status_dot(self.agents_state(&wt)))
            .child(
                div()
                    .min_w_0()
                    .max_w(px(320.0))
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text)
                    .child(SharedString::from(title)),
            )
            .children(branch.map(|b| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .min_w_0()
                    .text_color(t.secondary_text)
                    .child(
                        svg()
                            .path("icons/git-branch.svg")
                            .flex_none()
                            .size(px(12.0))
                            .text_color(t.secondary_text),
                    )
                    .child(
                        div()
                            .max_w(px(240.0))
                            .truncate()
                            .font_family(theme::font::family())
                            .child(SharedString::from(b)),
                    )
            }))
            .child(
                svg()
                    .path("icons/chevron-down.svg")
                    .flex_none()
                    .size(px(14.0))
                    .text_color(t.secondary_text),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, _w, cx| {
                    let at = gpui::point(e.position.x - px(8.0), e.position.y + px(16.0));
                    this.open_menu(at, MenuTarget::Worktrees, cx);
                }),
            );
        let key = self
            .keymap
            .key_for("agent_toggle_code")
            .map(|k| format!("  ({})", pretty_key(&k)))
            .unwrap_or_default();
        let back = (self.mode != Mode::Agents).then(|| {
            ui::icon_btn("titlebar-back-to-agent", 26.0)
                .flex_none()
                .tooltip(ui::tip(format!("Back to agent{key}")))
                .child(
                    svg()
                        .path("icons/bot.svg")
                        .size(px(15.0))
                        .text_color(t.primary),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.agents_back_to(wt.clone(), cx)),
                )
        });
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .min_w_0()
                .child(div().flex_none().text_color(t.line_number).child("›"))
                .child(crumb)
                .children(back),
        )
    }

    /// The worktree crumb's menu: the open space's worktrees, each as its Agents row (the one
    /// shown ticked), a new one, and — over the code — the way back to the agent.
    pub(crate) fn agents_worktrees_menu(&self) -> Vec<MenuEntry> {
        let Some((space, wt)) = self.agents_row_of_project() else {
            return Vec::new();
        };
        let Some(sp) = self.agents.cfg.space_of(&wt) else {
            return Vec::new();
        };
        let mut out: Vec<MenuEntry> = sp
            .rows()
            .into_iter()
            .map(|row| MenuEntry::Worktree {
                space: space.clone(),
                current: row == wt,
                run: std::rc::Rc::new({
                    let row = row.clone();
                    move |this: &mut Kyde, _w: &mut Window, cx: &mut Context<Kyde>| {
                        this.agents_switch_to(row.clone(), cx);
                    }
                }),
                wt: row,
            })
            .collect();
        if !sp.scratch && self.agents.git_spaces.contains(&space) {
            out.push(MenuEntry::Divider);
            out.push(MenuEntry::item("New Worktree", move |this, _w, cx| {
                this.agents_new_worktree(space.clone(), cx);
            }));
        }
        if self.mode != Mode::Agents {
            out.push(MenuEntry::Divider);
            out.push(MenuEntry::item("Back to Agent", move |this, _w, cx| {
                this.agents_back_to(wt.clone(), cx);
            }));
        }
        out
    }
}
