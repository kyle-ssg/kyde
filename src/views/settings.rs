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
            SettingsSection::Agents => self.settings_agents(cx),
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

    /// Agents section: the default start command (each space can override it from its header
    /// in the Agents view) and the idle-tab cleanup policy. Persists as you edit.
    fn settings_agents(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let cfg = &self.agents.cfg;
        let (mode, rules, hours) = (cfg.cleanup_mode, cfg.cleanup, cfg.idle_hours);
        // A `[−] value [+]` stepper writing into the config (clamped by `apply`).
        let stepper = |id: &'static str,
                       value: u32,
                       apply: fn(&mut kyde_config::agents::AgentsCfg, i64),
                       cx: &mut Context<Self>| {
            let btn = |bid: SharedString, sym: &'static str, delta: i64, cx: &mut Context<Self>| {
                div()
                    .id(bid)
                    .size(px(22.0))
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
                            apply(&mut this.agents.cfg, delta);
                            this.agents_save(cx);
                            cx.notify();
                        }),
                    )
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(btn(format!("{id}-dec").into(), "−", -1, cx))
                .child(
                    div()
                        .w(px(36.0))
                        .flex()
                        .justify_center()
                        .text_color(t.text)
                        .child(SharedString::from(value.to_string())),
                )
                .child(btn(format!("{id}-inc").into(), "+", 1, cx))
        };
        // One cleanup rule: a checkbox, its label, and maybe a stepper / a note.
        let rule = |id: &'static str,
                    on: bool,
                    label: &'static str,
                    toggle: fn(&mut kyde_config::agents::CleanupRules),
                    cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .text_color(t.text)
                .child(crate::checkbox_box(on))
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        toggle(&mut this.agents.cfg.cleanup);
                        this.agents_save(cx);
                        cx.notify();
                    }),
                )
        };
        // An on/off setting of the Agents view.
        let toggle = |id: &'static str,
                      on: bool,
                      label: &'static str,
                      flip: fn(&mut kyde_config::agents::AgentsCfg),
                      cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .text_color(t.text)
                .child(crate::checkbox_box(on))
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        let hooks = this.agents.cfg.claude_hooks;
                        flip(&mut this.agents.cfg);
                        if this.agents.cfg.claude_hooks != hooks {
                            this.agents.hooks_synced = false;
                            this.agents_sync_hooks(cx);
                        }
                        this.agents_save(cx);
                        cx.notify();
                    }),
                )
        };
        let row = cfg.row;
        // One prompt template: its label, a reset to the standard text, and the editor.
        let prompt = |id: &'static str,
                      label: &'static str,
                      editor: gpui::Entity<crate::CodeEditor>,
                      standard: &'static str,
                      cx: &mut Context<Self>| {
            let ed = editor.clone();
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(div().text_color(t.secondary_text).child(label))
                        .child(div().flex_1())
                        .child(
                            div()
                                .id(id)
                                .text_size(px(t.ui_font_size - 1.0))
                                .text_color(t.line_number)
                                .cursor_pointer()
                                .hover(|d| d.text_color(t.text))
                                .child("Reset to standard")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |_this, _e, _w, cx| {
                                        // The editor's change saves it (as "standard").
                                        ed.update(cx, |e, cx| {
                                            e.set_content(
                                                standard.to_string(),
                                                crate::Lang::PlainText,
                                                cx,
                                            );
                                        });
                                    }),
                                ),
                        ),
                )
                // Grows to fit the whole (wrapped) prompt.
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(t.divider)
                        .bg(t.main_bg)
                        .child(editor),
                )
        };
        let mode_pill = |id: &'static str,
                         label: &'static str,
                         m: kyde_config::agents::CleanupMode,
                         cx: &mut Context<Self>| {
            crate::tab_pill(id, label, 0, mode == m).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| {
                    this.agents.cfg.cleanup_mode = m;
                    this.agents_save(cx);
                    cx.notify();
                }),
            )
        };
        let new_task = cfg.new_task;
        let task_pill = |id: &'static str,
                         label: &'static str,
                         m: kyde_config::agents::NewTask,
                         cx: &mut Context<Self>| {
            crate::tab_pill(id, label, 0, new_task == m).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| {
                    this.agents.cfg.new_task = m;
                    this.agents_save(cx);
                    cx.notify();
                }),
            )
        };
        let task_note = match new_task {
            kyde_config::agents::NewTask::Move => {
                "A new tab's agent hears what's already in its worktree — another agent's \
                 work, uncommitted changes, unpushed commits. Give it an unrelated task and it \
                 moves to a fresh worktree off the latest main; the tab follows."
            }
            kyde_config::agents::NewTask::Ask => {
                "Same, but the agent asks you before it moves an unrelated task to a fresh \
                 worktree."
            }
            kyde_config::agents::NewTask::Stay => {
                "A new tab works where you opened it, whatever you ask."
            }
        };
        let mode_note = match mode {
            kyde_config::agents::CleanupMode::Off => "Tabs stay open until you close them.",
            kyde_config::agents::CleanupMode::Suggest => {
                "Finished tabs get a ✓ — close them from the Clean Up window (the eraser)."
            }
            kyde_config::agents::CleanupMode::Auto => {
                "Finished tabs close on their own (checked every minute)."
            }
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .child(settings_heading("Agents"))
            .child(
                div().text_color(t.line_number).child(
                    "Projects → worktrees → tabs. Each new tab starts with the start command.",
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(160.0))
                            .text_color(t.secondary_text)
                            .child("Start command"),
                    )
                    .child(
                        div()
                            .w(px(260.0))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(t.divider)
                            .bg(t.main_bg)
                            .font_family(theme::font::FAMILY)
                            .child(self.agents.default_cmd_input.clone()),
                    ),
            )
            .child(
                div()
                    .id("agents-only")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .text_color(t.text)
                    .child(crate::checkbox_box(cfg.agents_only))
                    .child("Agents-only window — hide the IDE rail, toolbars and status bar")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            let on = !this.agents.cfg.agents_only;
                            this.agents_set_only(on, cx);
                        }),
                    ),
            )
            .child(
                div()
                    .pt_2()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("New task in a busy worktree"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .child(task_pill(
                        "new-task-move",
                        "Move",
                        kyde_config::agents::NewTask::Move,
                        cx,
                    ))
                    .child(task_pill(
                        "new-task-ask",
                        "Ask",
                        kyde_config::agents::NewTask::Ask,
                        cx,
                    ))
                    .child(task_pill(
                        "new-task-stay",
                        "Stay",
                        kyde_config::agents::NewTask::Stay,
                        cx,
                    )),
            )
            .child(div().text_color(t.line_number).child(task_note))
            .when(!cfg.claude_hooks && new_task != kyde_config::agents::NewTask::Stay, |d| {
                d.child(
                    div()
                        .text_color(t.line_number)
                        .child("Needs the Claude Code hook (under Alerts) — it's off."),
                )
            })
            .child(
                div()
                    .pt_2()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Prompts"),
            )
            .child(div().text_color(t.line_number).child(
                "What an agent is told when you Start an issue or Review a pull request. \\
                 Placeholders: {number} {title} {url} {branch} {base} {author}.",
            ))
            .child(prompt(
                "prompt-issue-reset",
                "Start an issue",
                self.agents.issue_prompt_input.clone(),
                kyde_config::agents::ISSUE_PROMPT,
                cx,
            ))
            .child(prompt(
                "prompt-review-reset",
                "Review a pull request",
                self.agents.review_prompt_input.clone(),
                kyde_config::agents::REVIEW_PROMPT,
                cx,
            ))
            .child(prompt(
                "prompt-rereview-reset",
                "Re-review a pull request",
                self.agents.rereview_prompt_input.clone(),
                kyde_config::agents::REREVIEW_PROMPT,
                cx,
            ))
            .child(
                div()
                    .pt_2()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Alerts"),
            )
            .child(toggle(
                "agents-hooks",
                cfg.claude_hooks,
                "Claude Code hook — shows when an agent needs input, and resumes the exact session",
                |c| c.claude_hooks = !c.claude_hooks,
                cx,
            ))
            .child(div().pl(px(26.0)).text_color(t.line_number).child(
                "Adds a small hook to ~/.claude/settings.json that writes each Kyde agent's \
                 state to ~/.kyde/agents. It does nothing outside Kyde's tabs.",
            ))
            .child(toggle(
                "agents-sound",
                cfg.sound,
                "Play a sound when an agent finishes or needs input",
                |c| c.sound = !c.sound,
                cx,
            ))
            .child(toggle(
                "agents-dock",
                cfg.dock_alerts,
                if cfg!(target_os = "macos") {
                    "Bounce the Dock icon while Kyde is in the background"
                } else {
                    "Show a desktop notification while Kyde is in the background"
                },
                |c| c.dock_alerts = !c.dock_alerts,
                cx,
            ))
            .child(
                div()
                    .pt_2()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Sidebar"),
            )
            .child(toggle(
                "agents-sort",
                cfg.sort_attention,
                "List what needs you first (needs input, done, working, idle) — turns off drag to reorder",
                |c| c.sort_attention = !c.sort_attention,
                cx,
            ))
            .child(
                div()
                    .text_color(t.secondary_text)
                    .child("Each worktree row shows:"),
            )
            .child(toggle(
                "agents-row-title",
                row.ai_title,
                "Its agent's AI title (else the folder / branch name)",
                |c| c.row.ai_title = !c.row.ai_title,
                cx,
            ))
            .child(toggle(
                "agents-row-status",
                row.status,
                "What the agent needs — \"needs input\", \"done\"",
                |c| c.row.status = !c.row.status,
                cx,
            ))
            .child(toggle(
                "agents-row-git",
                row.git,
                "Git badges — changes, commits to push, clean",
                |c| c.row.git = !c.row.git,
                cx,
            ))
            .child(toggle(
                "agents-row-activity",
                row.activity,
                "How long since its agent last did anything",
                |c| c.row.activity = !c.row.activity,
                cx,
            ))
            .child(toggle(
                "agents-single-tab",
                cfg.hide_single_tab,
                "Hide the tab strip while a worktree has one tab",
                |c| c.hide_single_tab = !c.hide_single_tab,
                cx,
            ))
            .child(
                div()
                    .pt_2()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Tab cleanup"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .child(mode_pill(
                        "cleanup-off",
                        "Off",
                        kyde_config::agents::CleanupMode::Off,
                        cx,
                    ))
                    .child(mode_pill(
                        "cleanup-suggest",
                        "Suggest",
                        kyde_config::agents::CleanupMode::Suggest,
                        cx,
                    ))
                    .child(mode_pill(
                        "cleanup-auto",
                        "Auto",
                        kyde_config::agents::CleanupMode::Auto,
                        cx,
                    )),
            )
            .child(div().text_color(t.line_number).child(mode_note))
            .when(mode != kyde_config::agents::CleanupMode::Off, |d| {
                d.child(
                    div()
                        .text_color(t.secondary_text)
                        .child("A tab is finished when:"),
                )
                .child(rule(
                    "rule-unused",
                    rules.unused,
                    "It was never used — nothing typed into it (an auto-started agent)",
                    |r| r.unused = !r.unused,
                    cx,
                ))
                .child(rule(
                    "rule-quit",
                    rules.agent_quit,
                    "Its agent has quit — only a shell prompt is left",
                    |r| r.agent_quit = !r.agent_quit,
                    cx,
                ))
                .child(rule(
                    "rule-merged",
                    rules.merged,
                    "Its pull request is merged",
                    |r| r.merged = !r.merged,
                    cx,
                ))
                .child(rule(
                    "rule-ai",
                    rules.ai_done,
                    "An AI check says its work is done",
                    |r| r.ai_done = !r.ai_done,
                    cx,
                ))
                .child(div().pl(px(26.0)).text_color(t.line_number).child(
                    "`claude -p` on Haiku reads its title, prompts, git state and last screen \
                         — asked of quiet tabs, and again only after new output.",
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(rule(
                            "rule-idle",
                            rules.idle,
                            "Idle for",
                            |r| r.idle = !r.idle,
                            cx,
                        ))
                        .child(stepper(
                            "idle-hours",
                            hours,
                            |c, d| {
                                c.idle_hours =
                                    u32::try_from((i64::from(c.idle_hours) + d).clamp(1, 720))
                                        .unwrap_or(1);
                            },
                            cx,
                        ))
                        .child(
                            div()
                                .text_color(t.text)
                                .child("hours, with a clean worktree"),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_color(t.secondary_text)
                                .child("…once it's been quiet for"),
                        )
                        .child(stepper(
                            "quiet-mins",
                            rules.quiet_mins,
                            |c, d| {
                                c.cleanup.quiet_mins = u32::try_from(
                                    (i64::from(c.cleanup.quiet_mins) + d).clamp(1, 240),
                                )
                                .unwrap_or(1);
                            },
                            cx,
                        ))
                        .child(div().text_color(t.secondary_text).child("minutes")),
                )
                .child(div().text_color(t.line_number).child(
                    "The tab on screen and output you haven't seen are never closed. A closed \
                         agent tab's Claude session can be resumed.",
                ))
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
        let bare = held_modifiers(*m).iter().all(|&tok| tok == "shift");
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
                self.keymap_editor.note = Some(
                    if cfg!(target_os = "macos") {
                        "Shortcuts need ⌘, ⌃ or ⌥ (or an F-key) — press again."
                    } else {
                        "Shortcuts need Ctrl or Alt (or an F-key) — press again."
                    }
                    .into(),
                );
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

/// The held modifiers as keymap tokens, in canonical order (`["cmd", "shift"]`). A keymap
/// writes the primary modifier as `cmd` on every OS (see `keymap::platform_key`), so off macOS
/// Ctrl records as `cmd`; Super belongs to the desktop there and isn't recorded.
fn held_modifiers(m: gpui::Modifiers) -> Vec<&'static str> {
    let (cmd, ctrl) = if cfg!(target_os = "macos") {
        (m.platform, m.control)
    } else {
        (m.control, false)
    };
    [
        (cmd, "cmd"),
        (ctrl, "ctrl"),
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
        (
            "Agents",
            vec![
                "agent_next",
                "agent_prev",
                "agent_attention",
                "agent_tab_next",
                "agent_tab_prev",
                "agent_new_worktree",
                "agent_rename",
                "agent_scrollback",
                "agent_toggle_sidebar",
            ],
        ),
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
