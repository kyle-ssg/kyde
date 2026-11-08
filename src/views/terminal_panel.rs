//! Bottom terminal panel UI glue (tabs, toggle, resize). The PTY engine lives in
//! src/terminal.rs; this drives it from Kyde. Crate-root child module (terminal feature).

use crate::*;

impl Kyde {
    /// Bottom terminal panel: a drag-resize divider, a tab strip (one tab per shell +
    /// a new-tab button), and the active terminal filling the rest. Inset to align
    /// under the islands (left of the activity rail).
    #[cfg(feature = "terminal")]
    pub(crate) fn render_terminal_panel(
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
        for (i, view) in self.term.tabs.iter().enumerate() {
            let active = i == self.term.panel.active;
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
                    // `selected_bg` is a subtle tint (NOT the accent fill), so the active tab
                    // uses the general text colour — like the Settings sidebar / tree rows.
                    // `primary_text` (white) was unreadable on the light-mode light-blue tint.
                    .text_color(if active { t.text } else { t.secondary_text })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, window, cx| {
                            this.term.panel.active = i;
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
            ui::icon_btn("term-tab-new", 22.0)
                .tooltip(ui::tip("New terminal tab"))
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
        let maxed = self.term.panel.maximized;
        strip = strip
            .child(div().flex_1().min_w_0())
            // Minimize: hide the panel (reopen via the rail icon or ⌃`). A `−` glyph styled
            // like the tree / commit-panel collapse buttons.
            .child(
                ui::icon_btn("term-minimize", 22.0)
                    .text_size(px(16.0))
                    .tooltip(ui::tip("Minimize terminal"))
                    .child("−")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.term.panel.open = false;
                            this.term.panel.maximized = false;
                            cx.notify();
                        }),
                    ),
            )
            .child(
                ui::icon_btn("term-maximize", 22.0)
                    .tooltip(ui::tip(if maxed {
                        "Restore terminal"
                    } else {
                        "Maximize terminal"
                    }))
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
                            this.term.panel.maximized = !this.term.panel.maximized;
                            // Persist so the terminal reopens maximized next time.
                            crate::save_ui_bool("terminal_maximized", this.term.panel.maximized);
                            this.focus_active_terminal(window, cx);
                            cx.notify();
                        }),
                    ),
            );

        // The active terminal widget (entity → element).
        let body = self.term.tabs.get(self.term.panel.active).map_or_else(
            || div().into_any_element(),
            |v| v.clone().into_any_element(),
        );

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
            .when(self.term.panel.maximized, |d| d.flex_1().min_h_0())
            .when(!self.term.panel.maximized, |d| {
                d.flex_none().h(px(self.term.height))
            })
            // Inside the right column already (right of the full-height rail) → no left pad;
            // aligns with the body island above it.
            .pr(px(theme::FRAME_GAP))
            // Maximized has no body above it, so it needs the top frame gap the body used to
            // provide; docked mode gets its top spacing from the resize divider instead.
            .when(self.term.panel.maximized, |d| d.pt(px(theme::FRAME_GAP)))
            .pb(px(theme::FRAME_GAP))
            .bg(t.frame_bg)
            // No resize divider when maximized (it fills the column).
            .when(!self.term.panel.maximized, |d| d.child(divider))
            .child(island)
            .into_any_element()
    }

    /// Toggle the bottom terminal panel. Opening it spawns the first tab (lazily, so a
    /// build that never opens a terminal pays no PTY cost) and focuses it.
    pub(crate) fn act_toggle_terminal(
        &mut self,
        _: &crate::ToggleTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.terminal_is_focused(window, cx);
        match self.term.panel.toggle(focused, !self.term.tabs.is_empty()) {
            ToggleAction::Open { spawn } => {
                self.term.panel.open = true;
                if spawn {
                    self.new_terminal_tab(cx);
                }
                // Restore the persisted maximized preference on (re)open.
                self.term.panel.maximized = crate::load_ui_bool("terminal_maximized", false);
                self.focus_active_terminal(window, cx);
            }
            // Visible & focused → hide, returning focus to the app root so editor/tree
            // shortcuts (backspace = delete, etc.) work again.
            ToggleAction::Hide => {
                self.term.panel.open = false;
                self.term.panel.maximized = false;
                window.focus(&self.focus_handle);
            }
            // Visible but unfocused → just focus it (VSCode ⌃` behaviour).
            ToggleAction::FocusTerminal => self.focus_active_terminal(window, cx),
        }
        cx.notify();
    }

    /// Whether the active terminal tab currently owns keyboard focus, its find box included.
    pub(crate) fn terminal_is_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.term
            .tabs
            .get(self.term.panel.active)
            .is_some_and(|v| v.read(cx).handle().contains_focused(window, cx))
    }

    /// ⌘W while the terminal is focused: close the active tab (iTerm-style — unconditional).
    /// Scoped to the "Terminal" key context so it shadows the editor-tab ⌘W (`CloseTab`).
    pub(crate) fn act_close_terminal_tab(
        &mut self,
        _: &crate::CloseTerminalTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // ⌘W inside an Agents-view tab closes that tab, not the bottom panel's.
        if self.agents_terminal_focused(window, cx) {
            self.agents_close_visible_tab(cx);
            return;
        }
        if self.term.panel.open && !self.term.tabs.is_empty() {
            self.close_terminal_tab(self.term.panel.active, cx);
            // Keep focus on the terminal if any tab remains; otherwise the panel hid itself, so
            // hand focus back to the app root.
            if self.term.panel.open {
                self.focus_active_terminal(window, cx);
            } else {
                window.focus(&self.focus_handle);
            }
        }
    }

    /// ⌘T while the terminal is focused: open a fresh tab (panel already open) and focus it.
    pub(crate) fn act_new_terminal_tab(
        &mut self,
        _: &crate::NewTerminalTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // ⌘T inside an Agents-view tab opens another agent tab in that worktree.
        if self.agents_terminal_focused(window, cx) {
            self.agents_new_tab_here(cx);
            return;
        }
        if !self.term.panel.open {
            self.term.panel.open = true;
        }
        self.new_terminal_tab(cx);
        self.focus_active_terminal(window, cx);
        cx.notify();
    }

    /// A file path ⌘-clicked in a terminal: a text file opens in the editor (at its line, if
    /// it named one) — in its project, when it lives in one; a folder or a binary shows in
    /// Finder; a path that isn't there does nothing. The disk is read off the UI thread.
    #[cfg(feature = "terminal")]
    pub(crate) fn open_terminal_file(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        let probe = cx.background_executor().spawn({
            let path = path.clone();
            async move { probe_path(&path) }
        });
        cx.spawn(async move |_, cx| {
            let found = probe.await;
            let main = cx
                .update(|cx| cx.windows().into_iter().find_map(|w| w.downcast::<Kyde>()))
                .ok()
                .flatten();
            let Some(main) = main else {
                return;
            };
            let _ = main.update(cx, |this, window, cx| match found {
                Probe::Missing => {}
                Probe::Reveal => cx.reveal_path(&path),
                Probe::Text { root } => this.show_terminal_file(&path, root, line, window, cx),
            });
        })
        .detach();
    }

    /// Open a text file from a terminal in the editor — a tab of the open project when it's
    /// in it, else of its own project (`root`), else a loose tab.
    #[cfg(feature = "terminal")]
    fn show_terminal_file(
        &mut self,
        path: &std::path::Path,
        root: Option<PathBuf>,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let here = self.repo_root.clone().filter(|r| path.starts_with(r));
        let root = here.or(root);
        if let Some(r) = root
            .as_ref()
            .filter(|r| self.repo_root.as_ref() != Some(*r))
        {
            self.open_project(r.clone(), cx);
        }
        let rel = root
            .as_ref()
            .and_then(|r| path.strip_prefix(r).ok())
            .unwrap_or(path)
            .to_path_buf();
        self.agents_set_only(false, cx);
        self.switch_mode(Mode::Browse, cx);
        if let Some(n) = line {
            self.open_file_at_line(rel, n, window, cx);
        } else {
            self.open_file(rel, cx);
            self.focus_editor(window, cx);
        }
    }

    /// Spawn a new terminal tab rooted at the open project (or `$HOME`) and make it active.
    pub(crate) fn new_terminal_tab(&mut self, cx: &mut Context<Self>) {
        let cwd = self.repo_root.clone();
        let view = cx.new(|cx| crate::terminal::TerminalView::new(cwd, cx));
        // Repaint on title change; close the tab when its context menu's Close is hit.
        cx.subscribe(&view, |this, v, ev, cx| match ev {
            crate::terminal::TerminalEvent::TitleChanged => cx.notify(),
            crate::terminal::TerminalEvent::CloseRequested => {
                if let Some(idx) = this.term.tabs.iter().position(|t| t == &v) {
                    this.close_terminal_tab(idx, cx);
                }
            }
            // Shell output = files may have changed (mkdir/touch/git/…). Debounced:
            // each burst keeps postponing the one refresh until the output settles,
            // so the tree/status pick up terminal-made changes without refocusing.
            crate::terminal::TerminalEvent::Output => this.schedule_status_refresh(cx),
            crate::terminal::TerminalEvent::OpenFile { path, line } => {
                this.open_terminal_file(path.clone(), *line, cx);
            }
            crate::terminal::TerminalEvent::Submitted
            | crate::terminal::TerminalEvent::OpenScrollback => {}
        })
        .detach();
        self.term.tabs.push(view);
        self.term.panel.on_tab_added(self.term.tabs.len());
        cx.notify();
    }

    /// Close a terminal tab; closing the last one hides the panel. `TermPanel::on_tab_closed`
    /// owns the active-index + visibility update and flags `focus_pending` so the next paint
    /// re-homes focus (the ^D `exit` path runs in a window-less subscription, so we can't focus
    /// here; the new active tab — or the app root — is focused in `render`).
    pub(crate) fn close_terminal_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        let len = self.term.tabs.len();
        if idx >= len {
            return;
        }
        self.term.tabs.remove(idx);
        self.term.panel.on_tab_closed(len, idx);
        cx.notify();
    }

    /// Move focus to the active terminal tab's widget. Focus now AND next frame via
    /// `window.defer`: on first open the tab was just spawned this frame, so its
    /// `TerminalElement` isn't in the window tree yet and an immediate-only focus
    /// wouldn't stick (same gotcha as the finder/branch-popup focus).
    pub(crate) fn focus_active_terminal(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.term.tabs.get(self.term.panel.active) {
            let handle = view.read(cx).handle();
            window.focus(&handle);
            window.defer(cx, move |window, _cx| window.focus(&handle));
        }
    }
}

