//! Editor tab strip (tab bar, overflow menu, empty state) + tab close logic. Crate-root child.

use crate::{
    badge_inner, div, file_badge, pretty_key, px, status_color, svg, theme, Context, FluentBuilder,
    InteractiveElement, IntoElement, Kyde, MenuTarget, MouseButton, ParentElement, SharedString,
    StatefulInteractiveElement, Styled,
};

impl Kyde {
    pub(crate) fn render_tab_bar(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let dirty = self.browse.editor.read(cx).dirty;
        let tabs = self.browse.open_tabs.iter().enumerate().map(|(i, p)| {
            let active = self.browse.open_path.as_ref() == Some(p);
            // The preview (temporary) tab renders in italics, like VS Code.
            let preview = self.browse.preview_tab.as_ref() == Some(p);
            let name: SharedString = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into();
            // Share the git text-status color (modified/added/…) like the tree rows.
            let status_col = self
                .files
                .iter()
                .find(|f| &f.path == p)
                .map(|f| status_color(f.status));
            let icon = div().flex_none().child(badge_inner(file_badge(p), 0.0));
            // Active+dirty → a dot in place of the close affordance; otherwise an `×`.
            let grp = SharedString::from(format!("tabgrp-{i}"));
            let close = div()
                .id(SharedString::from(format!("tab-close-{i}")))
                .flex_none()
                .w(px(18.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .text_size(px(15.0))
                .text_color(t.line_number)
                .hover(|d| d.bg(t.bg_light).text_color(t.text))
                // Inactive tabs hide the close until the tab is hovered.
                .when(!active, |d| {
                    d.opacity(0.0).group_hover(grp.clone(), |s| s.opacity(1.0))
                })
                .child(if active && dirty { "●" } else { "×" })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.close_tab(i, cx);
                    }),
                );
            // Rounded pill per tab: active = accent border + faint accent fill; inactive =
            // transparent (no bg/border) until hovered. border_1 stays so widths don't shift.
            div()
                .id(SharedString::from(format!("tab-{i}")))
                .group(grp.clone())
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .h(px(28.0))
                .flex_none()
                .rounded_md()
                .border_1()
                .cursor_pointer()
                // Preview (temporary) tab → the same outline + translucent-fill treatment as an
                // active tab, but in grey instead of the accent blue, so it reads as "tentative".
                // A permanent active tab keeps the blue; inactive permanent tabs are bare.
                .when(preview, |d| {
                    d.bg(gpui::rgba(0x8A909022))
                        .border_color(gpui::rgb(0x6B7079))
                })
                .when(active && !preview, |d| {
                    d.bg(gpui::rgba(0x3574F026)).border_color(t.primary)
                })
                .when(!active && !preview, |d| {
                    d.border_color(gpui::rgba(0x00000000))
                        .hover(|d| d.bg(t.bg_mid))
                })
                .text_color(if active { t.text } else { t.line_number })
                .child(icon)
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .when_some(status_col, gpui::Styled::text_color)
                        .child(name),
                )
                .child(close)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        if let Some(p) = this.browse.open_tabs.get(i).cloned() {
                            this.open_file(p, cx);
                            cx.notify();
                        }
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                        this.open_menu(e.position, MenuTarget::Tab(i), cx);
                    }),
                )
        });

        // The `▾` overflow chooser is NOT rendered here — it's floated on the editor island
        // (see `render_browse`) so it's pinned to the island's right edge and stays visible
        // however wide the tab strip grows. We only reserve room for it on the right (`pr`)
        // so the last tab can scroll out from under it.
        div()
            .id("tab-bar")
            .flex()
            .flex_row()
            .items_center()
            .flex_none()
            .h(px(38.0))
            .bg(t.panel_bg)
            // Match the editor island's top corners (gpui clips rectangular, so the
            // top strip must round itself or it squares off the island corners).
            .rounded_t(px(theme::ISLAND_RADIUS))
            .border_b_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(fs)
            .child(
                div()
                    .id("tabs-scroll")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    // Keep the last tab clear of the button's solid right edge; the rest of
                    // the button is a transparent fade tabs can scroll under.
                    .pr(px(34.0))
                    .flex_1()
                    .min_w_0()
                    .overflow_x_scroll()
                    .track_scroll(&self.browse.tab_scroll)
                    // A plain mouse has only a vertical wheel; map it to horizontal so the
                    // tab strip scrolls. Native horizontal (trackpad) stays with overflow_x.
                    .on_scroll_wheel(cx.listener(|this, e: &gpui::ScrollWheelEvent, _w, cx| {
                        let d = e.delta.pixel_delta(px(18.0));
                        if d.y.abs() > px(0.0) {
                            let mut off = this.browse.tab_scroll.offset();
                            off.x += d.y;
                            this.browse.tab_scroll.set_offset(off);
                            cx.notify();
                        }
                    }))
                    .children(tabs),
            )
            .into_any_element()
    }

    /// The `▾` tab-overflow chooser, floated absolutely at the top-right of the editor
    /// island so it's pinned to the island's right edge and stays visible no matter how
    /// wide the tab strip grows (the strip can overflow + scroll under it). Click → a
    /// dropdown listing every open tab. Rendered only when tabs are open.
    pub(crate) fn render_tab_overflow_button(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        // Tabs scroll *under* this; a left→right fade (transparent → panel) dissolves them
        // into the button instead of a hard bordered box, so it reads as part of the strip.
        let fade_from = gpui::Rgba {
            a: 0.0,
            ..t.panel_bg.into()
        };
        // A child of `right_col` (relative), painted above the tab strip's scroll layer — a
        // child of the island itself was drawn *under* the scrolling tabs and vanished once
        // they filled the right edge. Anchored to the body region, so `top` is just the body's
        // top pad (FRAME_GAP) and any banner above main_row shifts it correctly; x = body right
        // pad (FRAME_GAP).
        div()
            .absolute()
            .top(px(theme::FRAME_GAP))
            .right(px(theme::FRAME_GAP))
            // Above the tab bar's 1px bottom border; match its rounded top-right corner.
            .h(px(37.0))
            .rounded_tr(px(theme::ISLAND_RADIUS))
            .flex()
            .items_center()
            .justify_end()
            .w(px(56.0))
            .pr(px(6.0))
            .occlude()
            .bg(gpui::linear_gradient(
                90.0,
                gpui::linear_color_stop(fade_from, 0.0),
                gpui::linear_color_stop(t.panel_bg, 0.55),
            ))
            .child(
                div()
                    .id("tabs-overflow")
                    .size(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .flex_none()
                    .cursor_pointer()
                    .hover(|d| d.bg(t.bg_mid))
                    .child(
                        svg()
                            .path("icons/chevron-down.svg")
                            .size(px(15.0))
                            .text_color(t.line_number),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &gpui::MouseDownEvent, _w, cx| {
                            // Drop the dropdown down-left of the cursor (panel ≥180px wide,
                            // button hugs the right edge) so it never lands off-screen-right.
                            let at = gpui::point(
                                (e.position.x - px(180.0)).max(px(8.0)),
                                e.position.y + px(8.0),
                            );
                            this.open_menu(at, MenuTarget::TabList, cx);
                        }),
                    ),
            )
            .into_any_element()
    }

    /// Shown in the editor pane when no file is open: the handful of shortcuts that
    /// actually get you somewhere (keys reflect the active keymap).
    pub(crate) fn render_no_file(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let key = |name: &str| {
            self.keymap
                .key_for(name)
                .map(|k| pretty_key(&k))
                .unwrap_or_default()
        };
        let row = |label: &'static str, accel: String| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(div().text_color(t.text).child(label))
                .child(
                    div()
                        .text_color(t.line_number)
                        .child(SharedString::from(accel)),
                )
        };

        let _ = cx;
        div()
            .flex()
            .flex_col()
            .flex_1()
            .gap_4()
            .px_12()
            .justify_center()
            // No bg: the rounded editor island behind provides the surface, so the
            // panel's corners stay rounded (gpui clips rectangular, not rounded).
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size + 2.0))
            .child(row("Go to File", key("go_to_file")))
            .child(row("Commit", key("commit")))
            .child(row("Commit view", key("mode_commit")))
            .child(row("Keymap / Settings", key("open_keymap")))
            .child(
                div()
                    .text_color(t.line_number)
                    .child("Select a file from the tree to open it"),
            )
            .child(
                div()
                    .text_color(t.line_number)
                    .child("Right-click a file to Commit or Rollback"),
            )
            .into_any_element()
    }

    /// Close the tab at `idx`. If it was active, fall to its right neighbour (else left,
    /// else nothing open).
    pub(crate) fn close_tab(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.browse.open_tabs.len() {
            return;
        }
        let closing = self.browse.open_tabs.remove(idx);
        if self.browse.preview_tab.as_ref() == Some(&closing) {
            self.browse.preview_tab = None;
        }
        if self.browse.open_path.as_ref() == Some(&closing) {
            let next = self
                .browse
                .open_tabs
                .get(idx)
                .or_else(|| self.browse.open_tabs.get(idx.saturating_sub(1)))
                .cloned();
            match next {
                Some(p) => self.open_file(p, cx),
                None => self.clear_open(cx),
            }
        }
        self.close_menu(cx);
    }

    /// Close every tab except the one at `idx`, and make it active.
    pub(crate) fn close_other_tabs(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(keep) = self.browse.open_tabs.get(idx).cloned() else {
            return;
        };
        self.browse.open_tabs = vec![keep.clone()];
        // Drop a stale preview pointer if its tab was among those closed.
        self.browse.preview_tab = self.browse.preview_tab.take().filter(|p| p == &keep);
        self.open_file(keep, cx);
        self.close_menu(cx);
    }

    /// Close all tabs to the right of `idx`. If the active tab was among them, activate `idx`.
    pub(crate) fn close_tabs_right(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx + 1 >= self.browse.open_tabs.len() {
            self.close_menu(cx);
            return;
        }
        let active_removed = self
            .browse
            .open_path
            .as_ref()
            .and_then(|p| self.browse.open_tabs.iter().position(|t| t == p))
            .is_some_and(|pos| pos > idx);
        self.browse.open_tabs.truncate(idx + 1);
        // Drop a stale preview pointer if its tab was truncated away.
        if self
            .browse
            .preview_tab
            .as_ref()
            .is_some_and(|p| !self.browse.open_tabs.contains(p))
        {
            self.browse.preview_tab = None;
        }
        if active_removed {
            if let Some(p) = self.browse.open_tabs.get(idx).cloned() {
                self.open_file(p, cx);
            }
        }
        self.close_menu(cx);
    }
}
