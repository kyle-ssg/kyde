//! Shared modal/dialog panel scaffolding — the chrome + text rows that every confirm
//! dialog, prompt, and empty-state island repeated by hand (issue #64). Like `btn_*` and
//! `modal_footer`, callers chain their own children (title/body/footer, inputs, buttons).

use gpui::prelude::*;
use gpui::{div, px, Div, SharedString};
use kyde_theme as theme;

/// Overlay confirm/prompt panel chrome (delete confirm, rename prompt): a fixed-width
/// rounded island on `frame_bg` with a divider border + shadow, UI font, one-step-larger
/// text, and `occlude`d so clicks don't bleed through. Chain [`dialog_title`] /
/// [`dialog_body`] / [`crate::modal_footer`] as children (and any `key_context`/`on_action`).
pub fn modal_panel(width: f32, ui: &'static str) -> Div {
    let t = theme::get();
    div()
        .w(px(width))
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .bg(t.frame_bg)
        .border_1()
        .border_color(t.divider)
        .rounded(px(theme::ISLAND_RADIUS))
        .shadow_lg()
        .font_family(ui)
        .text_size(px(t.ui_font_size + 1.0))
        .occlude()
}

/// A dialog's title line (primary text color). Font size is inherited from the panel.
pub fn dialog_title(title: impl Into<SharedString>) -> Div {
    div().text_color(theme::get().text).child(title.into())
}

/// A dialog's muted body paragraph: `flex_1` (grows to push a footer to the bottom) +
/// `secondary_text` at the base UI size. For the wrap-and-fill descriptions in confirm
/// dialogs (delete, clear data, clear local history).
pub fn dialog_body(text: impl Into<SharedString>) -> Div {
    let t = theme::get();
    div()
        .flex_1()
        .text_color(t.secondary_text)
        .text_size(px(t.ui_font_size))
        .child(text.into())
}

/// The body of a native modal *window* confirm dialog (Clear Data, Clear Local History):
/// a `size_full` column of [`dialog_title`] + [`dialog_body`]. The window itself provides
/// the chrome + titlebar; chain [`crate::modal_footer`] (Cancel + primary) as the last child.
pub fn confirm_body(title: impl Into<SharedString>, description: impl Into<SharedString>) -> Div {
    let t = theme::get();
    div()
        .size_full()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .font_family(theme::font::UI_FAMILY)
        .text_size(px(t.ui_font_size + 1.0))
        .child(dialog_title(title))
        .child(dialog_body(description))
}

/// A centered empty-state island (e.g. "You have nothing to commit", "Select a file"): the
/// panel fills its slot on `main_bg`, rounded like the other islands, with the muted
/// `line_number` message centered. Chain extra children (icon, button) for richer states.
pub fn empty_state(message: impl Into<SharedString>, ui: &'static str) -> Div {
    let t = theme::get();
    div()
        .flex()
        .flex_1()
        .h_full()
        .items_center()
        .justify_center()
        .bg(t.main_bg)
        .rounded(px(theme::ISLAND_RADIUS))
        .font_family(ui)
        .text_size(px(t.ui_font_size + 1.0))
        .text_color(t.line_number)
        .child(message.into())
}
