//! Local History (issue #7) — IntelliJ-style per-file snapshots, independent of git.
//!
//! Recording (all disk work OFF the UI thread, everything gated on the master switch):
//! - **Change**: every save funnels through [`Kyde::lh_note_save`], which marks the path
//!   pending and flushes ONE snapshot per file per throttle window (default 10s). The
//!   flush reads the file's final on-disk state, so a burst's last save is never lost —
//!   throttling delays the write, it never drops it. Unchanged content dedupes to zero
//!   bytes (content addressing in `kyde-local-history`).
//! - **Baseline / External**: opening a file records its pristine content on first sight,
//!   and an "External change" event when the disk differs from the last snapshot (the
//!   file was edited outside Kyde).
//! - **Labels**: destructive operations snapshot their targets FIRST — "Before rollback",
//!   "Before checkout X", "Before delete", "Before hunk revert", "Before compare apply",
//!   "Before revert" — and a commit stamps "Commit: <subject>" on its files.
//!
//! The window (`ModalKind::LocalHistory`) is the compare-view pattern: a snapshot
//! timeline on the left, snapshot ↔ current side-by-side panes on the right, a center
//! gutter whose `»` restores one hunk of the snapshot into the file, and a header
//! Revert button that restores the whole snapshot.

use crate::*;
use kyde_local_history::{format_ts, relative_ts, EventKind};

/// Center gutter width — matches the compare/merge gutters.
const LH_GUTTER_W: f32 = 56.0;

/// Milliseconds since the Unix epoch, best effort (pre-1970 clocks read as 0).
pub(crate) fn lh_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The local timezone's offset from UTC in minutes, read once per launch (`date +%z`
/// — no chrono/libc dependency; a failure falls back to UTC).
fn tz_offset_min() -> i32 {
    static TZ: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    *TZ.get_or_init(|| {
        let out = std::process::Command::new("date").arg("+%z").output().ok();
        let s = out
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        // "+0100" / "-0930" → minutes east of UTC.
        let (sign, digits) = match s.split_at_checked(1) {
            Some(("+", d)) => (1, d),
            Some(("-", d)) => (-1, d),
            _ => return 0,
        };
        let (Ok(h), Ok(m)) = (
            digits.get(0..2).unwrap_or("").parse::<i32>(),
            digits.get(2..4).unwrap_or("").parse::<i32>(),
        ) else {
            return 0;
        };
        sign * (h * 60 + m)
    })
}

impl Kyde {
    /// Resolve `rel` to its absolute path (scratch files are already absolute).
    fn lh_abs(&self, rel: &std::path::Path) -> Option<PathBuf> {
        if rel.is_absolute() {
            return Some(rel.to_path_buf());
        }
        self.repo_root.as_ref().map(|r| r.join(rel))
    }

