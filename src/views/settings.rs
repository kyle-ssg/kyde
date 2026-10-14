//! Settings window — a category sidebar (Appearance / Keymap / Language Packs) + a content
//! pane, IntelliJ-style. Hosted in a native `ModalWindow` (`ModalKind::Settings`). Changes
//! apply live via `theme::update` (font sizes, row height) / `choose_preset` (keymap).

use crate::{
    apply_keymap, btn_primary, btn_secondary, btn_secondary_state, div, key_parts, keymap,
    pretty_key, px, svg, theme, ui, Context, FluentBuilder, InteractiveElement, IntoElement, Kyde,
    ModalKind, MouseButton, ParentElement, Preset, SettingsSection, SharedString,
    StatefulInteractiveElement, Styled, Window,
};
use gpui::FontWeight;

impl Kyde {
    /// Open (or focus) the Settings window. Bound to ⌘, and the native Settings… menu.
    pub(crate) fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_theme.close();
        self.open_modal_window(ModalKind::Settings, "Settings", 800.0, 620.0, cx);
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
                            this.stop_key_recording();
                            this.settings_section = sec;
                            cx.notify();
                        }),
                    )
            }));

        let content = match section {
            SettingsSection::Appearance => self.settings_appearance(cx),
            SettingsSection::Keymap => self.settings_keymap(cx),
            SettingsSection::LanguagePacks => self.render_plugins_body(cx),
            SettingsSection::LocalHistory => self.settings_local_history(cx),
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

    /// Theme select — the reusable `ui::select` over the named palettes. The dropdown floats
    /// over the content (deferred absolute overlay), so it doesn't shove the size rows down.
    /// Picking a palette applies it live (preserving the user's font sizes / row height).
    pub(crate) fn theme_select(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::get();
        let presets = theme::Theme::presets();
        let labels: Vec<&'static str> = presets.iter().map(|(l, _)| *l).collect();
        let selected = presets.iter().position(|(_, p)| t.same_palette(p));
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap_3()
            .child(
                div()
                    .w(px(140.0))
                    .pt_1p5()
                    .text_color(t.secondary_text)
                    .child("Theme"),
            )
            .child(ui::select(
                cx,
                "theme-select",
                240.0,
                &labels,
                selected,
                &self.settings_theme,
                |k: &mut Kyde| &mut k.settings_theme,
                |_this, _cx| {},
                |_this, i, _cx| {
                    if let Some((_, palette)) = theme::Theme::presets().get(i) {
                        theme::apply_palette(*palette);
                    }
                },
            ))
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
            // Theme picker — a select dropdown over the named palettes (Kyde Dark/Light + the
            // colour-vision-deficiency variants). Too many for inline pills, so it's a
            // click-to-expand select. Switching preserves the user's font sizes / tree-row
            // height (only colours swap).
            .child(self.theme_select(cx))
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

    /// Local History section (issue #7): the master switch + retention/throttle steppers.
    /// Every change persists to `history.json` immediately; toggling the switch opens or
    /// drops the project store on the spot (`lh_sync_store`).
    fn settings_local_history(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let cfg = self.lh.cfg;
        let toggle = div()
            .id("lh-enabled")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .text_color(t.text)
            .child(crate::checkbox_box(cfg.enabled))
            .child("Enable local history")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.lh.cfg.enabled = !this.lh.cfg.enabled;
                    this.lh.cfg.save();
                    this.lh_sync_store(cx);
                    cx.notify();
                }),
            );
        // A labelled `[−] value [+]` stepper writing (clamped) into the persisted config.
        let num_row = |id: &'static str,
                       label: &'static str,
                       value: u32,
                       step: u32,
                       apply: fn(&mut kyde_config::history::HistoryCfg, i64),
                       cx: &mut Context<Self>| {
            let step_btn =
                |bid: SharedString, sym: &'static str, delta: i64, cx: &mut Context<Self>| {
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
                            cx.listener(move |this, _e, _w, cx| {
                                apply(&mut this.lh.cfg, delta);
                                this.lh.cfg = this.lh.cfg.clamped();
                                this.lh.cfg.save();
                                cx.notify();
                            }),
                        )
                };
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().w(px(160.0)).text_color(t.secondary_text).child(label))
                .child(step_btn(
                    format!("{id}-dec").into(),
                    "−",
                    -i64::from(step),
                    cx,
                ))
                .child(
                    div()
                        .w(px(40.0))
                        .flex()
                        .justify_center()
                        .text_color(t.text)
                        .child(SharedString::from(format!("{value}"))),
                )
                .child(step_btn(
                    format!("{id}-inc").into(),
                    "+",
                    i64::from(step),
                    cx,
                ))
        };
        let apply_days = |c: &mut kyde_config::history::HistoryCfg, d: i64| {
            c.retention_days = u32::try_from(i64::from(c.retention_days) + d).unwrap_or(1);
        };
        let apply_secs = |c: &mut kyde_config::history::HistoryCfg, d: i64| {
            c.throttle_secs = u32::try_from(i64::from(c.throttle_secs) + d).unwrap_or(1);
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .child(settings_heading("Local History"))
            .child(
                div()
                    .text_color(t.line_number)
                    .child("Snapshots of files as you work, independent of git. Right-click a file → Local History."),
            )
            .child(toggle)
            .when(cfg.enabled, |d| {
                d.child(num_row(
                    "lh-retention",
                    "Keep history (days)",
                    cfg.retention_days,
                    1,
                    apply_days,
                    cx,
                ))
                .child(num_row(
                    "lh-throttle",
                    "Snapshot throttle (seconds)",
                    cfg.throttle_secs,
                    5,
                    apply_secs,
                    cx,
                ))
            })
            // Destructive: wipes the OPEN project's snapshot store, behind its own
            // native confirmation window. Needs a store (project open + enabled).
            .when(self.lh.store.is_some(), |d| {
                // flex_row wrapper: keep the button at its natural width (a bare
                // flex_col child would stretch full-width).
                d.child(
                    div().flex().flex_row().child(
                        btn_secondary("lh-clear", "Clear Local History…")
                            .py_1()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _e, _w, cx| this.open_clear_local_history(cx)),
                            ),
                    ),
                )
            })
            .into_any_element()
    }

    /// Keymap section, laid out like the IDEs' shortcut editors: which preset the shortcuts are
    /// based on, confirmed "Reset to `WebStorm` / `IntelliJ`" / "Reset to VS Code" buttons, a
    /// search box, then a grouped Action | Shortcut table. Click a row and press the new keys
    /// (⌫ clears, Esc cancels); changed rows carry a "Custom" tag and a ↺ that resets just
    /// that action.
    fn settings_keymap(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let km = &self.keymap;
        let ed = &self.keymap_editor;
        let customised = km.overrides.len();

        // Which preset the shortcuts are based on — changed only through an explicit, confirmed
        // "Reset to …" (a dropdown that silently swapped every shortcut was too easy to hit).
        let base = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .child(
                div()
                    .text_color(t.secondary_text)
                    .child("Shortcuts based on"),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text)
                    .child(preset_name(km.preset)),
            )
            .when(customised > 0, |d| {
                d.child(
                    div()
                        .text_color(t.line_number)
                        .child(SharedString::from(format!("· {customised} custom"))),
                )
            });
        let buttons = div()
            .flex()
            .flex_row()
            .gap_2()
            .child(self.preset_reset_btn(Preset::WebStorm, "keymap-reset-ws", cx))
            .child(self.preset_reset_btn(Preset::VSCode, "keymap-reset-vsc", cx));
        let confirm = ed.confirm.map(|target| {
            let lost = if customised == 0 {
                String::new()
            } else {
                format!(
                    " Your {customised} custom shortcut{} will be lost.",
                    if customised == 1 { "" } else { "s" }
                )
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(t.divider)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(t.text)
                        .child(SharedString::from(format!(
                            "Replace every shortcut with the {} defaults?{lost}",
                            preset_name(target)
                        ))),
                )
                .child(
                    btn_secondary("keymap-reset-cancel", "Cancel")
                        .py_1()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| {
                                this.keymap_editor.confirm = None;
                                cx.notify();
                            }),
                        ),
                )
                .child(
                    btn_primary("keymap-reset-confirm", "Reset")
                        .py_1()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.confirm_preset_reset(cx)),
                        ),
                )
        });

        let search = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded_md()
            .border_1()
            .border_color(t.divider)
            .child(
                svg()
                    .path("icons/search.svg")
                    .size(px(14.0))
                    .flex_none()
                    .text_color(t.line_number),
            )
            .child(div().flex_1().min_w_0().child(ed.query.clone()))
            // Typing a search shouldn't be captured as a shortcut.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.stop_key_recording();
                    cx.notify();
                }),
            );

        let q = ed.query.read(cx).text().trim().to_lowercase();
        let mut body: Vec<gpui::AnyElement> = Vec::new();
        for (group, names) in keymap_groups() {
            let rows: Vec<gpui::AnyElement> = names
                .into_iter()
                .filter_map(|name| keymap::ACTIONS.iter().position(|a| a.name == name))
                .filter(|&i| {
                    let a = &keymap::ACTIONS[i];
                    let key = km.key_for(a.name).unwrap_or_default();
                    q.is_empty()
                        || a.label.to_lowercase().contains(&q)
                        || key.contains(&q)
                        || pretty_key(&key).to_lowercase().contains(&q)
                })
                .map(|i| self.keymap_row(i, cx))
                .collect();
            if rows.is_empty() {
                continue;
            }
            body.push(
                div()
                    .px_3()
                    .pt_3()
                    .pb_1()
                    .text_size(px(t.ui_font_size - 1.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.line_number)
                    .child(group)
                    .into_any_element(),
            );
            body.extend(rows);
        }
        if body.is_empty() {
            body.push(
                div()
                    .px_3()
                    .py_4()
                    .text_color(t.line_number)
                    .child("No shortcuts match.")
                    .into_any_element(),
            );
        }
        let columns = div()
            .flex()
            .flex_row()
            .px_3()
            .pb_1p5()
            .border_b_1()
            .border_color(t.divider)
            .text_size(px(t.ui_font_size - 1.0))
            .text_color(t.line_number)
            .child(div().flex_1().child("Action"))
            .child(div().w(px(SHORTCUT_COL_W)).child("Shortcut"));

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .track_focus(&ed.focus)
            .on_modifiers_changed(
                cx.listener(|this, ev: &gpui::ModifiersChangedEvent, _w, cx| {
                    if this.keymap_editor.recording.is_some() {
                        this.keymap_editor.held = ev.modifiers;
                        cx.notify();
                    }
                }),
            )
            .child(settings_heading("Keymap"))
            .child(base)
            .child(buttons)
            .children(confirm)
            .child(search)
            .child(
                div()
                    .text_color(t.line_number)
                    .child("Click an action and press its new keys — ⌫ clears it, Esc cancels."),
            )
            .when_some(ed.note.clone(), |d, note| {
                d.child(
                    div()
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .bg(t.bg_mid)
                        .text_color(t.secondary_text)
                        .child(SharedString::from(note)),
                )
            })
            .child(div().flex().flex_col().child(columns).children(body))
            .into_any_element()
    }

    /// One table row: the action, a "Custom" tag when changed, and its shortcut as key caps
    /// (or "Press keys…" while recording). Clicking the row records; ↺ resets the action.
    fn keymap_row(&self, i: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let a = &keymap::ACTIONS[i];
        let name = a.name;
        let km = &self.keymap;
        let recording = self.keymap_editor.recording == Some(name);
        let custom = km.is_customised(name);

        let held = held_modifiers(self.keymap_editor.held);
        let shortcut: gpui::AnyElement = if recording && held.is_empty() {
            div()
                .text_color(t.primary)
                .child("Press keys…")
                .into_any_element()
        } else if recording {
            // The modifiers held so far, translucent until a key completes the shortcut.
            key_caps(held.join("-").as_str())
                .opacity(0.5)
                .child(div().text_color(t.line_number).child("…"))
                .into_any_element()
        } else if let Some(k) = km.key_for(name) {
            key_caps(&k).into_any_element()
        } else {
            div()
                .text_color(t.line_number)
                .child("Not set")
                .into_any_element()
        };
        let reset = custom.then(|| {
            ui::icon_btn(("keymap-reset", i), 22.0)
                .tooltip(ui::tip("Reset to the preset's shortcut"))
                .child(
                    svg()
                        .path("icons/rotate-ccw.svg")
                        .size(px(13.0))
                        .text_color(t.line_number),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        cx.stop_propagation(); // not a click on the row (which records)
                        this.stop_key_recording();
                        let freed = this.keymap.reset(name);
                        let key = this.keymap.key_for(name).unwrap_or_default();
                        this.keymap_editor.note = moved_note(&key, &freed);
                        this.persist_keymap(cx);
                    }),
                )
        });
        let tag = custom.then(|| {
            div()
                .flex_none()
                .px_1p5()
                .rounded(px(4.0))
                .bg(t.selected_bg)
                .text_size(px(t.ui_font_size - 2.0))
                .text_color(t.text)
                .child("Custom")
        });

        div()
            .id(("keymap-row", i))
            .flex()
            .flex_row()
            .items_center()
            .h(px(32.0))
            .px_3()
            .rounded_md()
            .cursor_pointer()
            .border_1()
            .when(recording, |d| d.border_color(t.primary))
            .when(!recording, |d| {
                d.border_color(gpui::transparent_black())
                    .hover(|d| d.bg(t.bg_mid))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(t.text)
                            .child(SharedString::from(a.label)),
                    )
                    .children(tag),
            )
            .child(
                div()
                    .w(px(SHORTCUT_COL_W))
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(shortcut)
                    .child(div().w(px(22.0)).flex_none().children(reset)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, window, cx| {
                    this.start_key_recording(name, window, cx);
                }),
            )
            .into_any_element()
    }

    /// A "Reset to <preset>" button — a no-op (disabled) when the shortcuts already are that
    /// preset's untouched defaults; otherwise it asks for confirmation first.
    fn preset_reset_btn(
        &self,
        target: Preset,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let noop = self.keymap.preset == target && self.keymap.overrides.is_empty();
        btn_secondary_state(id, format!("Reset to {}", preset_name(target)), noop)
            .py_1()
            .when(!noop, |b| {
                b.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.request_preset_reset(target, cx)),
                )
            })
    }

    /// Ask to replace every shortcut with `preset`'s defaults. Nothing changes until
    /// [`Kyde::confirm_preset_reset`].
    pub(crate) fn request_preset_reset(&mut self, preset: Preset, cx: &mut Context<Self>) {
        self.stop_key_recording();
        self.keymap_editor.note = None;
        self.keymap_editor.confirm = Some(preset);
        cx.notify();
    }

    /// Apply the reset [`Kyde::request_preset_reset`] asked for.
    pub(crate) fn confirm_preset_reset(&mut self, cx: &mut Context<Self>) {
        if let Some(preset) = self.keymap_editor.confirm.take() {
            self.choose_preset(preset, cx);
            self.keymap_editor.note = Some(format!(
                "Shortcuts reset to the {} defaults.",
                preset_name(preset)
            ));
        }
    }

    /// Start recording `name`'s new shortcut: the next keystroke in the Settings window is
    /// captured before any binding can fire. A keystroke in another window cancels instead
    /// (and goes through), so a forgotten recording never swallows keys elsewhere.
    pub(crate) fn start_key_recording(
        &mut self,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.keymap_editor.note = None;
        self.keymap_editor.recording = Some(name);
        // Focus the section so modifier presses reach its listener (the live preview).
        window.focus(&self.keymap_editor.focus);
        self.keymap_editor.held = window.modifiers();
        let settings = self.settings_win.map(|h| h.window_id());
        let kyde = cx.entity().downgrade();
        self.keymap_editor.intercept = Some(cx.intercept_keystrokes(move |ev, window, cx| {
            let ours = settings == Some(window.window_handle().window_id());
            let ks = ev.keystroke.clone();
            let _ = kyde.update(cx, |k, cx| {
                if ours {
                    k.record_keystroke(&ks, cx);
                } else {
                    k.stop_key_recording();
                    cx.notify();
                }
            });
            if ours {
                cx.stop_propagation();
            }
        }));
        cx.notify();
    }

    /// Apply a captured keystroke to the action being recorded. Esc cancels, ⌫ / Delete
    /// unbinds, and a shortcut needs ⌘, ⌃ or ⌥ (or an F-key) — a bare letter would steal
    /// typing from every editor — so a bare key leaves recording on with a hint.
    pub(crate) fn record_keystroke(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        let Some(name) = self.keymap_editor.recording else {
            return;
        };
        let m = &ks.modifiers;
        let bare = !(m.platform || m.control || m.alt);
        let key = ks.key.as_str();
        let f_key =
            key.len() > 1 && key.starts_with('f') && key[1..].chars().all(|c| c.is_ascii_digit());
        match key {
            "escape" if bare => self.keymap_editor.note = None,
            "backspace" | "delete" if bare => {
                self.keymap.unbind(name);
                self.keymap_editor.note = None;
                self.persist_keymap(cx);
            }
            _ if bare && !f_key => {
                self.keymap_editor.note =
                    Some("Shortcuts need ⌘, ⌃ or ⌥ (or an F-key) — press again.".into());
                cx.notify();
                return;
            }
            _ => {
                let mut combo = held_modifiers(*m);
                combo.push(key);
                let combo = combo.join("-");
                let freed = self.keymap.rebind(name, &combo);
                self.keymap_editor.note = moved_note(&combo, &freed);
                self.persist_keymap(cx);
            }
        }
        self.stop_key_recording();
        cx.notify();
    }

    /// Stop capturing keystrokes for the keymap editor (drops the interceptor).
    pub(crate) fn stop_key_recording(&mut self) {
        self.keymap_editor.recording = None;
        self.keymap_editor.intercept = None;
        self.keymap_editor.held = gpui::Modifiers::default();
    }

    /// Save the keymap and re-bind every shortcut live.
    fn persist_keymap(&mut self, cx: &mut Context<Self>) {
        self.keymap.save();
        apply_keymap(cx, &self.keymap);
        cx.notify();
    }
}

