//! File-tree row — the reusable list item shared by Browse, Commit, History and Rollback
//! trees. Kyde-agnostic: generic over the hosting view `V`, so it knows nothing about app
//! state. The caller passes the row data + three handlers (activate / checkbox / context);
//! `suppress_hover` lets the caller mute the hover tint (e.g. mid divider-drag).
use crate::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn item<V: 'static>(
    cx: &mut Context<V>,
    suppress_hover: bool,
    path: &std::path::Path,
    is_dir: bool,
    expanded: bool,
    depth: usize,
    selected: bool,
    name: SharedString,
    name_color: gpui::Rgba,
    checkbox: Option<bool>,
    on_activate: impl Fn(&mut V, &gpui::MouseDownEvent, &mut Window, &mut Context<V>) + 'static,
    on_check: impl Fn(&mut V, &mut Context<V>) + 'static,
    on_context: impl Fn(&mut V, gpui::Point<Pixels>, &mut Context<V>) + 'static,
) -> gpui::AnyElement {
    let t = theme::get();
    let indent = px(8.0 + depth as f32 * 14.0);

    // Caret column (chevron for dirs, empty spacer for files) — fixed width so the
    // checkbox/badge/name columns align across rows.
    let caret = div()
        .w(px(16.0))
        .flex_none()
        .text_color(t.line_number)
        .text_size(px(theme::get().ui_font_size + 5.0))
        .when(is_dir, |d| d.child(if expanded { "▾" } else { "▸" }));

    // A real drawn checkbox (rounded square; check svg when ticked), after the caret. Its own
    // click toggles without firing the row.
    let checkbox_el = checkbox.map(|checked| {
        let mut b = div()
            .flex_none()
            .size(px(15.0))
            .mr(px(6.0))
            .rounded_sm()
            .border_1()
            .flex()
            .items_center()
            .justify_center();
        b = if checked {
            b.bg(t.primary).border_color(t.primary).child(
                svg()
                    .path("icons/check.svg")
                    .size(px(11.0))
                    .text_color(t.primary_text),
            )
        } else {
            b.border_color(t.line_number)
        };
        b.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _e, _w, cx| {
                cx.stop_propagation();
                on_check(this, cx);
            }),
        )
    });

    let badge = div()
        .w(px(22.0))
        .flex_none()
        .mr(px(4.0))
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_end()
        .child(if is_dir {
            svg()
                .path("icons/folder.svg")
                .size(px(16.0))
                .text_color(gpui::rgb(0x9AA0A6))
                .into_any_element()
        } else {
            badge_inner(file_badge(path), 2.0)
        });

    let content = div()
        .flex()
        .flex_row()
        .items_center()
        .flex_1()
        .min_w_0()
        .pl(indent)
        .child(caret)
        .when_some(checkbox_el, |d, cb| d.child(cb))
        .child(badge)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(name_color)
                .child(name),
        );

    div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(38.0))
        .text_size(px(theme::get().ui_font_size + 3.0))
        .flex_none()
        .mx(px(6.0))
        .pr_1()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |d| d.bg(t.selected_bg))
        .when(!selected && !suppress_hover, |d| d.hover(|d| d.bg(t.bg_mid)))
        .child(content)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &gpui::MouseDownEvent, w, cx| on_activate(this, e, w, cx)),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                on_context(this, e.position, cx)
            }),
        )
        .into_any_element()
}
