//! Checkbox box primitive (the square tick used in commit/rollback trees).
use crate::*;

/// The app's standard checkbox: a small rounded square, filled with `check.svg` when ticked.
/// Used by the tree rows and the rollback modal — never an emoji glyph.
pub(crate) fn checkbox_box(checked: bool) -> gpui::Div {
    let t = theme::get();
    let b = div()
        .flex_none()
        .size(px(15.0))
        .rounded_sm()
        .border_1()
        .flex()
        .items_center()
        .justify_center();
    if checked {
        b.bg(t.primary).border_color(t.primary).child(
            svg()
                .path("icons/check.svg")
                .size(px(11.0))
                .text_color(t.primary_text),
        )
    } else {
        b.border_color(t.line_number)
    }
}
