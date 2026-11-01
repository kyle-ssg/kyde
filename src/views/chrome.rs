//! Window chrome off macOS. On macOS the system draws the traffic lights over Kyde's
//! transparent title strip and shows Kyde's menus in the menu bar and the Dock. Linux has
//! neither, so there:
//!
//! - **The menus** — everything in the menu bar (`app_menus`: Open…, Recent Projects,
//!   Settings…, Plugins…, What's New…, Toggle FPS Monitor, Clear Data & Restart…, Quit) sit
//!   behind a menu button in the title strip. On Linux, where the window manager draws the
//!   title bar, they are a menu bar at the strip's left instead (`menu_bar`; Settings →
//!   Appearance → Menu bar picks either).
//! - **The title bar** — on X11, and on a Wayland compositor that decorates windows itself,
//!   the window manager draws it, so Kyde's strip is a plain toolbar row under it. On Wayland
//!   Kyde asks for client-side decorations (GNOME draws none for Wayland apps); with them the
//!   strip becomes the title bar: drag to move, double-click to maximize, right-click for the
//!   window menu, minimize / maximize / close at its right, and resize handles along the
//!   window's edges. Modal windows get a slim title strip of their own.
//!
//! Crate-root child module.

use crate::*;
use std::cell::Cell;
use std::rc::Rc;

/// Left inset of a title strip: room for the macOS traffic lights; elsewhere a margin before
/// the menu button.
pub(crate) const TITLE_INSET: f32 = if cfg!(target_os = "macos") { 84.0 } else { 6.0 };

/// Height of a modal window's own title strip (client-side decorations only).
pub(crate) const MODAL_STRIP_H: f32 = 34.0;

/// Width of the invisible resize band along each edge; corners take twice that.
const RESIZE_BAND: f32 = 5.0;

/// The main window's chrome, refreshed from the window each frame (see `Kyde::render`).
#[derive(Default)]
pub(crate) struct WindowChrome {
    /// `Some` while the window draws its own title bar (client-side decorations), with which
    /// of its edges are tiled (maximized / snapped edges get no resize handle).
    pub client: Option<gpui::Tiling>,
    /// Whether the window is maximized (the middle control shows Restore).
    pub maximized: bool,
    /// Which window controls the compositor supports.
    pub controls: gpui::WindowControls,
    /// A left press on the title strip that a drag would move the window from (see
    /// `drag_area`). Shared with the strip's handlers, so it survives the re-render a press
    /// triggers.
    pub drag_armed: Rc<Cell<bool>>,
    /// Where the menus live: a menu bar or the menu button (Settings → Appearance).
    #[cfg(target_os = "linux")]
    pub cfg: kyde_config::window::WindowCfg,
    /// Where the menu bar's titles were laid out (window coordinates), so a menu opens under
    /// its title and the pointer crossing a title switches to its menu.
    #[cfg(target_os = "linux")]
    pub menu_titles: Rc<std::cell::RefCell<Vec<Bounds<Pixels>>>>,
    /// Alt is held in the active window: the menu bar underlines its titles' access keys.
    #[cfg(target_os = "linux")]
    pub mnemonics: bool,
}

impl WindowChrome {
    /// The chrome of a new main window, with the saved menu bar choice (Linux).
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            cfg: kyde_config::window::WindowCfg::load(),
            ..Self::default()
        }
    }

    pub(crate) fn update(&mut self, window: &Window) {
        // macOS draws its own title bar: nothing to track (and no per-frame AppKit calls).
        if cfg!(target_os = "macos") {
            return;
        }
        self.client = client_decorations(window);
        self.maximized = window.is_maximized();
        self.controls = window.window_controls();
        #[cfg(target_os = "linux")]
        {
            self.mnemonics = window.is_window_active() && window.modifiers().alt;
        }
    }
}

/// Decorations to ask for when opening a window: client-side on Wayland (see the module
/// docs); otherwise gpui's default — server-side, the window manager's own title bar.
pub(crate) fn requested_decorations() -> Option<gpui::WindowDecorations> {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if gpui::guess_compositor() == "Wayland" {
        return Some(gpui::WindowDecorations::Client);
    }
    None
}

/// `Some(tiling)` when `window` draws its own decorations. Never on macOS, whose title bar is
/// always the system's.
pub(crate) fn client_decorations(window: &Window) -> Option<gpui::Tiling> {
    if cfg!(target_os = "macos") {
        return None;
    }
    match window.window_decorations() {
        gpui::Decorations::Client { tiling } => Some(tiling),
        gpui::Decorations::Server => None,
    }
}