/// What a ⌘-clicked terminal path is (see `Kyde::open_terminal_file`).
#[cfg(feature = "terminal")]
#[derive(Debug, PartialEq)]
enum Probe {
    Missing,
    /// A folder, a binary, or a file too big for the editor: shown in Finder.
    Reveal,
    /// A file for the editor, and the project it's in (the nearest folder with a `.git`).
    Text {
        root: Option<PathBuf>,
    },
}

/// Bigger than this opens in Finder, not the editor (which holds a file whole).
#[cfg(feature = "terminal")]
const TERMINAL_OPEN_MAX: u64 = 8 << 20;

/// Look `path` up on disk. Blocking IO — run it off the UI thread.
#[cfg(feature = "terminal")]
fn probe_path(path: &std::path::Path) -> Probe {
    let Ok(meta) = std::fs::metadata(path) else {
        return Probe::Missing;
    };
    if !meta.is_file() || meta.len() > TERMINAL_OPEN_MAX {
        return Probe::Reveal;
    }
    // Images and fonts the editor previews; any other file with a NUL up front is binary.
    if !is_image(path) && !is_font_file(path) {
        let mut head = [0u8; 8192];
        let n = std::fs::File::open(path)
            .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
            .unwrap_or(0);
        if head[..n].contains(&0) {
            return Probe::Reveal;
        }
    }
    let root = path
        .ancestors()
        .skip(1)
        .find(|d| d.join(".git").exists())
        .map(std::path::Path::to_path_buf);
    Probe::Text { root }
}

#[cfg(all(test, feature = "terminal"))]
mod tests {
    use super::*;

    /// A ⌘-clicked path: a text file opens in the editor in its project; a binary or a
    /// folder goes to Finder; one that isn't there, nowhere.
    #[test]
    fn probe_path_sorts_editor_files_from_the_rest() {
        let base = std::env::temp_dir().join(format!("kyde-probe-{}", std::process::id()));
        let src = base.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(base.join(".git")).unwrap();
        std::fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(src.join("app"), b"\x7fELF\0\0\0").unwrap();

        assert_eq!(
            probe_path(&src.join("main.rs")),
            Probe::Text {
                root: Some(base.clone())
            }
        );
        assert_eq!(probe_path(&src.join("app")), Probe::Reveal);
        assert_eq!(probe_path(&src), Probe::Reveal);
        assert_eq!(probe_path(&src.join("gone.rs")), Probe::Missing);
        let _ = std::fs::remove_dir_all(&base);
    }
}