/// A preset's display name.
fn preset_name(p: Preset) -> &'static str {
    match p {
        Preset::VSCode => "VS Code",
        // Custom keeps WebStorm defaults for anything it doesn't override.
        Preset::WebStorm | Preset::Custom => "WebStorm / IntelliJ",
    }
}

/// A shortcut as a row of key caps, one per key ("cmd-shift-o" → ⌘ ⇧ O).
fn key_caps(key: &str) -> gpui::Div {
    let t = theme::get();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .children(key_parts(key).into_iter().map(|cap| {
            div()
                .min_w(px(22.0))
                .h(px(22.0))
                .px_1p5()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .border_1()
                .border_color(t.divider)
                .bg(t.bg_light)
                .text_size(px(t.ui_font_size - 1.0))
                .text_color(t.text)
                .child(SharedString::from(cap))
        }))
}

/// The held modifiers as keymap tokens, in canonical order (`["cmd", "shift"]`).
fn held_modifiers(m: gpui::Modifiers) -> Vec<&'static str> {
    [
        (m.platform, "cmd"),
        (m.control, "ctrl"),
        (m.alt, "alt"),
        (m.shift, "shift"),
    ]
    .into_iter()
    .filter_map(|(on, tok)| on.then_some(tok))
    .collect()
}

