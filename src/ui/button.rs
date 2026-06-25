//! Primary / secondary buttons. Caller chains `.on_mouse_down(...)`. See the "Buttons" UI
//! principle in CLAUDE.md.
use crate::*;

/// Standard **primary** button (accent fill + primary text).
pub(crate) fn btn_primary(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    btn_primary_state(id, label, false)
}

/// `btn_primary` with an explicit `disabled` state. gpui allows only ONE `.hover()` per
/// element (a second one panics: "hover style already set"), so the disabled look — dimmed,
/// and *staying* dimmed on hover — must be baked into the single hover here.
pub(crate) fn btn_primary_state(
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
pub(crate) fn btn_secondary(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    div()
        .id(id)
        .px_4()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(t.divider)
        .text_color(t.secondary_text)
        .cursor_pointer()
        .hover(|s| s.bg(t.bg_mid))
        .child(label.into())
}
