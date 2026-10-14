//! A labelled dropdown `select`, generic over the hosting view. The caller owns the current
//! selection and a [`SelectState`] (open flag, keyboard highlight, the open list's focus);
//! the component flips that state itself through an accessor, so it stays stateless +
//! reusable. The option list paints as a `deferred` absolute overlay anchored just below the
//! control, so it floats **over** the content below instead of pushing it down. While open,
//! the list holds focus: ↑/↓ move the highlight, ⏎ / space pick, ⎋ closes (issue #81).
//! Kyde-agnostic — depends only on gpui + kyde-theme.
use crate::nav::{
    ListConfirm, ListDismiss, ListDown, ListToggle, ListUp, NAV_LIST, NAV_POPUP, NAV_TREE,
};
use crate::picker::{nav_down, nav_up};
use gpui::prelude::*;
use gpui::{deferred, div, px, relative, App, Context, FocusHandle, MouseButton, SharedString};
use kyde_theme as theme;
use std::rc::Rc;

/// A [`select`]'s open state, keyboard highlight, and the focus its open list takes.
pub struct SelectState {
    open: bool,
    hl: Option<usize>,
    focus: FocusHandle,
}

impl SelectState {
    /// A closed select.
    pub fn new(cx: &mut App) -> Self {
        Self {
            open: false,
            hl: None,
            focus: cx.focus_handle(),
        }
    }

    /// Whether the option list is showing.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Close the option list (e.g. when a sibling dropdown opens).
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Show the option list without a click (debug screenshots).
    pub fn show(&mut self) {
        self.open = true;
    }
}

/// Build a select control. `id` namespaces the element ids; `width` is the control + list
/// width (px); `options` are the row labels; `selected` is the current index (drives the
/// closed-state label + where the highlight starts). `state` / `access` are the caller's
/// [`SelectState`] (read now / mutated from handlers). `on_open(view, cx)` runs as the list
/// opens (e.g. to close a sibling dropdown); `on_pick(view, index, cx)` when a row is chosen
/// (the list has already closed).
#[allow(clippy::too_many_arguments)]
pub fn select<V: 'static>(
    cx: &mut Context<V>,
    id: &'static str,
    width: f32,
    options: &[&'static str],
    selected: Option<usize>,
    state: &SelectState,
    access: fn(&mut V) -> &mut SelectState,
    on_open: impl Fn(&mut V, &mut Context<V>) + 'static,
    on_pick: impl Fn(&mut V, usize, &mut Context<V>) + 'static,
) -> gpui::AnyElement {
    let t = theme::get();
    let open = state.open;
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
            cx.listener(move |this, _e, window, cx| {
                let st = access(this);
                st.open = !st.open;
                if st.open {
                    st.hl = selected;
                    // The list isn't in the tree until the next frame — focus now and again
                    // once it is.
                    let focus = st.focus.clone();
                    window.focus(&focus);
                    window.defer(cx, move |window, _cx| window.focus(&focus));
                    on_open(this, cx);
                }
                cx.notify();
            }),
        );

    // `relative` so the list anchors to the control; the list is `absolute` (out of flow, so it
    // doesn't push the rows below) + `deferred` (painted after siblings, so it floats on top).
    let mut root = div().relative().child(control);
    if open {
        let n = options.len();
        let pick = Rc::new(move |this: &mut V, i: usize, cx: &mut Context<V>| {
            access(this).open = false;
            on_pick(this, i, cx);
            cx.notify();
        });
        let rows = options.iter().enumerate().map(|(i, &opt)| {
            let pick = pick.clone();
            let lit = state.hl.or(selected) == Some(i);
            div()
                .id(SharedString::from(format!("{id}-opt-{i}")))
                .px_3()
                .py_1p5()
                .cursor_pointer()
                .text_color(t.text)
                .when(lit, |d| d.bg(t.selected_bg))
                .child(SharedString::from(opt))
                .on_hover(cx.listener(move |this, hovered: &bool, _w, cx| {
                    if *hovered {
                        access(this).hl = Some(i);
                        cx.notify();
                    }
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| pick(this, i, cx)),
                )
        });
        let confirm = {
            let pick = pick.clone();
            Rc::new(move |this: &mut V, cx: &mut Context<V>| {
                if let Some(i) = access(this).hl {
                    pick(this, i, cx);
                }
            })
        };
        let toggle = confirm.clone();
        let list = div()
            .track_focus(&state.focus)
            .key_context(format!("{NAV_LIST} {NAV_TREE} {NAV_POPUP}").as_str())
            .on_action(cx.listener(move |this, _: &ListUp, _w, cx| {
                let st = access(this);
                st.hl = Some(st.hl.map_or(n.saturating_sub(1), nav_up));
                cx.notify();
            }))
            .on_action(cx.listener(move |this, _: &ListDown, _w, cx| {
                let st = access(this);
                st.hl = Some(st.hl.map_or(0, |i| nav_down(i, n)));
                cx.notify();
            }))
            .on_action(cx.listener(move |this, _: &ListConfirm, _w, cx| confirm(this, cx)))
            .on_action(cx.listener(move |this, _: &ListToggle, _w, cx| toggle(this, cx)))
            .on_action(cx.listener(move |this, _: &ListDismiss, _w, cx| {
                access(this).open = false;
                cx.notify();
            }))
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
