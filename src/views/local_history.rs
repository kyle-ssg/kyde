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

    /// Open the Local History window for `rel` — a file, a folder (its timeline is
    /// every file's events under it), or the empty path (whole project).
    pub(crate) fn open_local_history(&mut self, rel: PathBuf, cx: &mut Context<Self>) {
        self.context_menu = None;
        if self.lh.store.is_none() {
            return; // disabled (the menu item is hidden then, but guard anyway)
        }
        // Capture any pending edits first so "Current vs latest" starts in sync.
        self.lh_flush(cx);
        let name = rel.file_name().map_or_else(
            || "Project".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        self.lh.path = Some(rel);
        self.lh.selected = 0;
        self.lh.file_selected = None;
        self.lh_reload(cx);
        self.open_modal_window(
            ModalKind::LocalHistory,
            format!("Local History: {name}"),
            1200.0,
            760.0,
            cx,
        );
    }

    /// (Re)read the timeline for the open scope and load the selected snapshot's diff.
    /// `events_under` scopes component-wise, so a file shows its own events and a
    /// folder shows every file's under it.
    pub(crate) fn lh_reload(&mut self, cx: &mut Context<Self>) {
        let (Some(path), Some(store)) = (self.lh.path.clone(), self.lh.store.clone()) else {
            return;
        };
        self.lh.events = store
            .lock()
            .map(|s| s.events_under(&path))
            .unwrap_or_default();
        self.lh.selected = self.lh.selected.min(self.lh.events.len().saturating_sub(1));
        self.lh_recompute_files();
        self.lh_load_diff(true, cx);
    }

    /// Select timeline row `i` and re-diff (the changed-files panel + its selection
    /// follow the row's file).
    pub(crate) fn lh_select(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.lh.events.len() {
            self.lh.selected = i;
            self.lh.file_selected = None; // back to the row's own file
            self.lh_recompute_files();
            self.lh_load_diff(true, cx);
        }
    }

    /// Select file `p` in the changed-files panel — the diff panes (and any restore)
    /// now target it.
    pub(crate) fn lh_select_file(&mut self, p: PathBuf, cx: &mut Context<Self>) {
        self.lh.file_selected = Some(p);
        self.lh_load_diff(true, cx);
    }

    /// Toggle a folder row in the changed-files panel.
    pub(crate) fn lh_toggle_dir(&mut self, p: &std::path::Path, cx: &mut Context<Self>) {
        if !self.lh.files_expanded.remove(p) {
            self.lh.files_expanded.insert(p.to_path_buf());
        }
        cx.notify();
    }

    /// Rebuild the "changed since this snapshot" panel: the distinct files of
    /// `events[0..=selected]` (this change + everything newer) that still DIFFER
    /// from their state at the snapshot, as a fully-expanded tree. A file that was
    /// touched but changed BACK (e.g. deleted then restored) is dropped — listing a
    /// "No differences" file reads as a bug. The panel selection survives if its
    /// file is still listed. Runs on selection change only (one content-hash read
    /// per candidate file — never on the render path).
    fn lh_recompute_files(&mut self) {
        let upto = self.lh.selected.min(self.lh.events.len().saturating_sub(1));
        let mut seen = std::collections::HashSet::new();
        let mut files: Vec<PathBuf> = self
            .lh
            .events
            .get(..=upto)
            .unwrap_or_default()
            .iter()
            .filter(|e| seen.insert(e.path.clone()))
            .map(|e| e.path.clone())
            .collect();
        // One rule: keep a file iff its content at the snapshot differs from its
        // content now, treating "no snapshot" / "no file" as empty — so a file that
        // changed back, or whose empty snapshot was deleted, never shows as a
        // "No differences" row.
        let empty_hash = kyde_local_history::content_hash("");
        files.retain(|f| {
            let base_hash = self
                .lh_base_event_for(f)
                .map_or_else(|| empty_hash.clone(), |e| e.hash.clone());
            let current_hash = self
                .lh_abs(f)
                .and_then(|a| std::fs::read_to_string(a).ok())
                .map_or_else(
                    || empty_hash.clone(),
                    |c| kyde_local_history::content_hash(&c),
                );
            base_hash != current_hash
        });
        files.sort();
        self.lh.files_tree = tree::Tree::build(&files);
        // Default fully expanded (the WebStorm presentation) — every ancestor dir.
        self.lh.files_expanded = files
            .iter()
            .flat_map(|f| f.ancestors().skip(1))
            .filter(|a| !a.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf)
            .collect();
        if let Some(sel) = &self.lh.file_selected {
            if !files.contains(sel) {
                self.lh.file_selected = None;
            }
        }
        self.lh.files = files;
    }

    /// The file the panes + restores target: the changed-files panel selection, else
    /// the selected timeline row's own file.
    fn lh_selected_path(&self) -> Option<PathBuf> {
        self.lh
            .file_selected
            .clone()
            .or_else(|| self.lh.events.get(self.lh.selected).map(|e| e.path.clone()))
    }

    /// `path`'s snapshot AT the selected point in time — its newest event at or before
    /// the selected row (`None` = first seen after it; its earlier state is unknown).
    fn lh_base_event_for(&self, path: &std::path::Path) -> Option<&kyde_local_history::Event> {
        self.lh
            .events
            .get(self.lh.selected..)
            .unwrap_or_default()
            .iter()
            .find(|e| e.path == path)
    }

    /// The base snapshot's text for the currently-targeted file.
    fn lh_selected_content(&self) -> Option<String> {
        let ev = self.lh_base_event_for(&self.lh_selected_path()?)?;
        let store = self.lh.store.as_ref()?;
        store.lock().ok()?.content(&ev.hash).ok()
    }

    /// Whether anything under `p` (a file or folder from the changed-files panel) has a
    /// snapshot at the selected point — i.e. whether a revert has something to restore.
    pub(crate) fn lh_has_base_under(&self, p: &std::path::Path) -> bool {
        self.lh
            .files
            .iter()
            .filter(|f| f.starts_with(p))
            .any(|f| self.lh_base_event_for(f).is_some())
    }

    /// Load snapshot (left) vs current file (right) into the aligned panes — the
    /// compare-view decoration pipeline. The file is the changed-files panel's
    /// selection (else the selected event's own); its snapshot side is the file's
    /// state AT the selected point in time. `park` scrolls to just above the first
    /// hunk.
    fn lh_load_diff(&mut self, park: bool, cx: &mut Context<Self>) {
        let Some(path) = self.lh_selected_path() else {
            self.lh.diff = None;
            cx.notify();
            return;
        };
        // No snapshot at this point (the file was first seen after it) → diff against
        // empty, so the whole file reads as added-since.
        let snapshot = self.lh_selected_content().unwrap_or_default();
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

    /// Header Revert: the targeted file becomes exactly its snapshot at the selected
    /// point.
    pub(crate) fn lh_revert_to_selected(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.lh_selected_path() else {
            return;
        };
        self.lh_revert_files(vec![path], cx);
    }

    /// Right-click a changed-files row: revert JUST that file (or that folder's files)
    /// to its state at the selected snapshot — never the whole selection.
    pub(crate) fn lh_revert_path(&mut self, p: PathBuf, cx: &mut Context<Self>) {
        let targets: Vec<PathBuf> = self
            .lh
            .files
            .iter()
            .filter(|f| f.starts_with(&p))
            .cloned()
            .collect();
        self.lh_revert_files(targets, cx);
    }

    /// Right-click a timeline row: undo this change and everything newer — every file
    /// in the changed-since panel returns to its state at the selected snapshot.
    pub(crate) fn lh_revert_since(&mut self, cx: &mut Context<Self>) {
        let targets = self.lh.files.clone();
        self.lh_revert_files(targets, cx);
    }

    /// Restore each of `paths` to its snapshot at the selected point. Files first seen
    /// AFTER the snapshot are skipped (their earlier state is unknown — deleting them
    /// would be guessing). Reverting is itself history: every pre-revert state is
    /// labeled "Before revert" first, so the timeline gains a marker and the undone
    /// state stays recoverable.
    fn lh_revert_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let restores: Vec<(PathBuf, String)> = paths
            .into_iter()
            .filter_map(|p| {
                let ev = self.lh_base_event_for(&p)?;
                let store = self.lh.store.as_ref()?;
                let text = store.lock().ok()?.content(&ev.hash).ok()?;
                Some((p, text))
            })
            .collect();
        if restores.is_empty() {
            return;
        }
        let targets: Vec<PathBuf> = restores.iter().map(|(p, _)| p.clone()).collect();
        self.lh_label_sync(&targets, "Before revert");
        for (path, text) in restores {
            if let Err(e) = self.write_open_file(&path, &text) {
                self.fail("Local history revert", e);
                continue;
            }
            // The restored file may be open in Browse — reload it unless it has
            // unsaved edits (the never-clobber rule).
            if self.browse.open_path.as_ref() == Some(&path) && !self.browse.editor.read(cx).dirty {
                let lang = self.effective_lang(&path);
                self.browse
                    .editor
                    .update(cx, |e, cx| e.set_content(text, lang, cx));
            }
            // The write itself is a change worth keeping (deduped if identical).
            self.lh_note_save(&path, cx);
        }
        self.lh_reload(cx);
        self.refresh(cx);
    }

    /// Record `paths`' current on-disk content as `label` events INLINE (on the UI
    /// thread) — the window's own restores use this instead of the background
    /// [`Kyde::lh_snapshot_now`] so the immediately-following `lh_reload` is
    /// guaranteed to show the new marker. Small, explicit user actions only.
    fn lh_label_sync(&mut self, paths: &[PathBuf], label: &str) {
        if !self.lh.cfg.enabled {
            return;
        }
        let Some(store) = self.lh.store.clone() else {
            return;
        };
        let now = lh_now_ms();
        let Ok(mut s) = store.lock() else { return };
        for rel in paths {
            let Some(abs) = self.lh_abs(rel) else {
                continue;
            };
            if let Ok(content) = std::fs::read_to_string(abs) {
                let _ = s.record(
                    rel,
                    &content,
                    EventKind::Label,
                    Some(label.to_string()),
                    now,
                );
            }
        }
    }

    /// Persist `text` as the targeted file: label the pre-write state, write, reload a
    /// clean open editor showing the file, re-diff, refresh git status.
    fn lh_write_current(&mut self, label: &str, text: &str, cx: &mut Context<Self>) {
        let Some(path) = self.lh_selected_path() else {
            return;
        };
        self.lh_label_sync(std::slice::from_ref(&path), label);
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

    // ── Clear Local History (confirmation window) ─────────────────

    /// Open the "Clear Local History" confirmation — a native modal window (the
    /// Clear Data pattern). No-op when local history is off / no project.
    pub(crate) fn open_clear_local_history(&mut self, cx: &mut Context<Self>) {
        if self.lh.store.is_none() {
            return;
        }
        self.open_modal_window(
            ModalKind::ClearLocalHistory,
            "Clear Local History",
            460.0,
            200.0,
            cx,
        );
    }

    /// Wipe the open project's snapshot store (journal + blobs) and empty any open
    /// Local History window. Destructive — only reachable through the confirmation.
    pub(crate) fn do_clear_local_history(&mut self, cx: &mut Context<Self>) {
        if let Some(store) = self.lh.store.clone() {
            match store.lock() {
                Ok(mut s) => {
                    if let Err(e) = s.clear() {
                        self.fail("Clear local history", e);
                    }
                }
                Err(_) => self.fail(
                    "Clear local history",
                    anyhow::anyhow!("store lock poisoned"),
                ),
            }
        }
        self.lh.pending.clear();
        self.lh.events.clear();
        self.lh.files.clear();
        self.lh.files_tree = tree::Tree::default();
        self.lh.files_expanded.clear();
        self.lh.file_selected = None;
        self.lh.selected = 0;
        self.lh.diff = None;
        self.close_modal_window(ModalKind::ClearLocalHistory, cx);
        cx.notify();
    }

    /// Body of the "Clear Local History" confirmation window (Enter confirms,
    /// Escape cancels — the shared `ModalWindow` key handling).
    pub(crate) fn render_clear_local_history_body(
        &mut self,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        let cancel = btn_secondary("clear-lh-cancel", "Cancel").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| {
                this.close_modal_window(ModalKind::ClearLocalHistory, cx);
            }),
        );
        let confirm = btn_primary("clear-lh-confirm", "Clear History").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| this.do_clear_local_history(cx)),
        );
        let project = self
            .repo_root
            .as_ref()
            .and_then(|r| r.file_name())
            .map_or_else(
                || "this project".to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size + 1.0))
            .child(div().text_color(t.text).child(SharedString::from(format!(
                "Clear local history for “{project}”?"
            ))))
            .child(
                div()
                    .flex_1()
                    .text_color(t.secondary_text)
                    .text_size(px(theme::get().ui_font_size))
                    .child(
                        "Deletes every snapshot and timeline event Kyde has recorded for \
                         this project. Git history is not touched. Can't be undone.",
                    ),
            )
            .child(ui::modal_footer().child(cancel).child(confirm))
            .into_any_element()
    }

    /// The Local History window's context-menu items — `render_context_menu`'s match
    /// delegates `LhRow`/`LhPath` here so the feature module owns its menu. `item`
    /// is the shared menu-row builder.
    pub(crate) fn lh_menu_items(
        &self,
        panel: gpui::Div,
        target: &MenuTarget,
        item: &dyn Fn(&'static str) -> gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let t = theme::get();
        match target {
            // Timeline row: undo this change and everything newer — every file in the
            // changed-since panel returns to its state at this snapshot.
            MenuTarget::LhRow => panel.child(item("Revert This Change and After").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.lh_revert_since(cx)),
            )),
            // Changed-files row: revert JUST this file (or this folder's files).
            MenuTarget::LhPath(p, is_dir) => {
                let label = if *is_dir {
                    "Revert This Folder"
                } else {
                    "Revert This File"
                };
                if self.lh_has_base_under(p) {
                    let p = p.clone();
                    panel.child(item(label).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.lh_revert_path(p.clone(), cx)),
                    ))
                } else {
                    // First seen after this snapshot — its earlier state is unknown,
                    // so there is nothing safe to restore.
                    panel.child(
                        div()
                            .px_3()
                            .py_1()
                            .cursor(gpui::CursorStyle::Arrow)
                            .text_color(t.line_number)
                            .child("No snapshot at this point"),
                    )
                }
            }
            _ => panel,
        }
    }

    /// The Local History window body: timeline + changed-files panel | snapshot ↔
    /// current panes + gutter.
    pub(crate) fn render_local_history_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let ui = theme::font::UI_FAMILY;
        let fs = px(t.editor_font_size);
        let ui_fs = px(t.ui_font_size);
        let row_h = px(editor::line_height_px());
        let now = lh_now_ms();
        let tz = tz_offset_min();

        // ── left: the snapshot timeline ──
        // Folder/project scope → rows span several files, so each carries its file name.
        let scoped_to_file = self
            .lh
            .events
            .iter()
            .all(|e| Some(e.path.as_path()) == self.lh.path.as_deref());
        let timeline_rows: Vec<gpui::AnyElement> = self
            .lh
            .events
            .iter()
            .enumerate()
            .map(|(i, ev)| {
                let sel = i == self.lh.selected;
                let mut title_text = match (&ev.kind, &ev.label) {
                    (EventKind::Label, Some(l)) => l.clone(),
                    (EventKind::External, _) => "External change".to_string(),
                    _ => "Change".to_string(),
                };
                if !scoped_to_file {
                    let file = ev
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    title_text = format!("{file} — {title_text}");
                }
                let title: SharedString = title_text.into();
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
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                            this.lh_select(i, cx);
                            this.open_menu(e.position, MenuTarget::LhRow, cx);
                        }),
                    )
                    .into_any_element()
            })
            .collect();
        let timeline = div()
            .id("lh-timeline")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .py_2()
            .children(timeline_rows);

        // ── bottom-left: files changed at or since the selected snapshot ──
        // Left-click a file → its diff; right-click → revert JUST it (folder rows
        // revert their subtree); the timeline row's menu reverts them all.
        let sel_path = self.lh_selected_path();
        let file_rows: Vec<gpui::AnyElement> = self
            .lh
            .files_tree
            .visible(&self.lh.files_expanded)
            .into_iter()
            .map(|r| {
                let is_dir = r.is_dir;
                let expanded = self.lh.files_expanded.contains(&r.path);
                let selected = !is_dir && Some(&r.path) == sel_path.as_ref();
                let name: SharedString = r
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into();
                let (p_act, p_ctx) = (r.path.clone(), r.path.clone());
                ui::tree::item(
                    cx,
                    false,
                    &r.path,
                    is_dir,
                    expanded,
                    r.depth,
                    selected,
                    name,
                    t.text,
                    None,
                    None,
                    move |this, _e, _w, cx| {
                        if is_dir {
                            this.lh_toggle_dir(&p_act, cx);
                        } else {
                            this.lh_select_file(p_act.clone(), cx);
                        }
                    },
                    |_, _| {},
                    move |this, pos, cx| {
                        if !is_dir {
                            this.lh_select_file(p_ctx.clone(), cx);
                        }
                        this.open_menu(pos, MenuTarget::LhPath(p_ctx.clone(), is_dir), cx);
                    },
                )
            })
            .collect();
        let n_changed = self.lh.files.len();
        let files_panel = div()
            .flex_none()
            .h(px(230.0))
            .flex()
            .flex_col()
            .border_t(px(1.0))
            .border_color(t.divider)
            .child(
                div()
                    .px_3()
                    .py_1p5()
                    .text_size(px(t.ui_font_size - 1.0))
                    .text_color(t.line_number)
                    .child(SharedString::from(format!(
                        "Changed since — {n_changed} file{}",
                        if n_changed == 1 { "" } else { "s" }
                    ))),
            )
            .child(
                div()
                    .id("lh-files")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .pb_1()
                    .children(file_rows),
            );

        let left_col = div()
            .w(px(260.0))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(t.panel_bg)
            .border_r(px(1.0))
            .border_color(t.divider)
            .font_family(ui)
            .text_size(ui_fs)
            .child(timeline)
            .child(files_panel);

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
                .child(left_col)
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
        // Under a folder scope, name the file both sides are showing. The snapshot side
        // is the file's state AT the selected point (its base event) — a file first
        // seen after the snapshot has no base, so no timestamp and no Revert button.
        let sel_file = sel_path
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let base = sel_path
            .as_ref()
            .and_then(|p| self.lh_base_event_for(p).cloned());
        let snap_label: SharedString = match &base {
            Some(ev) if scoped_to_file => format!("Snapshot · {}", format_ts(ev.ts_ms, tz)),
            Some(ev) => format!("{sel_file} · Snapshot · {}", format_ts(ev.ts_ms, tz)),
            None if scoped_to_file => "No snapshot at this point".to_string(),
            None => format!("{sel_file} · No snapshot at this point"),
        }
        .into();
        let current_label: SharedString = if scoped_to_file {
            "Current".into()
        } else {
            format!("{sel_file} · Current").into()
        };
        let mut header = div()
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
            );
        if base.is_some() {
            header = header.child(
                btn_secondary("lh-revert", "Revert to This Version")
                    .py_1()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.lh_revert_to_selected(cx)),
                    ),
            );
        }
        let header = header.child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_right()
                .text_color(t.text)
                .child(current_label),
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

        let mut root = div()
            .size_full()
            .flex()
            .flex_row()
            .child(left_col)
            .child(panes);
        // Right-click menus target THIS window's rows — render the menu here, not in
        // the main window (the rollback-modal pattern).
        if matches!(
            self.context_menu.as_ref().map(|m| &m.target),
            Some(MenuTarget::LhRow | MenuTarget::LhPath(..))
        ) {
            root = root.child(self.render_context_menu(cx));
        }
        root.into_any_element()
    }
}
