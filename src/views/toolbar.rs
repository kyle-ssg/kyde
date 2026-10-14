//! Git quick-access toolbar (issue #92): Fetch / Pull / Commit / Push / History as one-click
//! buttons at the right of the title bar, IDE-toolbar style, with the behind / ahead counts
//! on Pull / Push. Each button is the same action as the menu items and status-bar chips.
//! Crate-root child module.

use crate::*;

impl Kyde {
    /// The toolbar, for a project that's a git repo (hidden on the landing view and for a
    /// plain folder — issue #66).
    pub(crate) fn render_git_toolbar(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.repo_root.is_none() || !self.is_git {
            return None;
        }
        let shortcut = |action: &str| {
            self.keymap
                .key_for(action)
                .map(|k| format!("  ({})", pretty_key(&k)))
                .unwrap_or_default()
        };
        let sync = &self.sync;
        let behind = sync.behind.unwrap_or(0);
        let ahead = sync.ahead.unwrap_or(0);
        let busy_tip = |busy: bool, doing: &str, idle: String| {
            if busy {
                SharedString::from(format!("{doing}…"))
            } else {
                SharedString::from(idle)
            }
        };
        let row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_0p5()
            .flex_none()
            .pr_2()
            .font_family(theme::font::UI_FAMILY)
            .child(
                tool_btn(
                    "tb-fetch",
                    "icons/arrow-down-to-line.svg",
                    busy_tip(sync.fetching, "Fetching", "Fetch".into()),
                    sync.fetching,
                    0,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.do_fetch(cx)),
                ),
            )
            .child(
                tool_btn(
                    "tb-pull",
                    "icons/arrow-down.svg",
                    busy_tip(sync.pulling, "Pulling", "Pull (rebase)".into()),
                    sync.pulling,
                    behind,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.do_pull(cx)),
                ),
            )
            .child(
                tool_btn(
                    "tb-commit",
                    "icons/git-commit.svg",
                    format!("Commit…{}", shortcut("mode_commit")).into(),
                    false,
                    self.files.len(),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.switch_mode(Mode::Commit, cx)),
                ),
            )
            .child(
                tool_btn(
                    "tb-push",
                    "icons/arrow-up.svg",
                    busy_tip(sync.pushing, "Pushing", "Push…".into()),
                    sync.pushing,
                    ahead,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.open_push_modal(cx)),
                ),
            )
            .child(
                tool_btn(
                    "tb-history",
                    "icons/history.svg",
                    "History".into(),
                    false,
                    0,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.switch_mode(Mode::History, cx)),
                ),
            );
        Some(row.into_any_element())
    }
}

/// One toolbar button: an icon (dimmed while its operation runs) with a tooltip, plus a
/// count badge when `count > 0`.
fn tool_btn(
    id: &'static str,
    icon: &'static str,
    tip: SharedString,
    busy: bool,
    count: usize,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .h(px(26.0))
        .px_1p5()
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(t.bg_mid))
        .tooltip(move |_w, cx| cx.new(|_| Tip(tip.clone())).into())
        .child(svg().path(icon).size(px(16.0)).text_color(if busy {
            t.line_number
        } else {
            t.secondary_text
        }))
        .when(count > 0, |d| {
            d.child(
                div()
                    .px_1()
                    .rounded_sm()
                    .bg(t.bg_light)
                    .text_size(px(10.0))
                    .text_color(t.text)
                    .child(SharedString::from(count.to_string())),
            )
        })
}
