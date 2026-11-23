//! Quick actions: once an agent tab's turn ends, the follow-ups worth one click — Merge when
//! green, Merge release PR when ready, Fix CI, … — along its pane's bottom edge, beside its
//! model. Which apply comes from its worktree's git and GitHub state (`agents::quick_actions`);
//! a small model then picks the ones that fit where its conversation is. A click types the
//! action's prompt to its Claude Code, which does the work.

use super::*;
use crate::agents::{Hook, QuickAction};

impl Kyde {
    /// Pick the on-screen tab's quick actions, once per turn it finishes — off the UI thread
    /// (git, `gh`, a small model; a few seconds). Only the tab you're looking at: each pick
    /// costs a model call.
    pub(super) fn agents_pick_actions(&mut self, cx: &mut Context<Self>) {
        // Never `gh` or `claude` from tests.
        if cfg!(test) {
            return;
        }
        let Some((wt, i)) = self.agents_visible_tab().map(|(w, i)| (w.to_path_buf(), i)) else {
            return;
        };
        let Some(t) = self
            .agents
            .tabs
            .get_mut(&wt)
            .and_then(|g| g.tabs.get_mut(i))
        else {
            return;
        };
        let Some((Hook::Done, done)) = t.hook else {
            return;
        };
        if !t.agent || t.actions_for == Some(done) {
            return;
        }
        t.actions_for = Some(done);
        t.actions.clear();
        let (id, session) = (t.id.clone(), t.session.clone());
        let tail = t.view.read(cx).tail_text(40);
        cx.spawn(async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async move {
                    let options =
                        crate::agents::quick_actions(&crate::agents::read_action_facts(&wt));
                    if options.is_empty() {
                        return options;
                    }
                    let ctx = session
                        .as_deref()
                        .and_then(|s| crate::agents::read_session_context(&wt, s))
                        .unwrap_or_default();
                    let field = |f: Option<String>| f.map_or("(unknown)".into(), |s| clip(&s, 400));
                    let prompt = crate::agents::actions_prompt(
                        &field(ctx.title),
                        &field(ctx.last_prompt),
                        &tail,
                        &options,
                    );
                    crate::agents::pick_actions(&prompt, options)
                })
                .await;
            this.update(cx, |this, cx| {
                let tab = this
                    .agents
                    .tabs
                    .values_mut()
                    .flat_map(|g| g.tabs.iter_mut())
                    .find(|t| t.id == id);
                // Another turn may have ended meanwhile: that one gets its own pick.
                if let Some(t) = tab.filter(|t| t.actions_for == Some(done)) {
                    t.actions = picked;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Tab `tab`'s quick actions, for its pane's bottom edge — only while the turn they were
    /// picked for is still its latest (a new one hides them until it ends).
    pub(super) fn render_agents_actions(
        &self,
        wt: &Path,
        tab: &AgentTab,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let current = matches!(tab.hook, Some((Hook::Done, at)) if tab.actions_for == Some(at));
        if !current {
            return Vec::new();
        }
        let fs = theme::get().ui_font_size;
        tab.actions
            .iter()
            .enumerate()
            .map(|(i, action)| {
                let (wt, id, action) = (wt.to_path_buf(), tab.id.clone(), action.clone());
                btn_secondary(("agents-action", i), action.label)
                    .debug_selector(move || format!("agents-action-{i}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(20.0))
                    .px_2()
                    .py_0()
                    .text_size(px(fs - 1.0))
                    .tooltip(ui::tip(action.hint.clone()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                            this.agents_send_action(&wt, &id, &action, e.position, cx);
                        }),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// Hand quick action `action` to tab `id`'s Claude Code — or, when it can't take a command
    /// right now, say why, standing on the button at `at`.
    fn agents_send_action(
        &mut self,
        wt: &Path,
        id: &str,
        action: &QuickAction,
        at: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if let Some(why) = self.agents_send_blocker(wt, id, cx) {
            let at = gpui::point(at.x + px(12.0), at.y - px(12.0));
            self.open_menu(at, MenuTarget::AgentWhy(why), cx);
            return;
        }
        let Some(t) = self
            .agents
            .tabs
            .get_mut(wt)
            .and_then(|g| g.tabs.iter_mut().find(|t| t.id == id))
        else {
            return;
        };
        // On its way: the next turn's end picks again.
        t.actions.clear();
        t.view
            .update(cx, |v, cx| v.submit_prompt(&action.prompt, true, cx));
        self.agents.focus_pending = true;
        cx.notify();
    }
}
