//! The find bar every ⌘F shares — terminal scrollback, the editor, diff panes: a query box, the
//! match count, ↑ / ↓ / × and (for find / replace) a replace row, in a small panel floated at
//! the top right of what it searches.
use gpui::prelude::*;
use gpui::{
    div, px, svg, AnyElement, App, Div, ElementId, MouseButton, MouseDownEvent, SharedString,
    Window,
};
use kyde_theme as theme;

/// A click on one of the bar's buttons.
type OnClick = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

/// Width of the query (and replace) box.
const INPUT_W: f32 = 200.0;

/// The match count: blank with no query, then "No results", the total, or `current/total`
/// (`current` is 1-based; 0 = not on a match yet).
///
/// ```
/// use kyde_ui::find::match_label;
/// assert_eq!(match_label(false, 0, 0), "");
/// assert_eq!(match_label(true, 0, 0), "No results");
/// assert_eq!(match_label(true, 5, 0), "5");
/// assert_eq!(match_label(true, 5, 2), "2/5");
/// ```
#[must_use]
pub fn match_label(has_query: bool, total: usize, current: usize) -> String {
    match (has_query, total, current) {
        (false, ..) => String::new(),
        (true, 0, _) => "No results".to_string(),
        (true, n, 0) => n.to_string(),
        (true, n, i) => format!("{i}/{n}"),
    }
}

/// Float `bar` at the top right of its nearest `relative` ancestor, clear of a scrollbar
/// gutter along the right edge.
pub fn floating(bar: Bar) -> Div {
    div().absolute().top(px(6.0)).right(px(18.0)).child(bar)
}

/// A find bar. Build with [`Bar::new`], wire its buttons, and place it with [`floating`].
#[derive(IntoElement)]
pub struct Bar {
    id: ElementId,
    query: AnyElement,
    label: SharedString,
    up: Option<(SharedString, OnClick)>,
    down: Option<(SharedString, OnClick)>,
    close: Option<OnClick>,
    replace: Option<(AnyElement, OnClick, OnClick)>,
}

impl Bar {
    /// A bar around the `query` input (a single-line editor), showing `label` (see
    /// [`match_label`]) as its match count.
    pub fn new(
        id: impl Into<ElementId>,
        query: impl IntoElement,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            query: query.into_any_element(),
            label: label.into(),
            up: None,
            down: None,
            close: None,
            replace: None,
        }
    }

    /// The ↑ button: `tip` on hover, `f` on click.
    #[must_use]
    pub fn on_up(
        mut self,
        tip: impl Into<SharedString>,
        f: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.up = Some((tip.into(), Box::new(f)));
        self
    }

    /// The ↓ button: `tip` on hover, `f` on click.
    #[must_use]
    pub fn on_down(
        mut self,
        tip: impl Into<SharedString>,
        f: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.down = Some((tip.into(), Box::new(f)));
        self
    }

    /// The × button.
    #[must_use]
    pub fn on_close(
        mut self,
        f: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.close = Some(Box::new(f));
        self
    }

    /// A replace row under the query: the `input` box, then Replace (`one`) and All (`all`).
    #[must_use]
    pub fn replace(
        mut self,
        input: impl IntoElement,
        one: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
        all: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.replace = Some((input.into_any_element(), Box::new(one), Box::new(all)));
        self
    }
}

/// The rounded box an input sits in.
fn input_box(input: AnyElement) -> Div {
    let t = theme::get();
    div()
        .flex_none()
        .w(px(INPUT_W))
        .h(px(26.0))
        .px_2()
        .flex()
        .items_center()
        .bg(t.main_bg)
        .border_1()
        .border_color(t.divider)
        .rounded_md()
        .child(input)
}

/// A muted icon button showing the `icon` svg.
fn icon_button(
    id: &'static str,
    icon: &'static str,
    tip: Option<SharedString>,
    f: Option<OnClick>,
) -> gpui::Stateful<Div> {
    let t = theme::get();
    let b =
        crate::icon_btn(id, 22.0).child(svg().path(icon).size(px(14.0)).text_color(t.line_number));
    let b = match tip {
        Some(tip) => b.tooltip(crate::tip(tip)),
        None => b,
    };
    match f {
        Some(f) => b.on_mouse_down(MouseButton::Left, f),
        None => b,
    }
}

/// A muted text button, the icon buttons' look widened to fit `label`.
fn text_button(id: &'static str, label: &'static str, f: OnClick) -> gpui::Stateful<Div> {
    crate::icon_btn(id, 22.0)
        .w_auto()
        .px_2()
        .child(label)
        .on_mouse_down(MouseButton::Left, f)
}

impl RenderOnce for Bar {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let t = theme::get();
        let (up_tip, up) = self.up.map(|(t, f)| (Some(t), Some(f))).unwrap_or_default();
        let (down_tip, down) = self
            .down
            .map(|(t, f)| (Some(t), Some(f)))
            .unwrap_or_default();
        let find_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(input_box(self.query))
            .child(
                div()
                    .flex_none()
                    .min_w(px(64.0))
                    .px_1()
                    .text_color(t.line_number)
                    .text_size(px(t.ui_font_size - 1.0))
                    .child(self.label),
            )
            .child(icon_button("up", "icons/arrow-up.svg", up_tip, up))
            .child(icon_button("down", "icons/arrow-down.svg", down_tip, down))
            .child(icon_button(
                "close",
                "icons/x.svg",
                Some("Close  (Esc)".into()),
                self.close,
            ));
        let replace_row = self.replace.map(|(input, one, all)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(input_box(input))
                .child(text_button("replace-one", "Replace", one))
                .child(text_button("replace-all", "All", all))
        });
        div()
            .id(self.id)
            .occlude()
            // Clicks on the bar stay on it (not a caret or selection in what's underneath).
            .on_mouse_down(MouseButton::Left, |_e, _w, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_e, _w, cx| cx.stop_propagation())
            .flex()
            .flex_col()
            .gap_1()
            .pl_2()
            .pr_1()
            .py_1p5()
            .bg(t.panel_bg)
            .border_1()
            .border_color(t.divider)
            .rounded_md()
            .shadow_md()
            .font_family(theme::font::UI_FAMILY)
            .text_size(px(t.ui_font_size))
            .child(find_row)
            .children(replace_row)
    }
}
