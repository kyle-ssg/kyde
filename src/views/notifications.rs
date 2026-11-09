//! Transient banners: crash report, last-op error, update-available. Crate-root child module.

use crate::*;
use std::time::{Duration, Instant};

/// A window refocus asks GitHub again once the last release check is this old.
const UPDATE_RECHECK_EVERY: Duration = Duration::from_mins(15);

impl Kyde {
    /// Ask GitHub if there's a newer release, then surface the update banner. Network I/O off
    /// the UI thread; failures stay silent.
    pub(crate) fn check_for_update(&mut self, cx: &mut Context<Self>) {
        self.self_update.checked_at = Some(Instant::now());
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { update::check().ok().flatten() })
                .await;
            if let Some(rel) = found {
                this.update(cx, |this, cx| this.offer_update(rel, cx)).ok();
            }
        })
        .detach();
    }

    /// On window refocus: recheck for a release when the last check is stale (and no update
    /// is already downloading), so a long-running window still learns about new versions.
    pub(crate) fn recheck_update_if_stale(&mut self, cx: &mut Context<Self>) {
        let stale = self
            .self_update
            .checked_at
            .is_none_or(|at| at.elapsed() >= UPDATE_RECHECK_EVERY);
        if stale && !self.self_update.updating {
            self.check_for_update(cx);
        }
    }

    /// Show the banner for `rel` — unless that version was dismissed, or a download of the
    /// offered one is already running.
    pub(crate) fn offer_update(&mut self, rel: update::Release, cx: &mut Context<Self>) {
        let su = &mut self.self_update;
        if su.updating || su.dismissed.as_deref() == Some(rel.version.as_str()) {
            return;
        }
        su.available = Some(rel);
        cx.notify();
    }

    /// Top-of-window banner shown only when a newer release exists. The action is
    /// "Update & Relaunch" when running from a `.app` bundle (downloads + swaps in place),
    /// else "Download" (opens the release page) — see `do_update`.
    pub(crate) fn render_update_banner(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let Some(rel) = self.self_update.available.as_ref() else {
            return div().into_any_element();
        };
        let can_swap = update::running_bundle().is_some() && !rel.zip_url.is_empty();
        let action_label = if self.self_update.updating {
            "Updating…"
        } else if can_swap {
            "Update & Relaunch"
        } else {
            "Download"
        };
        let msg: SharedString = format!("Update available — v{}", rel.version).into();

        // ↑ badge
        let badge = div()
            .flex_none()
            .size(px(18.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(t.primary)
            .child(
                div()
                    .text_size(px(12.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(gpui::white())
                    .child("↑"),
            );

        let updating = self.self_update.updating;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .bg(t.bg_mid)
            .border_b_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(px(t.ui_font_size))
            .text_color(t.text)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(badge)
                    .child(msg),
            )
            // The release notes for what's being offered (issue #71) — same window as the
            // Kyde ▸ What's New… menu item.
            .child(
                div()
                    .id("update-whats-new")
                    .flex_none()
                    .cursor_pointer()
                    .text_color(t.primary)
                    .hover(|s| s.text_color(t.text))
                    .child("What's new")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.open_changelog(cx)),
                    ),
            )
            .child(
                btn_primary_state("update-now", action_label, updating).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.do_update(cx)),
                ),
            )
            .child(btn_secondary("update-dismiss", "Dismiss").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.dismiss_update(cx)),
            ))
            .into_any_element()
    }

    /// Act on the update banner. Running from a `.app` bundle with a zip asset → download,
    /// swap in place, relaunch. Otherwise (dev binary, or a release with no zip) → open the
    /// release page in the browser.
    pub(crate) fn do_update(&mut self, cx: &mut Context<Self>) {
        let Some(rel) = self.self_update.available.clone() else {
            return;
        };
        match update::running_bundle() {
            Some(bundle) if !rel.zip_url.is_empty() => {
                if self.self_update.updating {
                    return;
                }
                self.self_update.updating = true;
                cx.notify();
                let zip = rel.zip_url.clone();
                let sha = rel.sha256_url.clone();
                cx.spawn(async move |this, cx| {
                    let res = cx
                        .background_executor()
                        .spawn({
                            let bundle = bundle.clone();
                            async move { update::download_and_swap(&zip, &sha, &bundle) }
                        })
                        .await;
                    this.update(cx, |this, cx| {
                        this.self_update.updating = false;
                        match res {
                            // Relaunch the freshly-swapped bundle, then quit this instance.
                            Ok(()) => {
                                let _ = std::process::Command::new("open").arg(&bundle).spawn();
                                cx.quit();
                            }
                            Err(e) => {
                                this.op_error = Some(format!("Update failed: {e}"));
                                cx.notify();
                            }
                        }
                    })
                    .ok();
                })
                .detach();
            }
            _ => {
                // No bundle to swap (dev binary). Download the zip to ~/Downloads and reveal
                // it in Finder; only fall back to the release page if there's no zip asset
                // (always the case off macOS, see `update::check`), in the system browser.
                if rel.zip_url.is_empty() {
                    let url = if rel.page_url.is_empty() {
                        "https://github.com/kyle-ssg/kyde/releases/latest".to_string()
                    } else {
                        rel.page_url.clone()
                    };
                    cx.open_url(&url);
                    return;
                }
                if self.self_update.updating {
                    return;
                }
                self.self_update.updating = true;
                cx.notify();
                let zip = rel.zip_url.clone();
                cx.spawn(async move |this, cx| {
                    let dir = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                        .join("Downloads");
                    let res = cx
                        .background_executor()
                        .spawn(async move { update::download_zip(&zip, &dir) })
                        .await;
                    this.update(cx, |this, cx| {
                        this.self_update.updating = false;
                        match res {
                            // Reveal the downloaded zip so the user can install it.
                            Ok(path) => {
                                let _ = std::process::Command::new("open")
                                    .arg("-R")
                                    .arg(&path)
                                    .spawn();
                            }
                            Err(e) => this.op_error = Some(format!("Download failed: {e}")),
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// Dismiss the update banner for this session (reappears on next launch if still behind,
    /// or on a refocus recheck once a newer release than the dismissed one ships).
    pub(crate) fn dismiss_update(&mut self, cx: &mut Context<Self>) {
        if let Some(rel) = self.self_update.available.take() {
            self.self_update.dismissed = Some(rel.version);
        }
        cx.notify();
    }

    /// Open a pre-filled GitHub issue for the previous crash, then dismiss the banner.
    fn report_crash(&mut self, cx: &mut Context<Self>) {
        if let Some(crash) = self.pending_crash.clone() {
            cx.open_url(&crash_issue_url(&crash));
        }
        self.dismiss_crash(cx);
    }

    /// Clear the crash banner + truncate the log so it doesn't reappear.
    fn dismiss_crash(&mut self, cx: &mut Context<Self>) {
        self.pending_crash = None;
        if let Some(p) = crash_log_path() {
            let _ = std::fs::write(p, "");
        }
        cx.notify();
    }

    /// Thin top banner shown after a crash, with Report-on-GitHub + Dismiss.
    pub(crate) fn render_crash_banner(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        alert_banner(ui)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child("kyde crashed on the previous run."),
            )
            .child(
                btn_primary("crash-report", "Report on GitHub").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.report_crash(cx)),
                ),
            )
            .child(btn_secondary("crash-dismiss", "Dismiss").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.dismiss_crash(cx)),
            ))
            .into_any_element()
    }

    /// Dismiss the git-operation error banner.
    fn dismiss_op_error(&mut self, cx: &mut Context<Self>) {
        self.op_error = None;
        cx.notify();
    }

    /// Thin banner shown when a git operation failed, with a Dismiss button. Mirrors the
    /// crash banner (same surface + placement); shown only while `op_error` is set.
    pub(crate) fn render_op_error_banner(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let msg = self.op_error.clone().unwrap_or_default();
        alert_banner(ui)
            .child(div().flex_1().min_w_0().truncate().child(msg))
            .child(btn_secondary("op-error-dismiss", "Dismiss").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.dismiss_op_error(cx)),
            ))
            .into_any_element()
    }
}

/// The thin "something went wrong" strip the crash + git-error banners share: one line
/// on the theme's conflict tint (readable in every palette, light included), message
/// first, buttons trailing.
fn alert_banner(ui: &'static str) -> gpui::Div {
    let t = theme::get();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .h(px(34.0))
        .px_3()
        .bg(t.diff_conflict_bg)
        .border_b_1()
        .border_color(t.divider)
        .font_family(ui)
        .text_size(px(t.ui_font_size))
        .text_color(t.text)
}