/// The empty, draggable part of a title strip. On macOS the system moves the window (the
/// area is a `WindowControlArea::Drag` under a transparent titlebar) and a double-click zooms
/// (maximizes, matching macOS — not fullscreen). Under client-side decorations (`client`), a
/// press-and-drag moves the window, a double-click maximizes and a right-click opens the
/// window menu. Under the window manager's own title bar it is a plain spacer.
pub(crate) fn drag_area(client: bool, armed: &Rc<Cell<bool>>) -> gpui::Div {
    let area = div()
        .flex_1()
        .h_full()
        .window_control_area(gpui::WindowControlArea::Drag);
    if cfg!(target_os = "macos") {
        return area.on_mouse_down(
            MouseButton::Left,
            |e: &gpui::MouseDownEvent, window, _cx| {
                if e.click_count >= 2 {
                    window.zoom_window();
                }
            },
        );
    }
    if !client {
        return area;
    }
    // The move starts on the first motion after a press (not on the press itself), so a
    // double-click still reaches us instead of the compositor's move grab.
    let (down, up, out, moved) = (armed.clone(), armed.clone(), armed.clone(), armed.clone());
    area.on_mouse_down(
        MouseButton::Left,
        move |e: &gpui::MouseDownEvent, window, _cx| {
            down.set(e.click_count < 2);
            if e.click_count >= 2 {
                window.zoom_window();
            }
        },
    )
    .on_mouse_up(MouseButton::Left, move |_e, _w, _cx| up.set(false))
    .on_mouse_down_out(move |_e, _w, _cx| out.set(false))
    .on_mouse_move(move |e: &gpui::MouseMoveEvent, window, _cx| {
        if moved.replace(false) && e.pressed_button == Some(MouseButton::Left) {
            window.start_window_move();
        }
    })
    .on_mouse_down(
        MouseButton::Right,
        |e: &gpui::MouseDownEvent, window, _cx| {
            window.show_window_menu(e.position);
        },
    )
}

/// One title-bar control button: an icon, its tooltip, what a click does.
fn control_btn(
    id: &'static str,
    icon: &'static str,
    tip: &'static str,
    run: impl Fn(&mut Window) + 'static,
) -> gpui::Stateful<gpui::Div> {
    ui::icon_btn(id, 28.0)
        .tooltip(ui::tip(tip))
        .child(
            svg()
                .path(icon)
                .size(px(15.0))
                .text_color(theme::get().secondary_text),
        )
        .on_click(move |_e, window, _cx| run(window))
}

/// Minimize / maximize (or restore) / close, at the right of a window that draws its own
/// title bar. Close removes the window, as the window manager's close button does.
pub(crate) fn window_controls(maximized: bool, controls: gpui::WindowControls) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_1()
        .px_2()
        .when(controls.minimize, |d| {
            d.child(control_btn(
                "win-minimize",
                "icons/minus.svg",
                "Minimize",
                |w| w.minimize_window(),
            ))
        })
        .when(controls.maximize, |d| {
            let (icon, tip) = if maximized {
                ("icons/copy.svg", "Restore")
            } else {
                ("icons/square.svg", "Maximize")
            };
            d.child(control_btn("win-maximize", icon, tip, |w| w.zoom_window()))
        })
        .child(control_btn("win-close", "icons/x.svg", "Close", |w| {
            w.remove_window();
        }))
}

