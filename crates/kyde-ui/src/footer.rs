//! Shared modal/window footer rows. Every confirm dialog and native modal window ends in
//! the same right-aligned button row (Cancel + primary); these keep the layout in one
//! place. Like `btn_*`, callers chain their own children (the buttons) and any extras.

use gpui::prelude::*;
use gpui::{div, Div};
use kyde_theme as theme;

/// Right-aligned button row for an in-panel modal footer (delete confirm, name prompt, …):
/// `flex_row` + `justify_end` + `gap_2`. Chain the buttons as children.
pub fn modal_footer() -> Div {
    div().flex().flex_row().justify_end().gap_2()
}

/// Bottom action bar of a native modal *window* (Push, Rollback, …): a [`modal_footer`]
/// with the shared padding + top hairline divider.
pub fn window_footer() -> Div {
    modal_footer()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(theme::get().divider)
}
