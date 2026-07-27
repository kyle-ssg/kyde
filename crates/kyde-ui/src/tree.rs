//! File-tree row — the reusable list item shared by Browse, Commit, History and Rollback
//! trees. Kyde-agnostic: generic over the hosting view `V`, so it knows nothing about app
//! state. The caller passes the row data + three handlers (activate / checkbox / context);
//! `suppress_hover` lets the caller mute the hover tint (e.g. mid divider-drag).
use crate::{badge_inner, file_badge};
use gpui::prelude::*;
use gpui::{div, px, svg, Context, MouseButton, Pixels, SharedString, Window};
use kyde_theme as theme;

/// Drag payload for a tree row: the path being moved. Drop targets type-match on this, so
/// an in-app row drag never collides with an OS file drop (`ExternalPaths`).
#[derive(Clone)]
pub struct DragPath(pub std::path::PathBuf);

/// The little chip that follows the cursor while a tree row is dragged.
pub struct DragPreview {
    name: SharedString,
}

impl gpui::Render for DragPreview {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::get();
        div()
            .px_2()
            .py_0p5()
            .rounded_md()
            .bg(t.bg_light)
            .border_1()
            .border_color(t.divider)
            .text_color(t.text)
            .text_size(px(theme::get().ui_font_size + 1.0))
            .font_family(theme::font::UI_FAMILY)
            .child(self.name.clone())
    }
}

/// Boxed drop handler: `(view, dragged_source_path, cx)`. Passed to [`item`] on rows that
/// accept a drop (directories); `None` means the row is not a drop target.
pub type OnDrop<V> = Box<dyn Fn(&mut V, std::path::PathBuf, &mut Context<V>)>;

/// Boxed handler for files dragged in from the OS file manager (Finder): `(view, paths,
/// cx)`. `Some` on a row makes it a target for external-file drops with a highlight.
pub type OnExtDrop<V> = Box<dyn Fn(&mut V, Vec<std::path::PathBuf>, &mut Context<V>)>;

/// Render one file-tree row (chevron + icon + label) generic over the hosting view `V`.
/// The caller supplies the row's data + click/right-click handlers; this owns the layout,
/// indentation, and hover/selected styling so every tree in the app looks identical.
#[allow(clippy::too_many_arguments)]
pub fn item<V: 'static>(
    cx: &mut Context<V>,
    suppress_hover: bool,
    path: &std::path::Path,
    is_dir: bool,
    expanded: bool,
    depth: usize,
    selected: bool,
    name: SharedString,
    name_color: kyde_color::Color,
    checkbox: Option<bool>,
    trailing: Option<gpui::AnyElement>,
    // `draggable` makes the row a drag source (payload = its own `path`); `on_drop` = Some
    // makes it a target for in-app row drags (issue #65); `on_ext_drop` = Some makes it a
    // target for OS file-manager drops (issue #67). All default off.
    draggable: bool,
    on_drop: Option<OnDrop<V>>,
    on_ext_drop: Option<OnExtDrop<V>>,
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
        .when_some(checkbox_el, gpui::ParentElement::child)
        .child(badge)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(name_color)
                .child(name),
        )
        .when_some(trailing, gpui::ParentElement::child);

    // A stable per-path id makes the row a stateful element (needed for drag), and a Copy
    // color to tint the row while a compatible drag hovers it.
    let row_id: gpui::ElementId =
        SharedString::from(format!("tree-row::{}", path.to_string_lossy())).into();
    let drop_hl = t.selected_bg;
    let drag_name: SharedString = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .into();
    let drag_src = path.to_path_buf();

    let mut row = div()
        .id(row_id)
        .flex()
        .flex_row()
        .items_center()
        .h(px(theme::get().tree_row_height))
        .text_size(px(theme::get().ui_font_size + 3.0))
        .flex_none()
        .mx(px(6.0))
        .pr_1()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |d| d.bg(t.selected_bg))
        .when(!selected && !suppress_hover, |d| {
            d.hover(|d| d.bg(t.bg_mid))
        })
        .child(content)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &gpui::MouseDownEvent, w, cx| on_activate(this, e, w, cx)),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                on_context(this, e.position, cx);
            }),
        );

    if draggable {
        row = row.on_drag(DragPath(drag_src), move |_p, _off, _w, cx| {
            cx.new(|_| DragPreview {
                name: drag_name.clone(),
            })
        });
    }
    if let Some(handler) = on_drop {
        row = row
            .drag_over::<DragPath>(move |s, _drag, _w, _cx| s.bg(drop_hl))
            .on_drop(cx.listener(move |this, dp: &DragPath, _w, cx| {
                handler(this, dp.0.clone(), cx);
            }));
    }
    if let Some(handler) = on_ext_drop {
        row = row
            .drag_over::<gpui::ExternalPaths>(move |s, _drag, _w, _cx| s.bg(drop_hl))
            .on_drop(
                cx.listener(move |this, paths: &gpui::ExternalPaths, _w, cx| {
                    handler(this, paths.paths().to_vec(), cx);
                }),
            );
    }
    row.into_any_element()
}
