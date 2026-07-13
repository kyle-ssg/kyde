//! Shared list-picker mechanics: bounded index navigation + the selected/hover row pill.
//!
//! Every picker-style list in the app (fuzzy finder, history commit list, history branch
//! dropdown, push file list) hand-rolled the same two things: an Up/Down selection index
//! clamped to the list, and a row that fills `selected_bg` when selected and a hover shade
//! otherwise. Centralising them keeps the keyboard feel and the look consistent. Like
//! `btn_primary`/`btn_secondary`, [`row`] returns the base element — the caller chains its
//! own padding, layout, children, and `.on_mouse_down(...)`.

use gpui::prelude::*;
use gpui::{div, Div, ElementId, Stateful};
use kyde_theme as theme;

/// Move a picker selection one row up, clamped at the top.
#[must_use]
pub fn nav_up(selected: usize) -> usize {
    selected.saturating_sub(1)
}

/// Move a picker selection one row down, clamped to the last row (an empty list stays put).
///
/// ```
/// assert_eq!(kyde_ui::picker::nav_down(0, 3), 1);
/// assert_eq!(kyde_ui::picker::nav_down(2, 3), 2); // already last
/// assert_eq!(kyde_ui::picker::nav_down(0, 0), 0); // empty list
/// ```
#[must_use]
pub fn nav_down(selected: usize, len: usize) -> usize {
    if selected + 1 < len {
        selected + 1
    } else {
        selected
    }
}

/// The shared picker-row pill: rounded + clickable, `selected_bg` fill when selected, else
/// `hover_bg` on hover. `hover_bg` is explicit because it depends on the panel behind the
/// list (a `bg_mid` panel needs a lighter `bg_light` hover to be visible at all).
pub fn row(id: impl Into<ElementId>, selected: bool, hover_bg: kyde_color::Color) -> Stateful<Div> {
    let t = theme::get();
    div()
        .id(id)
        .rounded_md()
        .cursor_pointer()
        .when(selected, |d| d.bg(t.selected_bg))
        .when(!selected, move |d| d.hover(move |s| s.bg(hover_bg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_clamps_at_both_ends() {
        assert_eq!(nav_up(0), 0);
        assert_eq!(nav_up(5), 4);
        assert_eq!(nav_down(0, 1), 0);
        assert_eq!(nav_down(1, 5), 2);
        assert_eq!(nav_down(4, 5), 4);
        assert_eq!(nav_down(0, 0), 0);
    }
}
