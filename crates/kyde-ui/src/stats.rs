//! `+a −r` line-count label (IDE diff-stats style), shared by the changed-files tree
//! rows and the diff nav.
use gpui::prelude::*;
use gpui::{div, px};
use kyde_theme as theme;

/// Small `+a −r` spans in the theme's added/deleted colors. Zero sides are dropped and
/// `(0, 0)` renders nothing at all, so untouched rows stay exactly as they were.
pub fn line_stats(added: usize, removed: usize) -> Option<gpui::AnyElement> {
    if added == 0 && removed == 0 {
        return None;
    }
    let t = theme::get();
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .gap_1()
        .text_size(px(t.ui_font_size));
    if added > 0 {
        row = row.child(div().text_color(t.status_added).child(format!("+{added}")));
    }
    if removed > 0 {
        row = row.child(
            div()
                .text_color(t.status_deleted)
                .child(format!("−{removed}")),
        );
    }
    Some(row.into_any_element())
}
