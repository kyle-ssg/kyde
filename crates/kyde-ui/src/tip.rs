//! Text tooltips (gpui 0.2.2 ships no ready-made one), styled to match the chrome.

use gpui::prelude::*;
use gpui::{div, px, AnyView, App, Context, SharedString, Window};
use kyde_theme as theme;

/// The tooltip view itself — a small bordered chip of UI text.
pub struct Tip(pub SharedString);

impl gpui::Render for Tip {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::get();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(t.bg_light)
            .border_1()
            .border_color(t.divider)
            .text_color(t.text)
            .font_family(theme::font::UI_FAMILY)
            .text_size(px(t.ui_font_size))
            .shadow_lg()
            .child(self.0.clone())
    }
}

/// A tooltip builder for `.tooltip(..)`: `el.tooltip(tip("Fetch"))`.
pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_w, cx| cx.new(|_| Tip(text.clone())).into()
}
