//! Side-by-side diff building blocks shared by the Show-Diff, Compare, Merge and Local
//! History views: the center gutter column, its per-hunk control rows + glyph buttons, and
//! the content-width editor pane that scrolls through a shared handle. Callers chain their
//! own tooltips / handlers / extra children.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, Div, ElementId, Pixels, ScrollHandle, Stateful};
use kyde_theme as theme;

/// Width of the center apply gutter in the Compare / Merge / Local History windows.
pub const GUTTER_W: f32 = 56.0;

/// A gutter control glyph (`»`, `«`, `×`, `✓`…): `color` at rest, the accent on hover.
pub fn gutter_glyph(
    id: impl Into<ElementId>,
    glyph: &'static str,
    color: kyde_color::Color,
) -> Stateful<Div> {
    let t = theme::get();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(22.0))
        .rounded_sm()
        .text_size(px(17.0))
        .line_height(px(17.0))
        .text_color(color)
        .hover(|s| s.bg(t.bg_light).text_color(t.primary))
        .cursor_pointer()
        .child(glyph)
}

/// One aligned gutter row at display row `row` (absolutely placed, `row_h` tall) — its
/// glyphs centered side by side.
pub fn gutter_row(row: usize, row_h: Pixels) -> Div {
    div()
        .absolute()
        .top(row_h * row as f32)
        .left_0()
        .right_0()
        .h(row_h)
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap_0p5()
}

/// The center gutter column: separator tint with hairlines on both edges. Its `rows`
/// (from [`gutter_row`]) sit in a `total_h` sheet shifted by `scroll_y`, the panes' shared
/// vertical offset, so they track the aligned lines.
pub fn gutter_column(
    id: impl Into<ElementId>,
    width: f32,
    total_h: Pixels,
    scroll_y: Pixels,
    rows: impl IntoIterator<Item = AnyElement>,
) -> Stateful<Div> {
    let t = theme::get();
    div()
        .id(id)
        .w(px(width))
        .flex_none()
        .h_full()
        .overflow_hidden()
        .bg(t.diff_separator_bg)
        .border_l(px(1.0))
        .border_r(px(1.0))
        .border_color(t.divider)
        .child(
            div()
                .relative()
                .w_full()
                .h(total_h)
                .top(scroll_y)
                .children(rows),
        )
}

/// The scrolling part of a pane: a viewport scrolled by the (shared) `scroll` handle,
/// holding the editor at its `content_w` so long lines overflow sideways.
pub fn pane_scroller(
    id: impl Into<ElementId>,
    content_w: f32,
    editor: AnyElement,
    scroll: &ScrollHandle,
) -> Stateful<Div> {
    div()
        .id(id)
        .h_full()
        .w_full()
        .overflow_scroll()
        .track_scroll(scroll)
        .child(div().w(px(content_w)).child(editor))
}

/// A whole side-by-side pane: [`pane_scroller`] filling a `flex_1` column on the editor
/// background, in the code font at `font_size`.
pub fn editor_pane(
    id: impl Into<ElementId>,
    content_w: f32,
    editor: AnyElement,
    scroll: &ScrollHandle,
    font_size: Pixels,
) -> Div {
    let t = theme::get();
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .bg(t.main_bg)
        .font_family(theme::font::family())
        .text_size(font_size)
        .text_color(t.text)
        .child(pane_scroller(id, content_w, editor, scroll))
}
