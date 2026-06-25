//! Tab pill (the git view's Commit/Push tabs etc.). Caller chains `.on_mouse_down(...)`.
use crate::*;

/// One pill of a tab strip, IntelliJ-style: active = subtle filled bg + faint border; inactive
/// = transparent with a hover bg. A `count` badge shows when > 0 (accent-filled when active).
pub(crate) fn tab_pill(
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
                .text_color(if active { t.primary_text } else { t.secondary_text })
                .child(SharedString::from(count.to_string())),
        );
    }
    d
}
