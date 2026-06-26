//! First-run onboarding / keymap picker overlay + shell-command install row. Crate-root child.

use crate::*;

impl Kyde {
    pub(crate) fn render_onboarding(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let preset_card = |preset: Preset, cx: &mut Context<Self>| {
            let selected = self.onboarding_choice == preset;
            let sample: Vec<gpui::AnyElement> = keymap::ACTIONS
                .iter()
                .map(|a| {
                    let key = match preset {
                        Preset::VSCode => a.vscode,
                        _ => a.webstorm,
                    };
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .gap_4()
                        .child(SharedString::from(a.label))
                        .child(
                            div()
                                .px_2()
                                .bg(theme::get().bg_mid)
                                .rounded_md()
                                .text_color(theme::get().line_number)
                                .child(SharedString::from(pretty_key(key))),
                        )
                        .into_any_element()
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .gap_2()
                .w(px(300.0))
                .p_3()
                .rounded_lg()
                // Selection = thick accent border + a cool same-family gradient.
                .border_2()
                .border_color(if selected {
                    theme::get().primary
                } else {
                    theme::get().bg_light
                })
                .when(selected, |d| {
                    d.bg(gpui::linear_gradient(
                        145.0,
                        gpui::linear_color_stop(gpui::rgb(0x232838), 0.0),
                        gpui::linear_color_stop(gpui::rgb(0x2E3A5C), 1.0),
                    ))
                })
                .when(!selected, |d| d.bg(theme::get().panel_bg))
                .cursor_pointer()
                .child(
                    div()
                        .text_color(theme::get().text)
                        .child(SharedString::from(preset.label())),
                )
                .children(sample)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        this.onboarding_choice = preset;
                        cx.notify();
                    }),
                )
        };

        let panel = div()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(theme::get().bg_mid)
            .border_1()
            .border_color(theme::get().bg_light)
            .rounded_md()
            .shadow_lg()
            .font_family(ui)
            .text_size(fs)
            .text_color(theme::get().text)
            .child(
                div()
                    .text_color(theme::get().text)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Choose your keymap"),
            )
            .child(
                div()
                    .text_color(theme::get().line_number)
                    .child(if self.onboarding_forced {
                        "Pick a keymap to get started. You can change it later in Kyde → Settings."
                    } else {
                        "Reopen any time from Kyde → Settings (⌘,)."
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_4()
                    .child(preset_card(Preset::WebStorm, cx))
                    .child(preset_card(Preset::VSCode, cx)),
            )
            .child(self.render_shell_command_row(cx))
            // Single primary action, bottom-right: confirm the highlighted choice.
            .child(
                div().flex().flex_row().justify_end().mt_2().child(
                    div()
                        .px_5()
                        .py_1p5()
                        .rounded_md()
                        .bg(theme::get().primary)
                        .text_color(gpui::white())
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .child("Continue")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| {
                                // Apply the shell-command checkbox before closing: only
                                // when ticked and a name is actually free to claim.
                                if this.onboarding_install_cmd
                                    && matches!(shellcmd::state(), shellcmd::State::Available(_))
                                {
                                    if let Err(e) = shellcmd::install() {
                                        this.shell_cmd_error = Some(e);
                                    }
                                }
                                let choice = this.onboarding_choice;
                                this.choose_preset(choice, cx);
                            }),
                        ),
                ),
            );

        // Clicks inside the panel must not reach the backdrop.
        let panel = panel.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

        overlay(cx, !self.onboarding_forced)
            .child(panel)
            .into_any_element()
    }

    /// One row in the keymap picker: a checkbox to install a `ky`/`kyde` shell
    /// launcher (symlink into ~/.local/bin, VSCode-style). Shown on both first
    /// run and reopened Settings; the symlink is created when Continue confirms.
    /// Renders nothing when we can't resolve a location (`Unavailable`).
    fn render_shell_command_row(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let st = shellcmd::state();
        if matches!(st, shellcmd::State::Unavailable) {
            return div().into_any_element();
        }

        // Visual state of the box: installed → always on (and locked); a free
        // name → reflects the pending checkbox; taken → off (and locked).
        let (checked, enabled) = match &st {
            shellcmd::State::Installed(_) => (true, false),
            shellcmd::State::Available(_) => (self.onboarding_install_cmd, true),
            _ => (false, false),
        };
        let label = match &st {
            shellcmd::State::Installed(n) => {
                format!("Shell command installed — run `{n}` in any terminal")
            }
            shellcmd::State::Available(n) => {
                format!("Install `{n}` command — open Kyde from any terminal")
            }
            _ => "`ky` and `kyde` are already taken on your PATH — skipped".to_string(),
        };

        let checkbox = div()
            .size_4()
            .rounded_sm()
            .border_1()
            .border_color(if checked { t.primary } else { t.bg_light })
            .when(checked, |d| d.bg(t.primary))
            .flex()
            .items_center()
            .justify_center()
            .when(checked, |d| {
                d.child(
                    div()
                        .text_color(gpui::white())
                        .text_size(px(11.0))
                        .child("✓"),
                )
            });

        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .pt_3()
            .mt_1()
            .border_t_1()
            .border_color(t.divider)
            .child(checkbox)
            .child(
                div()
                    .text_color(if enabled {
                        t.secondary_text
                    } else {
                        t.line_number
                    })
                    .child(SharedString::from(label)),
            );

        if enabled {
            row = row.cursor_pointer().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.onboarding_install_cmd = !this.onboarding_install_cmd;
                    cx.notify();
                }),
            );
        }

        let mut col = div().flex().flex_col().gap_1().child(row);
        if let Some(err) = &self.shell_cmd_error {
            col = col.child(
                div()
                    .text_color(t.status_deleted)
                    .text_size(px(12.0))
                    .child(SharedString::from(err.clone())),
            );
        }
        col.into_any_element()
    }

    // ── keymap / onboarding ───────────────────────────────────────
    pub(crate) fn open_keymap(&mut self, _: &OpenKeymap, _: &mut Window, cx: &mut Context<Self>) {
        self.onboarding_choice = self.keymap.preset;
        self.onboarding_open = true;
        cx.notify();
    }

    pub(crate) fn choose_preset(&mut self, preset: Preset, cx: &mut Context<Self>) {
        self.keymap.set_preset(preset);
        self.keymap.save();
        apply_keymap(cx, &self.keymap);
        self.onboarding_open = false;
        self.onboarding_forced = false;
        cx.notify();
    }
}
