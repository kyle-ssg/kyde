//! Settings window — a category sidebar (Appearance / Keymap / Language Packs) + a content
//! pane, IntelliJ-style. Hosted in a native `ModalWindow` (ModalKind::Settings). Changes
//! apply live via `theme::update` (font sizes, row height) / `choose_preset` (keymap).

use crate::*;
use gpui::FontWeight;

impl Kyde {
    /// Open (or focus) the Settings window. Bound to ⌘, and the native Settings… menu.
    pub(crate) fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.open_modal_window(ModalKind::Settings, "Settings", 660.0, 480.0, cx);
    }

    /// Body of the Settings window: sidebar | divider | scrollable content.
    pub(crate) fn render_settings_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let section = self.settings_section;

        let sidebar = div()
            .w(px(180.0))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .p_2()
            .bg(t.frame_bg)
            .children(SettingsSection::ALL.iter().map(|&(sec, label)| {
                let selected = sec == section;
                div()
                    .id(label)
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.selected_bg).text_color(t.text))
                    .when(!selected, |d| {
                        d.text_color(t.secondary_text).hover(|d| d.bg(t.bg_mid))
                    })
                    .child(SharedString::from(label))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.settings_section = sec;
                            cx.notify();
                        }),
                    )
            }));

        let content = match section {
            SettingsSection::Appearance => self.settings_appearance(cx),
            SettingsSection::Keymap => self.settings_keymap(cx),
            SettingsSection::LanguagePacks => self.render_plugins_body(cx),
        };

        div()
            .size_full()
            .flex()
            .flex_row()
            .child(sidebar)
            .child(div().w(px(1.0)).h_full().flex_none().bg(t.divider))
            .child(
                div()
                    .id("settings-content")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .child(content),
            )
            .into_any_element()
    }

    /// Appearance section: theme + the three px size controls (live-applied).
    fn settings_appearance(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .child(settings_heading("Appearance"))
            // Theme picker — one palette for now ("Dark"); presets land here later.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w(px(140.0))
                            .text_color(t.secondary_text)
                            .child("Theme"),
                    )
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(t.divider)
                            .text_color(t.text)
                            .child("Dark"),
                    ),
            )
            .child(self.size_row(
                cx,
                "ui",
                "UI font size",
                t.ui_font_size,
                8.0,
                40.0,
                1.0,
                |th, v| th.ui_font_size = v,
            ))
            .child(self.size_row(
                cx,
                "editor",
                "Editor font size",
                t.editor_font_size,
                8.0,
                40.0,
                1.0,
                |th, v| th.editor_font_size = v,
            ))
            .child(self.size_row(
                cx,
                "row",
                "Tree row height",
                t.tree_row_height,
                16.0,
                40.0,
                2.0,
                |th, v| th.tree_row_height = v,
            ))
            .into_any_element()
    }

    /// A labelled `[−] value [+]` px stepper. `apply` writes the clamped value into the live
    /// theme (saved + repainted immediately). `value` is the current snapshot; after a click
    /// the body re-renders and reads the new value.
    #[allow(clippy::too_many_arguments)]
    fn size_row(
        &self,
        cx: &mut Context<Self>,
        id: &'static str,
        label: &'static str,
        value: f32,
        lo: f32,
        hi: f32,
        step: f32,
        apply: fn(&mut theme::Theme, f32),
    ) -> gpui::AnyElement {
        let t = theme::get();
        let step_btn =
            |bid: SharedString, sym: &'static str, delta: f32, cx: &mut Context<Self>| {
                div()
                    .id(bid)
                    .size(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .border_1()
                    .border_color(t.divider)
                    .text_color(t.text)
                    .cursor_pointer()
                    .hover(|d| d.bg(t.bg_mid))
                    .child(sym)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_this, _e, _w, cx| {
                            theme::update(|th| apply(th, (value + delta).clamp(lo, hi)));
                            cx.notify();
                        }),
                    )
            };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(div().w(px(140.0)).text_color(t.secondary_text).child(label))
            .child(step_btn(format!("{id}-dec").into(), "−", -step, cx))
            .child(
                div()
                    .w(px(40.0))
                    .flex()
                    .justify_center()
                    .text_color(t.text)
                    .child(SharedString::from(format!("{value:.0}"))),
            )
            .child(step_btn(format!("{id}-inc").into(), "+", step, cx))
            .into_any_element()
    }

    /// Keymap section: pick the preset (applies immediately via `choose_preset`).
    fn settings_keymap(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let current = self.keymap.preset;
        let opt = |preset: Preset, label: &'static str, cx: &mut Context<Self>| {
            let selected = current == preset;
            div()
                .id(label)
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .when(selected, |d| d.bg(t.selected_bg).text_color(t.text))
                .when(!selected, |d| {
                    d.text_color(t.secondary_text).hover(|d| d.bg(t.bg_mid))
                })
                .child(if selected { "●" } else { "○" })
                .child(SharedString::from(label))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.choose_preset(preset, cx)),
                )
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .child(settings_heading("Keymap"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(opt(Preset::WebStorm, "WebStorm", cx))
                    .child(opt(Preset::VSCode, "VS Code", cx)),
            )
            .into_any_element()
    }
}

/// A section heading inside the Settings content pane.
fn settings_heading(label: &'static str) -> gpui::Div {
    div()
        .text_size(px(theme::get().ui_font_size + 4.0))
        .font_weight(FontWeight::BOLD)
        .text_color(theme::get().text)
        .child(label)
}
