//! Dismiss-backdrop overlay primitive.
use crate::*;

/// A full-window dimmed overlay that centers its child. When `dismissable`, clicking the
/// backdrop closes the open overlays; otherwise the backdrop swallows the click (modal).
pub(crate) fn overlay(cx: &mut Context<Kyde>, dismissable: bool) -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        // A dim scrim, not a blackout — the app stays visible behind the modal.
        .bg(gpui::rgba(0x00000099))
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _e, window, cx| {
                if dismissable {
                    this.finder_open = false;
                    this.onboarding_open = false;
                    this.delete_target = None;
                    window.focus(&this.focus_handle);
                    cx.notify();
                }
            }),
        )
}
