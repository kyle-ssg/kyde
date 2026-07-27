//! Changelog window (issue #71) — the project's GitHub Releases mirrored in-app: a version
//! list on the left, that release's notes rendered as markdown on the right. Data comes from
//! `kyde_update::release_notes` (fetched off the UI thread, `KYDE_RELEASES_FEED_URL`-overridable);
//! a fetch failure shows a Retry + an "Open on GitHub" fallback. Crate-root child module.

use crate::*;

impl Kyde {
    /// Kyde menu "What's New…" / palette "What's New (Changelog)": open the changelog as a
    /// native modal window and (re)load the release feed.
    pub(crate) fn open_changelog(&mut self, cx: &mut Context<Self>) {
        self.open_modal_window(ModalKind::Changelog, "What's New", 860.0, 620.0, cx);
        if self.changelog.notes.is_empty() {
            self.load_changelog(cx);
        }
    }

    /// Menu-bar action wrapper for [`Self::open_changelog`].
    pub(crate) fn act_open_changelog(
        &mut self,
        _: &OpenChangelog,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_changelog(cx);
    }

    /// Fetch the release list on a background thread (network I/O never on the UI thread) and
    /// hand the result to [`Self::set_changelog`].
    pub(crate) fn load_changelog(&mut self, cx: &mut Context<Self>) {
        if self.changelog.loading {
            return;
        }
        self.changelog.loading = true;
        self.changelog.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move { update::release_notes().map_err(|e| e.to_string()) })
                .await;
            this.update(cx, |this, cx| this.set_changelog(fetched, cx))
                .ok();
        })
        .detach();
    }

    /// Apply a fetch result: keep the notes, select the newest, and push its body into the
    /// markdown pane. Split out from the fetch so tests can drive the window without network.
    pub(crate) fn set_changelog(
        &mut self,
        fetched: std::result::Result<Vec<update::ReleaseNote>, String>,
        cx: &mut Context<Self>,
    ) {
        self.changelog.loading = false;
        match fetched {
            Ok(notes) if notes.is_empty() => {
                self.changelog.notes = notes;
                self.changelog.error = Some("No releases published yet.".into());
            }
            Ok(notes) => {
                self.changelog.notes = notes;
                self.changelog.error = None;
                self.changelog.selected = 0;
            }
            Err(e) => self.changelog.error = Some(e),
        }
        self.sync_changelog_body(cx);
        cx.notify();
    }

    /// Show release `i`'s notes.
    pub(crate) fn select_changelog(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.changelog.notes.len() {
            return;
        }
        self.changelog.selected = i;
        self.sync_changelog_body(cx);
        cx.notify();
    }

    /// Push the selected release's markdown into the preview entity (no base dir — the notes
    /// are remote, so relative image paths have nothing local to resolve against).
    fn sync_changelog_body(&mut self, cx: &mut Context<Self>) {
        let md = self
            .changelog
            .notes
            .get(self.changelog.selected)
            .map(|r| r.body.clone())
            .unwrap_or_default();
        self.changelog
            .body
            .update(cx, |v, cx| v.set_text(&md, None, cx));
    }

    /// Body of the "What's New" modal window (hosted by `ModalWindow`, native titlebar).
    pub(crate) fn render_changelog_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let ui = theme::font::UI_FAMILY;
        let ui_fs = px(t.ui_font_size);
        let running = update::current_version();

        // ── left: one row per published release ──
        let rows: Vec<gpui::AnyElement> = self
            .changelog
            .notes
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let sel = i == self.changelog.selected;
                let title: SharedString = format!("v{}", r.version).into();
                let meta: SharedString = match (r.date.as_str(), r.prerelease) {
                    ("", false) => String::new(),
                    ("", true) => "pre-release".into(),
                    (d, false) => d.to_string(),
                    (d, true) => format!("{d} · pre-release"),
                }
                .into();
                // Mark the version the user is actually running.
                let current = r.version == running;
                ui::picker::row(("changelog-row", i), sel, t.bg_light)
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .mx(px(4.0))
                    .px_3()
                    .py_1p5()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(div().text_color(t.text).child(title))
                            .when(current, |d| {
                                d.child(
                                    div()
                                        .px_1p5()
                                        .rounded_md()
                                        .bg(t.bg_mid)
                                        .text_size(px(t.ui_font_size - 2.0))
                                        .text_color(t.line_number)
                                        .child("current"),
                                )
                            }),
                    )
                    .when(!meta.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(px(t.ui_font_size - 1.0))
                                // `line_number` grey vanishes against the selected-row fill.
                                .text_color(if sel { t.secondary_text } else { t.line_number })
                                .child(meta),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.select_changelog(i, cx)),
                    )
                    .into_any_element()
            })
            .collect();

        let list = div()
            .id("changelog-list")
            .flex_none()
            .w(px(200.0))
            .h_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .py_2()
            .border_r(px(1.0))
            .border_color(t.divider)
            .children(rows);

        // ── right: the selected release's notes (or the loading / error state) ──
        let selected = self.changelog.notes.get(self.changelog.selected);
        let header = selected.map(|r| {
            let heading: SharedString = if r.title.trim().is_empty() {
                format!("v{}", r.version)
            } else {
                r.title.clone()
            }
            .into();
            let url = r.page_url.clone();
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_3()
                .px_4()
                .py_2()
                .border_b(px(1.0))
                .border_color(t.divider)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(t.text)
                        .font_weight(FontWeight::BOLD)
                        .child(heading),
                )
                .when(!url.is_empty(), |d| {
                    d.child(
                        div()
                            .id("changelog-open-github")
                            .flex_none()
                            .cursor_pointer()
                            .text_color(t.primary)
                            .hover(|s| s.text_color(t.text))
                            .child("Open on GitHub")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |_this, _e, _w, cx| cx.open_url(&url)),
                            ),
                    )
                })
        });

        let content: gpui::AnyElement = if self.changelog.loading {
            self.changelog_message("Loading releases…", None, cx)
        } else if let Some(err) = self.changelog.error.clone() {
            self.changelog_message(
                SharedString::from(format!("Couldn't load the changelog — {err}")),
                Some(()),
                cx,
            )
        } else {
            div()
                .id("changelog-body")
                .size_full()
                .overflow_y_scroll()
                .p_4()
                .child(self.changelog.body.clone())
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_row()
            .font_family(ui)
            .text_size(ui_fs)
            .text_color(t.text)
            .child(list)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .children(header)
                    .child(content),
            )
            .into_any_element()
    }

    /// Centered status text for the notes pane; `retry` adds a Retry button + a link to the
    /// releases page (the error state — the window is still useful offline).
    fn changelog_message(
        &self,
        msg: impl Into<SharedString>,
        retry: Option<()>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .text_color(t.line_number)
            .child(msg.into())
            .when(retry.is_some(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(btn_primary("changelog-retry", "Retry").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.load_changelog(cx)),
                        ))
                        .child(
                            btn_secondary("changelog-releases", "Open on GitHub").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_this, _e, _w, cx| {
                                    cx.open_url("https://github.com/kyle-ssg/kyde/releases");
                                }),
                            ),
                        ),
                )
            })
            .into_any_element()
    }
}
