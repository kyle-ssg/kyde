//! A labelled dropdown `select`, generic over the hosting view. **Controlled**: the caller
//! owns the `open` flag + the current selection and supplies toggle/pick callbacks (so the
//! component is stateless + reusable). The option list paints as a `deferred` absolute overlay
//! anchored just below the control, so it floats **over** the content below instead of pushing
//! it down. Kyde-agnostic — depends only on gpui + kyde-theme.
use gpui::prelude::*;
use gpui::{deferred, div, px, relative, Context, MouseButton, SharedString};
use kyde_theme as theme;
use std::rc::Rc;

/// Build a select control. `id` namespaces the element ids; `width` is the control + list
/// width (px); `options` are the row labels; `selected` is the current index (drives the
/// closed-state label + the highlighted row); `open` shows the list. `on_toggle` flips the
/// caller's open flag; `on_pick(view, index, cx)` is called when a row is chosen.
#[allow(clippy::too_many_arguments)]
pub fn select<V: 'static>(
    cx: &mut Context<V>,
    id: &'static str,
    width: f32,
    options: &[&'static str],
    selected: Option<usize>,
    open: bool,
    on_toggle: impl Fn(&mut V, &mut Context<V>) + 'static,
    on_pick: impl Fn(&mut V, usize, &mut Context<V>) + 'static,
) -> gpui::AnyElement {
    let t = theme::get();
    let label = selected
        .and_then(|i| options.get(i))
        .copied()
        .unwrap_or("Custom");

    let control = div()
        .id(id)
        .w(px(width))
        .px_3()
        .py_1p5()
        .rounded_md()
        .border_1()
        .border_color(t.divider)
        .bg(t.main_bg)
        .cursor_pointer()
        .text_color(t.text)
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .hover(|d| d.bg(t.bg_mid))
        .child(SharedString::from(label))
        .child(
            div()
                .text_color(t.secondary_text)
                .child(if open { "▴" } else { "▾" }),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _e, _w, cx| on_toggle(this, cx)),
        );

    // `relative` so the list anchors to the control; the list is `absolute` (out of flow, so it
    // doesn't push the rows below) + `deferred` (painted after siblings, so it floats on top).
    let mut root = div().relative().child(control);
    if open {
        let on_pick = Rc::new(on_pick);
        let rows = options.iter().enumerate().map(move |(i, &opt)| {
            let on_pick = on_pick.clone();
            let is_sel = selected == Some(i);
            div()
                .id(SharedString::from(format!("{id}-opt-{i}")))
                .px_3()
                .py_1p5()
                .cursor_pointer()
                .text_color(t.text)
                .when(is_sel, |d| d.bg(t.selected_bg))
                .when(!is_sel, |d| d.hover(|d| d.bg(t.bg_mid)))
                .child(SharedString::from(opt))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| on_pick(this, i, cx)),
                )
        });
        let list = div()
            .absolute()
            .top(relative(1.0))
            .left_0()
            .mt(px(4.0))
            .w(px(width))
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(t.divider)
            .bg(t.panel_bg)
            .overflow_hidden()
            .occlude()
            .children(rows);
        root = root.child(deferred(list).with_priority(1));
    }
    root.into_any_element()
}