/// Invisible resize handles along the edges and corners of a window that draws its own
/// decorations (`client`), on top of everything else in it. Tiled edges get none. Empty
/// under the window manager's decorations.
pub(crate) fn resize_handles(client: Option<gpui::Tiling>) -> Vec<gpui::AnyElement> {
    use gpui::{CursorStyle as C, ResizeEdge as E};
    let Some(tiled) = client else {
        return Vec::new();
    };
    let (b, c) = (px(RESIZE_BAND), px(RESIZE_BAND * 2.0));
    let band = |edge: E, cursor: C| {
        div().absolute().occlude().cursor(cursor).on_mouse_down(
            MouseButton::Left,
            move |_e, window, cx| {
                cx.stop_propagation();
                window.start_window_resize(edge);
            },
        )
    };
    let mut out = Vec::new();
    let mut add = |on: bool, d: gpui::Div| {
        if on {
            out.push(d.into_any_element());
        }
    };
    add(
        !tiled.top,
        band(E::Top, C::ResizeUpDown).top_0().left(c).right(c).h(b),
    );
    add(
        !tiled.bottom,
        band(E::Bottom, C::ResizeUpDown)
            .bottom_0()
            .left(c)
            .right(c)
            .h(b),
    );
    add(
        !tiled.left,
        band(E::Left, C::ResizeLeftRight)
            .left_0()
            .top(c)
            .bottom(c)
            .w(b),
    );
    add(
        !tiled.right,
        band(E::Right, C::ResizeLeftRight)
            .right_0()
            .top(c)
            .bottom(c)
            .w(b),
    );
    add(
        !tiled.top && !tiled.left,
        band(E::TopLeft, C::ResizeUpLeftDownRight)
            .top_0()
            .left_0()
            .size(c),
    );
    add(
        !tiled.top && !tiled.right,
        band(E::TopRight, C::ResizeUpRightDownLeft)
            .top_0()
            .right_0()
            .size(c),
    );
    add(
        !tiled.bottom && !tiled.left,
        band(E::BottomLeft, C::ResizeUpRightDownLeft)
            .bottom_0()
            .left_0()
            .size(c),
    );
    add(
        !tiled.bottom && !tiled.right,
        band(E::BottomRight, C::ResizeUpLeftDownRight)
            .bottom_0()
            .right_0()
            .size(c),
    );
    out
}

/// A modal window's own title strip under client-side decorations: its title (draggable,
/// like a title bar) and a close button.
pub(crate) fn modal_title_strip(title: SharedString, armed: &Rc<Cell<bool>>) -> gpui::Div {
    let t = theme::get();
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .h(px(MODAL_STRIP_H))
        .pl(px(36.0))
        .bg(t.titlebar_bg)
        .child(
            drag_area(true, armed)
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.secondary_text)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(div().flex_none().pr_1().child(control_btn(
            "modal-close",
            "icons/x.svg",
            "Close",
            Window::remove_window,
        )))
}

/// The menu bar's menus as one in-window menu (off macOS): the last menu (File) first and the
/// app menu last, so Quit ends the list; a submenu (Recent Projects) becomes a flyout. Each
/// item dispatches the menu bar's own action, so the two can't drift apart.
pub(crate) fn app_menu_entries(menus: Vec<Menu>) -> Vec<MenuEntry> {
    let mut out = Vec::new();
    let mut flyouts = 0;
    for menu in menus.into_iter().rev() {
        push_divider(&mut out);
        push_items(&mut out, menu.items, &mut flyouts);
    }
    while matches!(out.last(), Some(MenuEntry::Divider)) {
        out.pop();
    }
    out
}

/// One menu of the menu bar on Linux (`menu_bar`) as an in-window menu, built the same way.
#[cfg(target_os = "linux")]
pub(crate) fn menu_entries(menu: Menu) -> Vec<MenuEntry> {
    app_menu_entries(vec![menu])
}

/// Append `items` as menu entries (no leading or doubled hairlines); `flyouts` numbers the
/// submenus so each opens on its own.
fn push_items(out: &mut Vec<MenuEntry>, items: Vec<MenuItem>, flyouts: &mut usize) {
    for item in items {
        match item {
            MenuItem::Separator => push_divider(out),
            MenuItem::Action { name, action, .. } => {
                out.push(MenuEntry::item(name, move |_this, window, cx| {
                    // Deferred to after this handler, so the action reaches `Kyde`'s
                    // listeners (or the app's, for Quit) with no update in flight.
                    window.dispatch_action(action.boxed_clone(), cx);
                }));
            }
            MenuItem::Submenu(sub) => {
                let kind = Submenu::App(*flyouts);
                *flyouts += 1;
                let mut inner = Vec::new();
                push_items(&mut inner, sub.items, flyouts);
                while matches!(inner.last(), Some(MenuEntry::Divider)) {
                    inner.pop();
                }
                out.push(MenuEntry::Submenu {
                    kind,
                    icon_label: sub.name.clone(),
                    label: sub.name,
                    items: inner,
                });
            }
            MenuItem::SystemMenu(_) => {}
        }
    }
}

/// A hairline, unless the list is empty or already ends in one.
fn push_divider(out: &mut Vec<MenuEntry>) {
    if !out.is_empty() && !matches!(out.last(), Some(MenuEntry::Divider)) {
        out.push(MenuEntry::Divider);
    }
}

