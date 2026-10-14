//! Primary / secondary buttons. Caller chains `.on_mouse_down(...)`. See the "Buttons" UI
//! principle in CLAUDE.md.
use gpui::prelude::*;
use gpui::{div, SharedString};
use kyde_theme as theme;

/// Standard **primary** button (accent fill + primary text).
pub fn btn_primary(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    btn_primary_state(id, label, false)
}

/// `btn_primary` with an explicit `disabled` state. gpui allows only ONE `.hover()` per
/// element (a second one panics: "hover style already set"), so the disabled look — dimmed,
/// and *staying* dimmed on hover — must be baked into the single hover here.
pub fn btn_primary_state(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    disabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    let b = div()
        .id(id)
        .px_4()
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

/// Standard **secondary** button (transparent fill + divider border + secondary text).
pub fn btn_secondary(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    btn_secondary_state(id, label, false)
}

/// `btn_secondary` with an explicit `disabled` state (dimmed, no hover — see
/// [`btn_primary_state`] for why the look lives in the single `.hover()`).
pub fn btn_secondary_state(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    disabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    let b = div()
        .id(id)
        .px_4()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(t.divider)
        .text_color(t.secondary_text);
    let b = if disabled {
        b.opacity(0.6).cursor_default().hover(|s| s.opacity(0.6))
    } else {
        b.cursor_pointer().hover(|s| s.bg(t.bg_mid))
    };
    b.child(label.into())
}

/// A small square icon / glyph button — the `−` / `»` panel toggles, terminal-strip and
/// toolbar icons: centred content, muted until hovered. The caller sets the glyph's text
/// size (or an `svg` child) and chains the child, tooltip and handler.
pub fn icon_btn(id: impl Into<gpui::ElementId>, size: f32) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(gpui::px(size))
        .rounded_md()
        .text_color(t.line_number)
        .hover(|s| s.bg(t.bg_light).text_color(t.text))
        .cursor_pointer()
}
