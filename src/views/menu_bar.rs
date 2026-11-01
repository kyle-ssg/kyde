//! The menu bar on Linux. Where the window manager draws the title bar, the menus (File / View
//! / Help, `menu_bar_menus`) sit in a row at the left of Kyde's title strip instead of behind
//! its ☰ button, with the project switcher to their right: one row, not a menu bar stacked over
//! the strip. Settings → Appearance → Menu bar picks the bar, the button, or (the default)
//! whichever the desktop calls for (see `chrome`).
//!
//! A click on a title opens its menu under it; while one is open, the pointer crossing another
//! title switches to that one and ← / → step between them. F10 opens the first menu (the ☰
//! menu where the menus sit behind the button), Alt + a title's underlined letter opens its
//! menu, and the letters are underlined while Alt is held. Each menu is the context-menu panel
//! (`context_menu`), so ↑ / ↓ / ⏎ / ⎋ work as in every other menu.
//!
//! Crate-root child module.

use crate::*;

/// Gap between a title and the top of its menu.
const MENU_GAP: f32 = 2.0;

impl Kyde {
    /// The menu bar: one title per menu, the open one lit. Records where each title lands, so a
    /// menu opens right under its title.
    pub(crate) fn menu_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let open = self.open_bar_menu();
        let titles = self.chrome.menu_titles.clone();
        let underline = gpui::HighlightStyle {
            underline: Some(gpui::UnderlineStyle {
                thickness: px(1.0),
                color: None,
                wavy: false,
            }),
            ..Default::default()
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .font_family(theme::font::ui_family())
            .text_size(px(t.ui_font_size))
            .on_children_prepainted(move |bounds, _w, _cx| *titles.borrow_mut() = bounds)
            .children(MENU_BAR.iter().enumerate().map(|(i, title)| {
                let lit = open == Some(i);
                let mut text = gpui::StyledText::new(*title);
                if self.chrome.mnemonics {
                    let key = title.chars().next().map_or(0, char::len_utf8);
                    text = text.with_highlights([(0..key, underline)]);
                }
                div()
                    .id(SharedString::from(format!("menu-bar-{i}")))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(t.text)
                    .when(lit, |d| d.bg(t.selected_bg))
                    .when(!lit, |d| d.hover(|d| d.bg(t.bg_mid)))
                    .child(text)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            cx.stop_propagation();
                            this.open_bar_menu_at(i, false, cx);
                        }),
                    )
            }))
            .into_any_element()
    }

    /// The menu bar's keys on a window root (the project view and the Projects views): F10,
    /// Alt + an access key, and Alt held underlining the access keys.
    pub(crate) fn with_menu_bar_keys(&self, root: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        root.on_action(cx.listener(Self::act_menu_key))
            .on_action(cx.listener(Self::act_menu_bar_open))
            .on_modifiers_changed(
                cx.listener(|this, e: &gpui::ModifiersChangedEvent, _w, cx| {
                    // The next frame reads Alt from the window (`WindowChrome::update`).
                    if this.menu_bar_shown() && e.modifiers.alt != this.chrome.mnemonics {
                        cx.notify();
                    }
                }),
            )
    }

    /// The menu bar's menu open now, if any.
    pub(crate) fn open_bar_menu(&self) -> Option<usize> {
        match self.context_menu.as_ref()?.target {
            MenuTarget::MenuBar(i) => Some(i),
            _ => None,
        }
    }

    /// Open the menu bar's `i`th menu (wrapping round) under its title, in place of any menu
    /// open in the window; `by_key` lights its first item.
    pub(crate) fn open_bar_menu_at(&mut self, i: usize, by_key: bool, cx: &mut Context<Self>) {
        let i = i % MENU_BAR.len();
        let at = self.chrome.menu_titles.borrow().get(i).map_or_else(
            || gpui::point(px(chrome::TITLE_INSET), px(36.0)),
            |b| b.bottom_left() + gpui::point(px(0.0), px(MENU_GAP)),
        );
        // Over a menu already open here, the panel keeps keyboard focus (one handle serves
        // every menu) and where it hands focus back to on close.
        let replacing = self
            .context_menu
            .as_ref()
            .is_some_and(|m| m.target.host().is_none());
        let pending = self.menu_focus_pending;
        if by_key {
            self.open_menu_by_key(at, MenuTarget::MenuBar(i), cx);
        } else {
            self.open_menu(at, MenuTarget::MenuBar(i), cx);
        }
        if replacing {
            self.menu_focus_pending = pending;
        }
    }

    /// ← / → in one of the bar's menus, past any flyout: the menu beside it, first item lit.
    /// False when no menu of the bar is open.
    pub(crate) fn step_bar_menu(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let Some(i) = self.open_bar_menu() else {
            return false;
        };
        let next = if forward {
            i + 1
        } else {
            i + MENU_BAR.len() - 1
        };
        self.open_bar_menu_at(next, true, cx);
        true
    }

    /// The pointer at `pos` while one of the bar's menus is open (over the menu's backdrop,
    /// which covers the titles): on another title, switch to its menu. False when it isn't.
    pub(crate) fn track_bar_menu(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(open) = self.open_bar_menu() else {
            return false;
        };
        let over = self
            .chrome
            .menu_titles
            .borrow()
            .iter()
            .position(|b| b.contains(&pos));
        match over {
            Some(i) if i != open => {
                self.open_bar_menu_at(i, false, cx);
                true
            }
            _ => false,
        }
    }

    /// F10: open the first menu — the ☰ menu where the menus sit behind the button — or close
    /// the menu it opened.
    fn act_menu_key(&mut self, _: &MenuKey, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.context_menu.as_ref().map(|m| &m.target);
        if matches!(open, Some(MenuTarget::MenuBar(_) | MenuTarget::AppMenu)) {
            self.close_menu_restoring(window, cx);
        } else if self.menu_bar_shown() {
            self.open_bar_menu_at(0, true, cx);
        } else {
            // Under the ☰ button: at the strip's right end below a window manager's title bar,
            // else at its left (`render_app_menu_button`).
            let x = if self.app_menu_at_right() {
                window.viewport_size().width - px(chrome::TITLE_INSET)
            } else {
                px(chrome::TITLE_INSET)
            };
            self.open_menu_by_key(gpui::point(x, px(36.0)), MenuTarget::AppMenu, cx);
        }
    }

    /// Alt + a title's access key: open that menu (or close it, if it's the one open). Where
    /// the menus sit behind the ☰ button, the key goes on to whatever has focus.
    fn act_menu_bar_open(&mut self, a: &MenuBarOpen, window: &mut Window, cx: &mut Context<Self>) {
        if !self.menu_bar_shown() {
            cx.propagate();
        } else if self.open_bar_menu() == Some(a.0) {
            self.close_menu_restoring(window, cx);
        } else {
            self.open_bar_menu_at(a.0, true, cx);
        }
    }
}