impl Kyde {
    /// Whether the menu button sits at the RIGHT end of the title strip: off macOS, when the
    /// window manager draws the title bar (X11). Cinnamon puts its own window-menu button at
    /// that title bar's left, right above where Kyde's would be. Under client-side decorations
    /// (Wayland) the strip is the title bar and the button stays at its left.
    pub(crate) fn app_menu_at_right(&self) -> bool {
        !cfg!(target_os = "macos") && self.chrome.client.is_none()
    }

    /// Whether the menus show as a menu bar on Linux (`menu_bar`), not behind the menu button:
    /// the Settings choice, which by default follows whether the window manager draws the
    /// title bar.
    #[cfg(target_os = "linux")]
    pub(crate) fn menu_bar_shown(&self) -> bool {
        self.chrome
            .cfg
            .menu_bar
            .shows_bar(self.chrome.client.is_none())
    }

    /// The menu bar, where it shows (Linux only).
    #[cfg(target_os = "linux")]
    pub(crate) fn render_menu_bar(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        self.menu_bar_shown().then(|| self.menu_bar(cx))
    }

    /// The menu bar, where it shows (Linux only).
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn render_menu_bar(&self, _cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        None
    }

    /// The menu button of a title strip `strip_h` tall, off macOS (where the menu bar holds
    /// these): opens the menu bar's items as an in-window menu just below it.
    pub(crate) fn render_app_menu_button(
        &self,
        strip_h: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if cfg!(target_os = "macos") {
            return None;
        }
        #[cfg(target_os = "linux")]
        if self.menu_bar_shown() {
            return None;
        }
        Some(
            // No tooltip: it would sit over the menu it opens.
            ui::icon_btn("app-menu", 28.0)
                .child(
                    svg()
                        .path("icons/menu.svg")
                        .size(px(16.0))
                        .text_color(theme::get().secondary_text),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        // At the right end the menu hangs from the click; the menu snaps back
                        // inside the window (render_context_menu).
                        let x = if this.app_menu_at_right() {
                            e.position.x
                        } else {
                            px(TITLE_INSET)
                        };
                        let at = gpui::point(x, px(strip_h - 4.0));
                        this.open_menu(at, MenuTarget::AppMenu, cx);
                    }),
                )
                .into_any_element(),
        )
    }

    /// A title strip with nothing of Kyde's in it (the Projects views): on macOS just the
    /// draggable area under the traffic lights; elsewhere the menu button, the drag area and,
    /// under client-side decorations, the window controls.
    pub(crate) fn render_bare_title_strip(&self, h: f32, cx: &mut Context<Self>) -> gpui::Div {
        let strip = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .h(px(h))
            .w_full();
        if cfg!(target_os = "macos") {
            return strip.child(
                div()
                    .size_full()
                    .window_control_area(gpui::WindowControlArea::Drag),
            );
        }
        let client = self.chrome.client;
        let menu = self.render_app_menu_button(h, cx);
        let (left, right) = if self.app_menu_at_right() {
            (None, menu)
        } else {
            (menu, None)
        };
        strip
            .pl(px(TITLE_INSET))
            .children(left)
            .children(self.render_menu_bar(cx))
            .child(drag_area(client.is_some(), &self.chrome.drag_armed))
            .when(right.is_some(), |d| d.pr(px(TITLE_INSET)))
            .children(right)
            .when(client.is_some(), |d| {
                d.child(window_controls(self.chrome.maximized, self.chrome.controls))
            })
    }

    /// What a Projects-view root needs for its title strip, off macOS: the menu button's (or
    /// the menu bar's) menu when open, the window's resize handles under client-side
    /// decorations, and handlers for the menu's actions — this view has none of its own, and
    /// the in-window menu is the only way to reach them (macOS greys them out in its menu bar
    /// here instead).
    pub(crate) fn with_title_strip_extras(
        &self,
        root: gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        if cfg!(target_os = "macos") {
            return root;
        }
        #[cfg(target_os = "linux")]
        let root = self.with_menu_bar_keys(root, cx);
        root.on_action(cx.listener(Self::act_open_project))
            .on_action(cx.listener(Self::open_recent_project))
            .on_action(cx.listener(Self::open_keymap))
            .on_action(cx.listener(Self::act_open_plugins))
            .on_action(cx.listener(Self::act_open_changelog))
            .on_action(cx.listener(Self::act_toggle_fps))
            .on_action(cx.listener(Self::act_clear_data))
            .when(
                self.context_menu
                    .as_ref()
                    .is_some_and(|m| m.target.host().is_none()),
                |d| d.child(self.render_context_menu(cx)),
            )
            .children(resize_handles(self.chrome.client))
    }
}
