//! Native modal windows + their bodies: language-pack manager, fonts, clear-data, new-branch,
//! the install banner, and the ModalWindow plumbing (open/close/slot). Crate-root child module.

use super::*;

impl Kyde {
    pub(crate) fn render_install_banner(
        &self,
        pack: &'static highlight::Pack,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let ext = self
            .open_path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("");

        // ⓘ info badge
        let info = div()
            .flex_none()
            .size(px(18.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(t.primary)
            .child(
                div()
                    .text_size(px(11.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(gpui::white())
                    .child("i"),
            );

        let link = |label: SharedString, id: &'static str| {
            div()
                .id(id)
                .flex_none()
                .text_color(t.primary)
                .cursor_pointer()
                .hover(|s| s.text_color(t.text))
                .child(label)
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_4()
            .px_3()
            .py_2()
            .bg(t.bg_mid)
            .border_b_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(fs)
            .text_color(t.text)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(info)
                    .child(SharedString::from(format!("Plugins supporting *.{ext}"))),
            )
            .child(
                link(
                    format!("Install {} plugin", pack.name).into(),
                    "install-plugin",
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        this.install_open_pack(cx);
                        cx.notify();
                    }),
                ),
            )
            .child(link("Ignore extension".into(), "ignore-ext").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.ignore_open_pack(cx)),
            ))
            .into_any_element()
    }

    /// The language-plugin manager: every installable pack with its monogram badge,
    /// approximate compiled footprint, install state, and an Install/Uninstall button.
    /// Filtered live by the search box.
    /// Body of the Language-Plugins modal window (hosted by `ModalWindow`, native titlebar).
    pub(crate) fn render_plugins_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        let q = self.plugins_query.read(cx).text().to_lowercase();
        let q = q.trim();
        let mut packs: Vec<_> = highlight::PACKS
            .iter()
            .filter(|p| q.is_empty() || p.name.to_lowercase().contains(q) || p.id.contains(q))
            .collect();
        packs.sort_by_key(|p| p.name.to_lowercase());
        let rows: Vec<gpui::AnyElement> = packs
            .into_iter()
            .map(|p| {
                let installed = self.plugins.is_installed(p.id);
                let id = p.id;
                // Larger badge: the rows are two lines tall (name + size), so size the
                // language monogram to roughly match.
                let badge = badge_inner(
                    file_badge(std::path::Path::new(&format!("x.{}", pack_ext(p.id)))),
                    14.0,
                );
                let bid = SharedString::from(format!("pkg-{id}"));
                let btn = if installed {
                    btn_secondary(bid, "Uninstall")
                } else {
                    btn_primary(bid, "Install")
                }
                .flex_none()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.toggle_plugin(id, cx)),
                );
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .hover(|s| s.bg(t.bg_mid))
                    .child(
                        div()
                            .w(px(40.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(badge),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_color(t.text).child(SharedString::from(p.name)))
                            .child(
                                div()
                                    .text_color(t.line_number)
                                    .text_size(px(11.0))
                                    .child(SharedString::from(pack_size(p.id))),
                            ),
                    )
                    .child(btn)
                    .into_any_element()
            })
            .collect();

        // Fills the modal window (chrome + native titlebar come from `ModalWindow`).
        div()
            .size_full()
            .flex()
            .flex_col()
            .font_family(ui)
            .child(div().px_2().py_2().child(self.plugins_query.clone()))
            // Divider under the search input.
            .child(div().h(px(1.0)).bg(t.divider))
            .child(
                div()
                    .id("plugins-list")
                    .overflow_y_scroll()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .p_1()
                    .children(rows),
            )
            .into_any_element()
    }

    /// "Create New Branch" dialog body (own native window). Name field (`branch_query`) +
    /// Checkout / Overwrite toggles + Cancel / Create.
    pub(crate) fn render_new_branch_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        let (checkout, overwrite) = (self.new_branch_checkout, self.new_branch_overwrite);

        let name_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_4()
            .pt_4()
            .child(div().flex_none().text_color(t.text).child("Branch Name:"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(t.primary)
                    .bg(t.main_bg)
                    .font_family(theme::font::FAMILY)
                    .child(self.branch_query.clone()),
            );

        let check = |label: &'static str, on: bool, toggle: fn(&mut Self, &mut Context<Self>)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .child(checkbox_box(on))
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| toggle(this, cx)),
                )
        };
        let checks = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_6()
            .px_4()
            .pt_3()
            .child(check("Checkout branch", checkout, |this, cx| {
                this.new_branch_checkout = !this.new_branch_checkout;
                cx.notify();
            }))
            .child(check("Overwrite existing branch", overwrite, |this, cx| {
                this.new_branch_overwrite = !this.new_branch_overwrite;
                cx.notify();
            }));

        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .px_4()
            .pb_4()
            .pt_4()
            .child(
                btn_secondary("newbranch-cancel", "Cancel")
                    .py_1p5()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.close_modal_window(ModalKind::NewBranch, cx)
                        }),
                    ),
            )
            .child(btn_primary("create", "Create").py_1p5().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.do_create_branch(cx)),
            ));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.panel_bg)
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size))
            .text_color(t.text)
            .child(name_row)
            .child(checks)
            .child(div().flex_1()) // spacer → footer sits at the bottom
            .child(footer)
            .into_any_element()
    }

    /// Body of the "Clear Data & Restart" confirmation modal window. Destructive: wipes the
    /// whole config dir and restarts.
    pub(crate) fn render_clear_data_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        let cancel = btn_secondary("clear-cancel", "Cancel").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| this.close_modal_window(ModalKind::ClearData, cx)),
        );
        let confirm = btn_primary("clear-confirm", "Clear & Restart").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| this.do_clear_data(cx)),
        );
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size + 1.0))
            .child(div().text_color(t.text).child("Clear all data & restart?"))
            .child(
                div()
                    .flex_1()
                    .text_color(t.secondary_text)
                    .text_size(px(theme::get().ui_font_size))
                    .child(
                        "Uninstalls all language plugins and clears cached settings (keymap, \
                         theme, recent projects, preferences). Kyde will restart. Can't be undone.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(cancel)
                    .child(confirm),
            )
            .into_any_element()
    }

    /// Font specimen viewer: each bundled family at every available weight, previewing a
    /// full sentence at a large size so weight differences are obvious.
    /// Body of the Fonts modal window (hosted by `ModalWindow`, native titlebar).
    pub(crate) fn render_fonts_body(&mut self, _cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        const QUOTE: &str = "Me fail English? That's unpossible.";
        let line = |family: &'static str, label: &'static str, weight: FontWeight| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .py_2()
                .child(
                    div()
                        .font_family(ui)
                        .text_color(t.line_number)
                        .text_size(px(11.0))
                        .child(SharedString::from(label)),
                )
                .child(
                    div()
                        .font_family(family)
                        .font_weight(weight)
                        .text_size(px(26.0))
                        .text_color(t.text)
                        .child(QUOTE),
                )
        };
        let header = |title: &'static str| {
            div()
                .px_3()
                .pt_3()
                .pb_1()
                .font_family(ui)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.secondary_text)
                .text_size(px(12.0))
                .child(SharedString::from(title))
        };
        // Fills the modal window (chrome + native "Fonts" titlebar from `ModalWindow`).
        div()
            .size_full()
            .flex()
            .flex_col()
            .font_family(ui)
            .child(
                div()
                    .id("fonts-list")
                    .overflow_y_scroll()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .pb_2()
                    .child(header("Inter — UI"))
                    .child(line(
                        theme::font::UI_FAMILY,
                        "Regular · 400",
                        FontWeight::NORMAL,
                    ))
                    .child(line(
                        theme::font::UI_FAMILY,
                        "Medium · 500",
                        FontWeight::MEDIUM,
                    ))
                    .child(line(
                        theme::font::UI_FAMILY,
                        "SemiBold · 600",
                        FontWeight::SEMIBOLD,
                    ))
                    .child(line(theme::font::UI_FAMILY, "Bold · 700", FontWeight::BOLD))
                    .child(header("JetBrains Mono — Code"))
                    .child(line(
                        theme::font::FAMILY,
                        "Regular · 400",
                        FontWeight::NORMAL,
                    ))
                    .child(line(
                        theme::font::FAMILY,
                        "SemiBold · 600",
                        FontWeight::SEMIBOLD,
                    ))
                    .child(line(theme::font::FAMILY, "Bold · 700", FontWeight::BOLD)),
            )
            .into_any_element()
    }

    /// The window handle slot for a modal kind.
    fn modal_slot(&mut self, kind: ModalKind) -> &mut Option<gpui::WindowHandle<ModalWindow>> {
        match kind {
            ModalKind::Rollback => &mut self.rollback_win,
            ModalKind::Push => &mut self.push_win,
            ModalKind::Diff => &mut self.diff_win,
            ModalKind::NewBranch => &mut self.new_branch_win,
            ModalKind::Plugins => &mut self.plugins_win,
            ModalKind::Fonts => &mut self.fonts_win,
            ModalKind::ClearData => &mut self.clear_data_win,
        }
    }

    /// Open (or re-focus) a modal as its own native OS window. Opened from a spawned task so
    /// `cx.open_window` never runs inside this `Kyde` update — the new window's first render
    /// calls back into `Kyde` (`kyde.update`), which would panic re-entrantly otherwise. See
    /// the memory note on gpui phase/re-entrancy gotchas.
    pub(crate) fn open_modal_window(
        &mut self,
        kind: ModalKind,
        title: impl Into<SharedString>,
        w: f32,
        h: f32,
        cx: &mut Context<Self>,
    ) {
        let title = title.into();
        // Already open → just bring it forward (handle.update fails if it was closed).
        if let Some(existing) = *self.modal_slot(kind) {
            if existing
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return;
            }
            *self.modal_slot(kind) = None; // stale (user closed it) → fall through and reopen
        }
        let kyde = cx.entity();
        // The Diff modal opens at the MAIN window's bounds (captured each frame in
        // `impl Render for Kyde`) so it's as big as the editor and lands over it — NOT the
        // focused window's, which may be another modal (e.g. Rollback) it was launched from.
        let main_bounds = (kind == ModalKind::Diff)
            .then_some(self.main_window_bounds)
            .flatten();
        cx.spawn(async move |this, cx| {
            let opened = cx.update(|cx| {
                // Center on the display the main window is on (else gpui picks the primary
                // monitor, so the modal can pop up on a different screen than the IDE).
                let display = cx
                    .active_window()
                    .and_then(|w| {
                        w.update(cx, |_, window, cx| window.display(cx).map(|d| d.id()))
                            .ok()
                    })
                    .flatten();
                let window_bounds = main_bounds.unwrap_or_else(|| {
                    WindowBounds::Windowed(Bounds::centered(display, gpui::size(px(w), px(h)), cx))
                });
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(window_bounds),
                        titlebar: Some(gpui::TitlebarOptions {
                            title: Some(title.clone()),
                            appears_transparent: false,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    {
                        let kyde = kyde.clone();
                        move |_, cx| cx.new(|cx| ModalWindow::new(kyde.clone(), kind, cx))
                    },
                )
            });
            if let Ok(Ok(handle)) = opened {
                let _ = handle.update(cx, |view, window, cx| {
                    // New Branch: focus the name field so you can type immediately. Others:
                    // focus the root so Escape (on_key_down) dispatches.
                    if view.kind == ModalKind::NewBranch {
                        let input = view.kyde.read(cx).branch_query.read(cx).focus_handle(cx);
                        window.focus(&input);
                    } else if view.kind == ModalKind::Plugins {
                        let input = view.kyde.read(cx).plugins_query.read(cx).focus_handle(cx);
                        window.focus(&input);
                    } else {
                        let fh = view.focus_handle(cx);
                        window.focus(&fh);
                    }
                    cx.activate(true);
                });
                this.update(cx, |k, _| *k.modal_slot(kind) = Some(handle))
                    .ok();
            }
        })
        .detach();
    }

    /// Close a modal's native window (if open) and clear its handle. The actual
    /// `remove_window` is deferred: it's often called from *inside* that window's own button
    /// handler (e.g. the rollback window's "Rollback" button → `do_rollback`), and removing a
    /// window mid-dispatch of its own event is re-entrant; deferring runs it once the current
    /// effect cycle finishes.
    pub(crate) fn close_modal_window(&mut self, kind: ModalKind, cx: &mut Context<Self>) {
        if let Some(handle) = self.modal_slot(kind).take() {
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
    }

    /// Pack available for the open file but not yet installed (drives the banner).
    pub(crate) fn pending_pack(&self) -> Option<&'static highlight::Pack> {
        self.open_path
            .as_ref()
            .and_then(|p| Lang::from_path(p).pack())
            .filter(|p| !self.plugins.is_installed(p.id) && !self.ignored_packs.contains(p.id))
    }

    /// Dismiss the install banner for the open file's type (session-only).
    pub(crate) fn ignore_open_pack(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.pending_pack() {
            self.ignored_packs.insert(p.id);
            cx.notify();
        }
    }

    /// Install the pack for the open file and re-highlight it in place
    /// (without disturbing the buffer's content, selection, or dirty flag).
    pub(crate) fn install_open_pack(&mut self, cx: &mut Context<Self>) {
        let Some(rel) = self.open_path.clone() else {
            return;
        };
        let lang = Lang::from_path(&rel);
        if let Some(p) = lang.pack() {
            self.plugins.install(p.id);
            self.plugins.save();
            // Re-highlight in place so the colors appear immediately — previously this only
            // set the lang, leaving the cached (plain) spans until the file was reopened.
            self.file_editor.update(cx, |e, cx| e.set_lang(lang, cx));
        }
    }

    /// Escape: close whatever overlay is open (most-transient first); if none, cancel the
    /// Commit view back to Browse. A no-op in plain Browse.
    /// Native-menu "Plugins…": open the language-pack manager (native modal window).
    pub(crate) fn act_open_plugins(
        &mut self,
        _: &OpenPlugins,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_modal_window(ModalKind::Plugins, "Language Plugins", 520.0, 560.0, cx);
    }

    /// Native-menu "Clear Data & Restart…": open the confirmation as a native modal window.
    pub(crate) fn act_clear_data(
        &mut self,
        _: &ClearData,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_modal_window(
            ModalKind::ClearData,
            "Clear Data & Restart",
            460.0,
            230.0,
            cx,
        );
    }

    /// Confirmed: wipe the config dir (uninstalls every plugin, drops keymap/theme/projects/
    /// ui prefs) and restart into a clean first-run state.
    pub(crate) fn do_clear_data(&mut self, cx: &mut Context<Self>) {
        let _ = std::fs::remove_dir_all(crate::config_dir());
        cx.restart();
    }

    /// Toggle a language pack's installed state from the plugin manager, persist it, and
    /// re-highlight the open file in place if it's affected (so colors appear/clear at once).
    /// Install a pack by id (used by the font-file install prompt), persist, and refresh the
    /// relevant preview/highlight so it applies immediately.
    pub(crate) fn install_pack(&mut self, id: &str, cx: &mut Context<Self>) {
        self.plugins.install(id);
        self.plugins.save();
        if id == "font" {
            self.load_font_preview(cx);
        } else if let Some(rel) = self.open_path.clone() {
            let eff = self.effective_lang(&rel);
            self.file_editor.update(cx, |e, cx| e.set_lang(eff, cx));
        }
        cx.notify();
    }

    pub(crate) fn toggle_plugin(&mut self, pack_id: &str, cx: &mut Context<Self>) {
        if self.plugins.is_installed(pack_id) {
            self.plugins.uninstall(pack_id);
        } else {
            self.plugins.install(pack_id);
        }
        self.plugins.save();
        if pack_id == "font" {
            self.load_font_preview(cx);
        } else if let Some(rel) = self.open_path.clone() {
            // If the open file's language maps to this pack, re-highlight it now.
            let lang = Lang::from_path(&rel);
            if lang.pack().map(|p| p.id) == Some(pack_id) {
                let eff = self.effective_lang(&rel);
                self.file_editor.update(cx, |e, cx| e.set_lang(eff, cx));
            }
        }
        cx.notify();
    }

}

