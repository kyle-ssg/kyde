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
        for (i, view) in self.term_tabs.iter().enumerate() {
            let active = i == self.term_panel.active;
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
                            this.term_panel.active = i;
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
        let maxed = self.term_panel.maximized;
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
                            this.term_panel.open = false;
                            this.term_panel.maximized = false;
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
                            this.term_panel.maximized = !this.term_panel.maximized;
                            // Persist so the terminal reopens maximized next time.
                            crate::save_ui_bool("terminal_maximized", this.term_panel.maximized);
                            this.focus_active_terminal(window, cx);
                            cx.notify();
                        }),
                    ),
            );

        // The active terminal widget (entity → element).
        let body = self.term_tabs.get(self.term_panel.active).map_or_else(
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
            .when(self.term_panel.maximized, |d| d.flex_1().min_h_0())
            .when(!self.term_panel.maximized, |d| {
                d.flex_none().h(px(self.term_height))
            })
            // Inside the right column already (right of the full-height rail) → no left pad;
            // aligns with the body island above it.
            .pr(px(theme::FRAME_GAP))
            // Maximized has no body above it, so it needs the top frame gap the body used to
            // provide; docked mode gets its top spacing from the resize divider instead.
            .when(self.term_panel.maximized, |d| d.pt(px(theme::FRAME_GAP)))
            .pb(px(theme::FRAME_GAP))
            .bg(t.frame_bg)
            // No resize divider when maximized (it fills the column).
            .when(!self.term_panel.maximized, |d| d.child(divider))
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
        match self.term_panel.toggle(focused, !self.term_tabs.is_empty()) {
            ToggleAction::Open { spawn } => {
                self.term_panel.open = true;
                if spawn {
                    self.new_terminal_tab(cx);
                }
                // Restore the persisted maximized preference on (re)open.
                self.term_panel.maximized = crate::load_ui_bool("terminal_maximized", false);
                self.focus_active_terminal(window, cx);
            }
            // Visible & focused → hide, returning focus to the app root so editor/tree
            // shortcuts (backspace = delete, etc.) work again.
            ToggleAction::Hide => {
                self.term_panel.open = false;
                self.term_panel.maximized = false;
                window.focus(&self.focus_handle);
            }
            // Visible but unfocused → just focus it (VSCode ⌃` behaviour).
            ToggleAction::FocusTerminal => self.focus_active_terminal(window, cx),
        }
        cx.notify();
    }

    /// Whether the active terminal tab currently owns keyboard focus.
    pub(crate) fn terminal_is_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.term_tabs
            .get(self.term_panel.active)
            .is_some_and(|v| v.read(cx).handle().is_focused(window))
    }

    /// ⌘W while the terminal is focused: close the active tab (iTerm-style — unconditional).
    /// Scoped to the "Terminal" key context so it shadows the editor-tab ⌘W (`CloseTab`).
    pub(crate) fn act_close_terminal_tab(
        &mut self,
        _: &crate::CloseTerminalTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.term_panel.open && !self.term_tabs.is_empty() {
            self.close_terminal_tab(self.term_panel.active, cx);
            // Keep focus on the terminal if any tab remains; otherwise the panel hid itself, so
            // hand focus back to the app root.
            if self.term_panel.open {
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
        if !self.term_panel.open {
            self.term_panel.open = true;
        }
        self.new_terminal_tab(cx);
        self.focus_active_terminal(window, cx);
        cx.notify();
    }

    /// Spawn a new terminal tab rooted at the open project (or `$HOME`) and make it active.
    pub(crate) fn new_terminal_tab(&mut self, cx: &mut Context<Self>) {
        let cwd = self.repo_root.clone();
        let view = cx.new(|cx| crate::terminal::TerminalView::new(cwd, cx));
        // Repaint on title change; close the tab when its context menu's Close is hit.
        cx.subscribe(&view, |this, v, ev, cx| match ev {
            crate::terminal::TerminalEvent::TitleChanged => cx.notify(),
            crate::terminal::TerminalEvent::CloseRequested => {
                if let Some(idx) = this.term_tabs.iter().position(|t| t == &v) {
                    this.close_terminal_tab(idx, cx);
                }
            }
        })
        .detach();
        self.term_tabs.push(view);
        self.term_panel.on_tab_added(self.term_tabs.len());
        cx.notify();
    }

    /// Close a terminal tab; closing the last one hides the panel. `TermPanel::on_tab_closed`
    /// owns the active-index + visibility update and flags `focus_pending` so the next paint
    /// re-homes focus (the ^D `exit` path runs in a window-less subscription, so we can't focus
    /// here; the new active tab — or the app root — is focused in `render`).
    pub(crate) fn close_terminal_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        let len = self.term_tabs.len();
        if idx >= len {
            return;
        }
        self.term_tabs.remove(idx);
        self.term_panel.on_tab_closed(len, idx);
        cx.notify();
    }

    /// Move focus to the active terminal tab's widget. Focus now AND next frame via
    /// `window.defer`: on first open the tab was just spawned this frame, so its
    /// `TerminalElement` isn't in the window tree yet and an immediate-only focus
    /// wouldn't stick (same gotcha as the finder/branch-popup focus).
    pub(crate) fn focus_active_terminal(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.term_tabs.get(self.term_panel.active) {
            let handle = view.read(cx).handle();
            window.focus(&handle);
            window.defer(cx, move |window, _cx| window.focus(&handle));
        }
    }
}