    /// Keep the store in sync with the open project + the master switch: open (and
    /// prune) the project's store in the background when the project changes, drop it
    /// when local history is disabled or no project is open. Cheap when nothing
    /// changed — called from `refresh`.
    pub(crate) fn lh_sync_store(&mut self, cx: &mut Context<Self>) {
        if !self.lh.cfg.enabled || self.repo_root.is_none() {
            self.lh.store = None;
            self.lh.store_root = None;
            self.lh.pending.clear();
            return;
        }
        let root = self.repo_root.clone();
        if self.lh.store_root == root {
            return; // already the right store
        }
        self.lh.store = None; // never record into the previous project's store
        self.lh.store_root.clone_from(&root);
        let Some(root) = root else { return };
        let retention = self.lh.cfg.retention_ms();
        cx.spawn(async move |this, cx| {
            let opened = cx
                .background_executor()
                .spawn(async move {
                    let mut store = kyde_local_history::Store::for_project(&root).ok()?;
                    // Retention housekeeping once per project-open, never on a hot path.
                    let _ = store.prune(lh_now_ms(), retention);
                    Some((root, std::sync::Arc::new(std::sync::Mutex::new(store))))
                })
                .await;
            if let Some((root, store)) = opened {
                this.update(cx, |this, _| {
                    // Still the active project? (The user may have switched again.)
                    if this.lh.store_root.as_ref() == Some(&root) {
                        this.lh.store = Some(store);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// A save landed for `rel`: mark it pending and arm the throttle flush. At most one
    /// snapshot per file per throttle window; the flush reads the file's FINAL on-disk
    /// state, so the last save of a burst is always the one recorded.
    pub(crate) fn lh_note_save(&mut self, rel: &std::path::Path, cx: &mut Context<Self>) {
        if !self.lh.cfg.enabled || self.lh.store.is_none() {
            return;
        }
        self.lh.pending.insert(rel.to_path_buf());
        if self.lh.flush_scheduled {
            return; // an armed timer will pick this path up too
        }
        self.lh.flush_scheduled = true;
        let wait = std::time::Duration::from_millis(self.lh.cfg.throttle_ms());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| {
                this.lh.flush_scheduled = false;
                this.lh_flush(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Snapshot every pending path's current on-disk content as a `Change` event.
    pub(crate) fn lh_flush(&mut self, cx: &mut Context<Self>) {
        if self.lh.pending.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = self.lh.pending.drain().collect();
        let Some(store) = self.lh.store.clone() else {
            return;
        };
        let abs: Vec<(PathBuf, PathBuf)> = paths
            .into_iter()
            .filter_map(|rel| self.lh_abs(&rel).map(|a| (rel, a)))
            .collect();
        cx.background_executor()
            .spawn(async move {
                let now = lh_now_ms();
                for (rel, abs) in abs {
                    // Unreadable (deleted mid-burst) or non-UTF-8 (binary) files are skipped.
                    if let Ok(content) = std::fs::read_to_string(&abs) {
                        if let Ok(mut s) = store.lock() {
                            let _ = s.record(&rel, &content, EventKind::Change, None, now);
                        }
                    }
                }
            })
            .detach();
    }

    /// A file was opened with `content` fresh from disk: record the baseline on first
    /// sight (so the pristine version is always recoverable), or an "External change"
    /// when the disk no longer matches the last snapshot (edited outside Kyde).
    pub(crate) fn lh_note_open(
        &mut self,
        rel: &std::path::Path,
        content: &str,
        cx: &mut Context<Self>,
    ) {
        if !self.lh.cfg.enabled || self.lh.store.is_none() {
            return;
        }
        let Some(store) = self.lh.store.clone() else {
            return;
        };
        let (rel, content) = (rel.to_path_buf(), content.to_string());
        cx.background_executor()
            .spawn(async move {
                let Ok(mut s) = store.lock() else { return };
                let kind = match s.last_hash(&rel) {
                    None => EventKind::Change, // baseline — the file's first appearance
                    Some(h) if h != kyde_local_history::content_hash(&content) => {
                        EventKind::External
                    }
                    Some(_) => return, // disk matches the last snapshot — nothing new
                };
                let _ = s.record(&rel, &content, kind, None, lh_now_ms());
            })
            .detach();
    }

    /// Immediately snapshot `paths`' CURRENT on-disk content under `label` — called by
    /// destructive operations *before* they touch the files. The reads happen inline
    /// (the caller is about to overwrite the content; a deferred read would capture the
    /// wrong state), the store writes happen in the background.
    pub(crate) fn lh_snapshot_now(
        &mut self,
        paths: Vec<PathBuf>,
        label: &str,
        cx: &mut Context<Self>,
    ) {
        if !self.lh.cfg.enabled || self.lh.store.is_none() || paths.is_empty() {
            return;
        }
        let Some(store) = self.lh.store.clone() else {
            return;
        };
        let contents: Vec<(PathBuf, String)> = paths
            .into_iter()
            .filter_map(|rel| {
                let abs = self.lh_abs(&rel)?;
                std::fs::read_to_string(abs).ok().map(|c| (rel, c))
            })
            .collect();
        if contents.is_empty() {
            return;
        }
        let label = label.to_string();
        cx.background_executor()
            .spawn(async move {
                let now = lh_now_ms();
                let Ok(mut s) = store.lock() else { return };
                for (rel, content) in contents {
                    let _ = s.record(&rel, &content, EventKind::Label, Some(label.clone()), now);
                }
            })
            .detach();
    }

    // ── the Local History window ──────────────────────────────────

    /// Open the Local History window for `rel`.
    pub(crate) fn open_local_history(&mut self, rel: PathBuf, cx: &mut Context<Self>) {
        self.context_menu = None;
        if self.lh.store.is_none() {
            return; // disabled (the menu item is hidden then, but guard anyway)
        }
        // Capture any pending edits first so "Current vs latest" starts in sync.
        self.lh_flush(cx);
        let name = rel
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.lh.path = Some(rel);
        self.lh.selected = 0;
        self.lh_reload(cx);
        self.open_modal_window(
            ModalKind::LocalHistory,
            format!("Local History: {name}"),
            1200.0,
            760.0,
            cx,
        );
    }

    /// (Re)read the timeline for the open path and load the selected snapshot's diff.
    pub(crate) fn lh_reload(&mut self, cx: &mut Context<Self>) {
        let (Some(path), Some(store)) = (self.lh.path.clone(), self.lh.store.clone()) else {
            return;
        };
        self.lh.events = store
            .lock()
            .map(|s| s.events_for(&path))
            .unwrap_or_default();
        self.lh.selected = self.lh.selected.min(self.lh.events.len().saturating_sub(1));
        self.lh_load_diff(true, cx);
    }

    /// Select timeline row `i` and re-diff.
    pub(crate) fn lh_select(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.lh.events.len() {
            self.lh.selected = i;
            self.lh_load_diff(true, cx);
        }
    }

    /// The selected snapshot's text, read back from the blob store.
    fn lh_selected_content(&self) -> Option<String> {
        let ev = self.lh.events.get(self.lh.selected)?;
        let store = self.lh.store.as_ref()?;
        store.lock().ok()?.content(&ev.hash).ok()
    }

    /// Load snapshot (left) vs current file (right) into the aligned panes — the
    /// compare-view decoration pipeline. `park` scrolls to just above the first hunk.
    fn lh_load_diff(&mut self, park: bool, cx: &mut Context<Self>) {
        let Some(path) = self.lh.path.clone() else {
            return;
        };
        let Some(snapshot) = self.lh_selected_content() else {
            self.lh.diff = None;
            cx.notify();
            return;
        };
        let current = self
            .lh_abs(&path)
            .and_then(|a| std::fs::read_to_string(a).ok())
            .unwrap_or_default();
        let d = FileDiff::compute(&snapshot, &current);
        let first = d.hunks.first().map(|h| h.old_range.start);
        let (lbg, rbg) = diff_line_bgs(&d);
        let (lw, rw) = diff_word_bgs(&d);
        let (lf, lf_end, rf, rf_end) = diff_fillers(&d);
        let lang = self.effective_lang(&path);
        let t = theme::get();
        self.lh.left.update(cx, |e, cx| {
            e.gutter_right = true;
            e.line_bg = lbg;
            e.word_bg = lw;
            e.word_bg_color = t.diff_word_old_bg;
            e.filler = lf;
            e.filler_end = lf_end;
            e.set_content(snapshot, lang, cx);
        });
        self.lh.right.update(cx, |e, cx| {
            e.line_bg = rbg;
            e.word_bg = rw;
            e.word_bg_color = t.diff_word_new_bg;
            e.filler = rf;
            e.filler_end = rf_end;
            e.set_content(current, lang, cx);
        });
        let sh = self.lh.scroll.clone();
        self.lh
            .left
            .update(cx, |e, _| e.set_scroll_handle(sh.clone()));
        self.lh.right.update(cx, |e, _| e.set_scroll_handle(sh));
        if park {
            let row = first.unwrap_or(0).saturating_sub(SCROLL_CONTEXT_ROWS) as f32;
            self.lh
                .scroll
                .set_offset(gpui::point(px(0.0), px(-row * editor::line_height_px())));
        }
        self.lh.diff = Some(d);
        cx.notify();
    }

    /// `»` on hunk `hi`: restore that hunk of the snapshot into the current file
    /// (all OTHER differences stay). Snapshot-first, so the pre-restore state is
    /// itself in the timeline.
    pub(crate) fn lh_apply_hunk(&mut self, hi: usize, cx: &mut Context<Self>) {
        let Some(d) = self.lh.diff.as_ref() else {
            return;
        };
        // diff = (snapshot → current); keeping every hunk EXCEPT `hi` un-reverts
        // nothing else and hands hunk `hi` back its snapshot lines.
        let text = d.partial_new_content(|j| j != hi);
        self.lh_write_current("Before hunk restore", &text, cx);
    }

    /// Header Revert: the file becomes exactly the selected snapshot.
    pub(crate) fn lh_revert_to_selected(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.lh_selected_content() else {
            return;
        };
        self.lh_write_current("Before revert", &snapshot, cx);
    }

    /// Persist `text` as the current file: snapshot the pre-write state under `label`,
    /// write, reload a clean open editor showing the file, re-diff, refresh git status.
    fn lh_write_current(&mut self, label: &str, text: &str, cx: &mut Context<Self>) {
        let Some(path) = self.lh.path.clone() else {
            return;
        };
        self.lh_snapshot_now(vec![path.clone()], label, cx);
        if let Err(e) = self.write_open_file(&path, text) {
            self.fail("Local history restore", e);
            cx.notify();
            return;
        }
        // The restored file may be open in Browse — reload it unless it has unsaved
        // edits (the never-clobber rule).
        if self.browse.open_path.as_ref() == Some(&path) && !self.browse.editor.read(cx).dirty {
            let lang = self.effective_lang(&path);
            let content = text.to_string();
            self.browse
                .editor
                .update(cx, |e, cx| e.set_content(content, lang, cx));
        }
        // The write itself is a change worth keeping (deduped if identical).
        self.lh_note_save(&path, cx);
        self.lh_reload(cx);
        self.refresh(cx);
    }

    /// The Local History window body: timeline | snapshot ↔ current panes + gutter.
    pub(crate) fn render_local_history_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let ui = theme::font::UI_FAMILY;
        let fs = px(t.editor_font_size);
        let ui_fs = px(t.ui_font_size);
        let row_h = px(editor::line_height_px());
        let now = lh_now_ms();
        let tz = tz_offset_min();

        // ── left: the snapshot timeline ──
        let timeline_rows: Vec<gpui::AnyElement> = self
            .lh
            .events
            .iter()
            .enumerate()
            .map(|(i, ev)| {
                let sel = i == self.lh.selected;
                let title: SharedString = match (&ev.kind, &ev.label) {
                    (EventKind::Label, Some(l)) => l.clone().into(),
                    (EventKind::External, _) => "External change".into(),
                    _ => "Change".into(),
                };
                let when: SharedString = format!(
                    "{} · {}",
                    format_ts(ev.ts_ms, tz),
                    relative_ts(now, ev.ts_ms)
                )
                .into();
                ui::picker::row(("lh-row", i), sel, t.bg_light)
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .mx(px(4.0))
                    .px_3()
                    .py_1p5()
                    .child(div().text_color(t.text).child(title))
                    .child(
                        div()
                            .text_size(px(t.ui_font_size - 1.0))
                            .text_color(t.line_number)
                            .child(when),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.lh_select(i, cx)),
                    )
                    .into_any_element()
            })
            .collect();
        let timeline = div()
            .id("lh-timeline")
            .w(px(260.0))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .py_2()
            .bg(t.panel_bg)
            .border_r(px(1.0))
            .border_color(t.divider)
            .font_family(ui)
            .text_size(ui_fs)
            .children(timeline_rows);

        // ── right: snapshot ↔ current, the compare-view pane pattern ──
        let Some(d) = self.lh.diff.as_ref() else {
            let empty = div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .font_family(ui)
                .text_size(ui_fs)
                .text_color(t.line_number)
                .child("No local history for this file yet — edit it and history will appear.");
            return div()
                .size_full()
                .flex()
                .flex_row()
                .child(timeline)
                .child(empty)
                .into_any_element();
        };

        let rows = aligned_rows(d);
        let total_h = row_h * rows.len() as f32;
        let hunk_rows: Vec<(usize, usize)> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.hunk_start)
            .filter_map(|(row, r)| r.hunk.map(|h| (row, h)))
            .collect();
        let mut ctls: Vec<gpui::AnyElement> = Vec::new();
        for &(row, hi) in &hunk_rows {
            let tip = SharedString::from("Restore this change from the snapshot");
            ctls.push(
                div()
                    .absolute()
                    .top(row_h * row as f32)
                    .left_0()
                    .right_0()
                    .h(row_h)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id(SharedString::from(format!("lh-apply-{hi}")))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(22.0))
                            .rounded_sm()
                            .text_size(px(17.0))
                            .line_height(px(17.0))
                            .text_color(t.line_number)
                            .hover(|s| s.bg(t.bg_light).text_color(t.primary))
                            .cursor_pointer()
                            .tooltip(move |_w, cx| cx.new(|_| Tip(tip.clone())).into())
                            .child("»")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    cx.stop_propagation();
                                    this.lh_apply_hunk(hi, cx);
                                }),
                            ),
                    )
                    .into_any_element(),
            );
        }
        let scroll_y = self.lh.scroll.offset().y;
        let gutter = div()
            .id("lh-gutter")
            .w(px(LH_GUTTER_W))
            .flex_none()
            .h_full()
            .overflow_hidden()
            .bg(t.diff_separator_bg)
            .border_l(px(1.0))
            .border_r(px(1.0))
            .border_color(t.divider)
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(total_h)
                    .top(scroll_y)
                    .children(ctls),
            );

        let lw = self.lh.left.read(cx).content_width();
        let rw = self.lh.right.read(cx).content_width();
        let pane = |id: &'static str, w: f32, ed: gpui::AnyElement, scroll: &ScrollHandle| {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .bg(t.main_bg)
                .font_family(theme::font::FAMILY)
                .text_size(fs)
                .text_color(t.text)
                .child(
                    div()
                        .id(id)
                        .h_full()
                        .w_full()
                        .overflow_scroll()
                        .track_scroll(scroll)
                        .child(div().w(px(w)).child(ed)),
                )
        };
        let scroll = self.lh.scroll.clone();

        let n = d.hunks.len();
        let count = if n == 0 {
            "No differences".to_string()
        } else {
            format!("{n} difference{}", if n == 1 { "" } else { "s" })
        };
        let snap_label: SharedString = self
            .lh
            .events
            .get(self.lh.selected)
            .map(|ev| format!("Snapshot · {}", format_ts(ev.ts_ms, tz)))
            .unwrap_or_default()
            .into();
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .font_family(ui)
            .text_size(ui_fs)
            .border_b(px(1.0))
            .border_color(t.divider)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(t.text)
                    .child(snap_label),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(t.secondary_text)
                    .child(SharedString::from(count)),
            )
            .child(
                btn_secondary("lh-revert", "Revert to This Version")
                    .py_1()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.lh_revert_to_selected(cx)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_right()
                    .text_color(t.text)
                    .child("Current"),
            );

        let panes = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(pane(
                        "lh-left",
                        lw,
                        self.lh.left.clone().into_any_element(),
                        &scroll,
                    ))
                    .child(gutter)
                    .child(pane(
                        "lh-right",
                        rw,
                        self.lh.right.clone().into_any_element(),
                        &scroll,
                    )),
            );

        div()
            .size_full()
            .flex()
            .flex_row()
            .child(timeline)
            .child(panes)
            .into_any_element()
    }
}