// ── plugin-pack metadata (file extension + approx compiled size) ──
/// Representative file extension for a pack id, so `file_badge` can pick the language
/// monogram chip shown in the plugin manager.
pub(crate) fn pack_ext(id: &str) -> &'static str {
    match id {
        "json" => "json",
        "typescript" => "ts",
        "javascript" => "js",
        "rust" => "rs",
        "markdown" => "md",
        "shell" => "sh",
        "css" => "css",
        "scss" => "scss",
        "yaml" => "yml",
        "toml" => "toml",
        "python" => "py",
        "html" => "html",
        "go" => "go",
        "env" => "env",
        "gitignore" => "gitignore",
        "font" => "ttf",
        _ => "txt",
    }
}

/// Approximate compiled footprint of a pack's grammar (tree-sitter parse tables linked
/// into the binary). These ship in the binary rather than being downloaded, so this is the
/// resident size each adds — a rough, static figure, not an exact per-build measurement.
pub(crate) fn pack_size(id: &str) -> &'static str {
    match id {
        "json" => "~55 KB",
        "typescript" => "~2.6 MB",
        "javascript" => "~1.1 MB",
        "rust" => "~1.6 MB",
        "markdown" => "~210 KB",
        "shell" => "~480 KB",
        "css" => "~260 KB",
        "scss" => "shares CSS grammar",
        "yaml" => "~150 KB",
        "toml" => "~120 KB",
        "python" => "~900 KB",
        "html" => "~120 KB",
        "go" => "~700 KB",
        "env" | "gitignore" => "built-in (no grammar)",
        "font" => "preview only",
        _ => "—",
    }
}