/// Width of the table's Shortcut column (key caps + the ↺ slot).
const SHORTCUT_COL_W: f32 = 190.0;

/// "⌘⇧O moved from Go to File" — the actions a rebind took `key` from (`None` if none).
fn moved_note(key: &str, freed: &[&str]) -> Option<String> {
    (!freed.is_empty()).then(|| format!("{} moved from {}", pretty_key(key), freed.join(", ")))
}

/// The table's sections, in order, by action name. Anything `ACTIONS` gains that isn't
/// listed here lands in "Other", so a new action can't silently vanish from the editor.
fn keymap_groups() -> Vec<(&'static str, Vec<&'static str>)> {
    let mut groups = vec![
        (
            "Navigation",
            vec![
                "go_to_file",
                "find_in_files",
                "actions",
                "nav_back",
                "nav_forward",
                "mode_browse",
            ],
        ),
        (
            "Editing",
            vec!["save", "new_scratch", "sort_lines", "sort_keys"],
        ),
        ("Git", vec!["mode_commit", "commit", "push", "pull"]),
        ("General", vec!["open_keymap"]),
    ];
    let other: Vec<&'static str> = keymap::ACTIONS
        .iter()
        .map(|a| a.name)
        .filter(|n| !groups.iter().any(|(_, names)| names.contains(n)))
        .collect();
    if !other.is_empty() {
        groups.push(("Other", other));
    }
    groups
}

/// A section heading inside the Settings content pane.
fn settings_heading(label: &'static str) -> gpui::Div {
    div()
        .text_size(px(theme::get().ui_font_size + 4.0))
        .font_weight(FontWeight::BOLD)
        .text_color(theme::get().text)
        .child(label)
}
