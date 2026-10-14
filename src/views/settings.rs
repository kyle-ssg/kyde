//! Settings window — a category sidebar (Appearance / Keymap / Language Packs) + a content
//! pane, IntelliJ-style. Hosted in a native `ModalWindow` (`ModalKind::Settings`). Changes
//! apply live via `theme::update` (font sizes, row height) / `choose_preset` (keymap).

use crate::{
    apply_keymap, btn_secondary, btn_secondary_state, div, key_parts, keymap, pretty_key, px, svg,
    theme, ui, Context, FluentBuilder, InteractiveElement, IntoElement, Kyde, ModalKind,
    MouseButton, ParentElement, Preset, SettingsSection, SharedString, StatefulInteractiveElement,
    Styled,
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

    /// Keymap section, laid out like the IDEs' shortcut editors: a preset dropdown with
    /// "Reset to `WebStorm` / `IntelliJ` defaults" beside it, a search box, then a grouped
    /// Action | Shortcut table. Click a row and press the new keys (⌫ clears, Esc cancels);
    /// changed rows carry a "Custom" tag and a ↺ that resets just that action.
    fn settings_keymap(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let km = &self.keymap;
        let ed = &self.keymap_editor;
        let customised = km.overrides.len();

        let presets = [Preset::WebStorm, Preset::VSCode];
        let preset_select = ui::select(
            cx,
            "keymap-preset",
            200.0,
            &["WebStorm / IntelliJ", "VS Code"],
            presets.iter().position(|&p| p == km.preset),
            &ed.preset_select,
            |k: &mut Kyde| &mut k.keymap_editor.preset_select,
            |this, _cx| this.stop_key_recording(),
            move |this, i, cx| {
                if let Some(&p) = presets.get(i) {
                    this.keymap_editor.note = None;
                    this.choose_preset(p, cx);
                }
            },
        );
        let pristine = km.preset == Preset::WebStorm && customised == 0;
        let reset_all = btn_secondary_state(
            "keymap-reset-all",
            "Reset to WebStorm / IntelliJ defaults",
            pristine,
        )
        .py_1()
        .when(!pristine, |b| {
            b.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.stop_key_recording();
                    this.keymap_editor.note = None;
                    this.choose_preset(Preset::WebStorm, cx);
                }),
            )
        });
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .text_color(t.secondary_text)
                    .child("Preset"),
            )
            .child(preset_select)
            .child(div().flex_1())
            .child(reset_all);
        let preset_hint = (customised > 0).then(|| {
            div()
                .text_color(t.line_number)
                .child(SharedString::from(format!(
                    "{customised} custom shortcut{} — switching preset resets {}.",
                    if customised == 1 { "" } else { "s" },
                    if customised == 1 { "it" } else { "them" },
                )))
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
            .child(settings_heading("Keymap"))
            .child(header)
            .children(preset_hint)
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

        let shortcut: gpui::AnyElement = if recording {
            div()
                .text_color(t.primary)
                .child("Press keys…")
                .into_any_element()
        } else if let Some(k) = km.key_for(name) {
            div()
                .flex()
                .flex_row()
                .gap_1()
                .children(key_parts(&k).into_iter().map(|cap| {
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
                        .bg(t.bg_mid)
                        .text_size(px(t.ui_font_size - 1.0))
                        .text_color(t.text)
                        .child(SharedString::from(cap))
                }))
                .into_any_element()
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
            .when(recording, |d| d.border_color(t.primary).bg(t.bg_mid))
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
                cx.listener(move |this, _e, _w, cx| this.start_key_recording(name, cx)),
            )
            .into_any_element()
    }

    /// Start recording `name`'s new shortcut: the next keystroke in the Settings window is
    /// captured before any binding can fire. A keystroke in another window cancels instead
    /// (and goes through), so a forgotten recording never swallows keys elsewhere.
    pub(crate) fn start_key_recording(&mut self, name: &'static str, cx: &mut Context<Self>) {
        self.keymap_editor.note = None;
        self.keymap_editor.recording = Some(name);
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
                let mut combo = String::new();
                for (on, m) in [
                    (m.platform, "cmd-"),
                    (m.control, "ctrl-"),
                    (m.alt, "alt-"),
                    (m.shift, "shift-"),
                ] {
                    if on {
                        combo.push_str(m);
                    }
                }
                combo.push_str(key);
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
    }

    /// Save the keymap and re-bind every shortcut live.
    fn persist_keymap(&mut self, cx: &mut Context<Self>) {
        self.keymap.save();
        apply_keymap(cx, &self.keymap);
        cx.notify();
    }
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
