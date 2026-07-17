//! Merge view: the branch-merge action, the merge-in-progress banner, and the two-stage
//! resolve window (`ModalKind::Merge`, a native OS window like every other modal):
//!
//! 1. **Conflicts list** (`IntelliJ`'s Conflicts dialog): every conflicted file with what
//!    each side did (Modified/Added/Deleted from the index stages), plus per-file
//!    **Accept Yours / Accept Theirs / Merge…** — the whole-file accepts resolve without
//!    opening a diff; Merge… opens stage 2 for that file.
//! 2. **3-pane resolve** (yours | result | theirs): an aligned 3-way diff
//!    (`kyde_diff::merge::Merge3`) whose two gutters carry `»`/`«` (apply that side) and
//!    `×` (ignore) on EVERY changed chunk — nothing applies automatically; the toolbar's
//!    "Apply non-conflicting changes: » Left · »« All · « Right" bulk-applies the clean
//!    ones. The toolbar also offers `IntelliJ`'s **Compare Contents** pairs (Left/Middle/
//!    Right/Base, 2-pane) and a **whitespace mode** (exact / trim / ignore all). Apply
//!    saves + stages the file; once every file is resolved, Commit Merge concludes with
//!    git's prepared `MERGE_MSG`.
//!
//! The banner also picks up merges started OUTSIDE kyde (a conflicted `git pull` in a
//! terminal) via the refresh snapshot's `MERGE_HEAD` probe.

use crate::*;
use kyde_diff::merge::{ChunkKind, Resolution, SideState, WhitespaceMode};

/// Width of each of the two control gutters between the three panes.
const MERGE_GUTTER_W: f32 = 56.0;

impl Kyde {
    // ── actions ───────────────────────────────────────────────────

    /// Branch popup right-click → merge `name` into the current branch, off the UI
    /// thread. Clean → note banner; conflicts → the resolve window opens.
    pub(crate) fn menu_merge_branch(&mut self, name: String, cx: &mut Context<Self>) {
        self.context_menu = None;
        self.branch.popup_open = false;
        if self.merge.busy {
            return;
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        self.merge.busy = true;
        self.merge.source = Some(name.clone());
        self.merge.note = None;
        cx.notify();
        let target = self.current_branch.clone().unwrap_or_else(|| "HEAD".into());
        let branch = name.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { Repo::discover(&root).and_then(|r| r.merge_branch(&branch)) })
                .await;
            this.update(cx, |this, cx| {
                this.merge.busy = false;
                match res {
                    Ok(git::MergeOutcome::UpToDate) => {
                        this.merge.source = None;
                        this.merge.note = Some(format!(
                            "Already up to date — nothing to merge from “{name}”."
                        ));
                    }
                    Ok(git::MergeOutcome::Merged) => {
                        this.merge.source = None;
                        this.merge.note = Some(format!("Merged “{name}” into “{target}”."));
                    }
                    Ok(git::MergeOutcome::Conflicts(paths)) => {
                        // Per-side statuses for the list columns; fall back to plain
                        // modified/modified rows if the stage read races/fails.
                        let entries = this
                            .repo()
                            .map(|r| r.conflict_entries())
                            .filter(|e| !e.is_empty())
                            .unwrap_or_else(|| {
                                paths
                                    .into_iter()
                                    .map(|path| git::ConflictEntry {
                                        path,
                                        ours: git::ConflictSide::Modified,
                                        theirs: git::ConflictSide::Modified,
                                    })
                                    .collect()
                            });
                        this.merge.files = entries;
                        this.merge.resolved.clear();
                        this.open_merge_window(cx);
                    }
                    Err(e) => {
                        this.merge.source = None;
                        this.fail_pending("Merge", e);
                    }
                }
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// What's being merged in, for labels: the branch we initiated with, else
    /// `MERGE_HEAD`'s name from the snapshot (external merge/pull).
    fn merge_source_label(&self) -> String {
        self.merge
            .source
            .clone()
            .or_else(|| self.merge.in_progress.clone())
            .unwrap_or_else(|| "MERGE_HEAD".into())
    }

    /// Open (or re-focus) the resolve window on its CONFLICTS-LIST stage. Opened from
    /// the conflict outcome and from the banner's Resolve button — the latter covers
    /// merges started outside kyde, so the list is (re)read from git when we don't
    /// already have one.
    pub(crate) fn open_merge_window(&mut self, cx: &mut Context<Self>) {
        if self.merge.files.is_empty() {
            if let Some(repo) = self.repo() {
                self.merge.files = repo.conflict_entries();
            }
            self.merge.resolved.clear();
        }
        // Always land on the list first (the user asked to see the overview before any
        // merge diff); Merge… drills into a file.
        self.merge.selected = None;
        self.merge.model = None;
        self.merge.compare = MergeCompare::MergeView3;
        if self.merge.files.is_empty() {
            return;
        }
        self.merge.list_sel = self.first_unresolved();
        let title = format!(
            "Merge “{}” into “{}”",
            self.merge_source_label(),
            self.current_branch.as_deref().unwrap_or("HEAD")
        );
        self.open_modal_window(ModalKind::Merge, title, 1200.0, 760.0, cx);
        cx.notify();
    }

    /// Index of the first file not yet resolved this session.
    fn first_unresolved(&self) -> Option<usize> {
        self.merge
            .files
            .iter()
            .position(|f| !self.merge.resolved.contains(&f.path))
    }

    /// "Merge…" — load one conflicted file into the three panes: index stages 1/2/3 →
    /// `Merge3` under the current whitespace mode. Resolved files stay closed — their
    /// index stages are gone once staged.
    pub(crate) fn select_merge_file(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.merge.files.get(idx).cloned() else {
            return;
        };
        if self.merge.resolved.contains(&entry.path) {
            return;
        }
        let Some(repo) = self.repo() else {
            return;
        };
        self.merge.base_text = repo.conflict_stage(&entry.path, 1);
        self.merge.ours_text = repo.conflict_stage(&entry.path, 2);
        self.merge.theirs_text = repo.conflict_stage(&entry.path, 3);
        let m = kyde_diff::merge::Merge3::compute_with(
            &self.merge.base_text,
            &self.merge.ours_text,
            &self.merge.theirs_text,
            self.merge.ws,
        );
        self.merge.res = vec![Resolution::default(); m.chunks.len()];
        self.merge.selected = Some(idx);
        self.merge.list_sel = Some(idx);
        self.merge.compare = MergeCompare::MergeView3;
        self.merge.compare_open = false;
        self.merge.ws_open = false;
        let lang = self.effective_lang(&entry.path);
        let (ours_text, theirs_text) =
            (self.merge.ours_text.clone(), self.merge.theirs_text.clone());
        self.merge.ours.update(cx, |e, cx| {
            e.gutter_right = true;
            e.set_content(ours_text, lang, cx);
        });
        self.merge.theirs.update(cx, |e, cx| {
            e.set_content(theirs_text, lang, cx);
        });
        self.merge.model = Some(m);
        self.reload_merge_result(lang, true, cx);
        cx.notify();
    }

    /// "‹ Back" — leave the resolve stage for the conflicts list (undecided chunk state
    /// for the open file is discarded, like cancelling `IntelliJ`'s merge dialog).
    pub(crate) fn merge_back_to_list(&mut self, cx: &mut Context<Self>) {
        self.merge.list_sel = self.merge.selected.or(self.merge.list_sel);
        self.merge.selected = None;
        self.merge.model = None;
        self.merge.compare = MergeCompare::MergeView3;
        cx.notify();
    }

    /// One gutter click: set a chunk side's state (apply / ignore / back to pending)
    /// and rebuild the result pane.
    pub(crate) fn merge_set_side(
        &mut self,
        chunk: usize,
        ours_side: bool,
        state: SideState,
        cx: &mut Context<Self>,
    ) {
        if let Some(r) = self.merge.res.get_mut(chunk) {
            if ours_side {
                r.ours = state;
            } else {
                r.theirs = state;
            }
        }
        let lang = self.merge_lang();
        self.reload_merge_result(lang, false, cx);
        cx.notify();
    }

    /// Display-row starts of the changed (non-stable) chunks, in the 3-pane geometry
    /// (each chunk spans max(side lens) rows — must match `render_merge_panes`).
    fn merge_chunk_rows(&self) -> Vec<usize> {
        let Some(m) = self.merge.model.as_ref() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        let mut row = 0usize;
        for (i, c) in m.chunks.iter().enumerate() {
            if c.kind != ChunkKind::Stable {
                rows.push(row);
            }
            let rl = self.merge.res_ranges.get(i).map_or(0, std::ops::Range::len);
            row += c.ours.len().max(c.theirs.len()).max(rl);
        }
        rows
    }

    /// Toolbar ↑/↓ — scroll the aligned panes to the previous/next changed chunk,
    /// wrapping around (same anchor math as the diff view's hunk navigation).
    pub(crate) fn merge_nav_chunk(&mut self, next: bool, cx: &mut Context<Self>) {
        let rows = self.merge_chunk_rows();
        if rows.is_empty() {
            return;
        }
        let lh = editor::line_height_px();
        let top = (-f32::from(self.merge.scroll.offset().y) / lh).round() as i64;
        let anchor = top + SCROLL_CONTEXT_ROWS as i64;
        // `rows` is non-empty (checked above), so first/last are valid by construction.
        let (first, last) = (rows[0], rows[rows.len() - 1]);
        let target = if next {
            rows.iter()
                .copied()
                .find(|&r| (r as i64) > anchor)
                .unwrap_or(first) // past the last → wrap to the first
        } else {
            rows.iter()
                .copied()
                .rev()
                .find(|&r| (r as i64) < anchor)
                .unwrap_or(last) // before the first → wrap to the last
        };
        let row = target.saturating_sub(SCROLL_CONTEXT_ROWS) as f32;
        self.merge
            .scroll
            .set_offset(gpui::point(px(0.0), px(-row * lh)));
        cx.notify();
    }

    /// The toolbar's "Apply non-conflicting changes": bulk-apply every still-pending
    /// CLEAN chunk from the left (ours) and/or right (theirs) side. `Same` chunks are
    /// non-conflicting on both sides, so either button applies them.
    pub(crate) fn merge_apply_clean(&mut self, left: bool, right: bool, cx: &mut Context<Self>) {
        if let Some(m) = self.merge.model.as_ref() {
            for (i, c) in m.chunks.iter().enumerate() {
                let Some(r) = self.merge.res.get_mut(i) else {
                    continue;
                };
                match c.kind {
                    ChunkKind::Ours if left && r.ours == SideState::Pending => {
                        r.ours = SideState::Applied;
                    }
                    ChunkKind::Theirs if right && r.theirs == SideState::Pending => {
                        r.theirs = SideState::Applied;
                    }
                    ChunkKind::Same if (left || right) && r.ours == SideState::Pending => {
                        r.ours = SideState::Applied;
                    }
                    _ => {}
                }
            }
        }
        let lang = self.merge_lang();
        self.reload_merge_result(lang, false, cx);
        cx.notify();
    }

    /// Language of the file open in the resolve stage.
    fn merge_lang(&self) -> Lang {
        self.merge
            .selected
            .and_then(|i| self.merge.files.get(i))
            .map_or(Lang::PlainText, |e| self.effective_lang(&e.path))
    }

    /// Switch the whitespace comparison mode and recompute the open file's model under
    /// it (chunk boundaries change, so per-chunk decisions reset).
    pub(crate) fn merge_set_ws(&mut self, ws: WhitespaceMode, cx: &mut Context<Self>) {
        self.merge.ws_open = false;
        if self.merge.ws == ws {
            cx.notify();
            return;
        }
        self.merge.ws = ws;
        if let Some(i) = self.merge.selected {
            let cmp = self.merge.compare;
            self.select_merge_file(i, cx);
            self.merge.compare = cmp;
            if cmp != MergeCompare::MergeView3 {
                self.load_compare_panes(cx);
            }
        }
        cx.notify();
    }

    /// Switch what the resolve stage shows: the 3-pane merge, an INTERACTIVE Middle pair
    /// (Left/Right and Middle — the live merge panes + their apply gutter, just without
    /// the third pane), or a read-only comparison pair (Base pairs, Left and Right).
    pub(crate) fn merge_set_compare(&mut self, mode: MergeCompare, cx: &mut Context<Self>) {
        self.merge.compare_open = false;
        self.merge.compare = mode;
        // Only the read-only pairs need the separate compare editors loaded; the Middle
        // pairs reuse the live 3-pane editors (already loaded + decorated).
        if self.compare_pair().is_some() {
            self.load_compare_panes(cx);
        }
        cx.notify();
    }

    /// The two texts of a READ-ONLY Compare Contents pair (Base pairs, Left and Right)
    /// with their pane labels. `None` for the merge view and the interactive Middle pairs
    /// (those render the live merge panes instead — the middle IS the editable result).
    fn compare_pair(&self) -> Option<(String, String, String, String)> {
        let yours = format!(
            "Yours — {}",
            self.current_branch.as_deref().unwrap_or("HEAD")
        );
        let theirs = format!("Theirs — {}", self.merge_source_label());
        let result_text = || {
            let res = self.merge.res.clone();
            self.merge
                .model
                .as_ref()
                .map(|m| m.merged_text(|i| res.get(i).copied().unwrap_or_default()))
                .unwrap_or_default()
        };
        let (l, r, ll, rl) = match self.merge.compare {
            MergeCompare::MergeView3 | MergeCompare::LeftMiddle | MergeCompare::RightMiddle => {
                return None
            }
            MergeCompare::LeftRight => (
                self.merge.ours_text.clone(),
                self.merge.theirs_text.clone(),
                yours,
                theirs,
            ),
            MergeCompare::BaseLeft => (
                self.merge.base_text.clone(),
                self.merge.ours_text.clone(),
                "Base".to_string(),
                yours,
            ),
            MergeCompare::BaseMiddle => (
                self.merge.base_text.clone(),
                result_text(),
                "Base".to_string(),
                "Result".to_string(),
            ),
            MergeCompare::BaseRight => (
                self.merge.base_text.clone(),
                self.merge.theirs_text.clone(),
                "Base".to_string(),
                theirs,
            ),
        };
        Some((l, r, ll, rl))
    }

    /// Load the 2-pane Compare Contents editors for the current pair (side-by-side diff
    /// decorations under the current whitespace mode). The 3-pane editors are untouched,
    /// so switching back is instant.
    fn load_compare_panes(&mut self, cx: &mut Context<Self>) {
        let Some((lt, rt, _, _)) = self.compare_pair() else {
            return;
        };
        let lang = self.merge_lang();
        let d = FileDiff::compute_with(&lt, &rt, self.merge.ws);
        let first = d.hunks.first().map(|h| h.old_range.start);
        let (lbg, rbg) = diff_line_bgs(&d);
        let (lw, rw) = diff_word_bgs(&d);
        let (lf, lf_end, rf, rf_end) = diff_fillers(&d);
        let t = theme::get();
        self.merge.cmp_l.update(cx, |e, cx| {
            e.gutter_right = true;
            e.line_bg = lbg;
            e.word_bg = lw;
            e.word_bg_color = t.diff_word_old_bg;
            e.filler = lf;
            e.filler_end = lf_end;
            e.set_content(lt, lang, cx);
        });
        self.merge.cmp_r.update(cx, |e, cx| {
            e.line_bg = rbg;
            e.word_bg = rw;
            e.word_bg_color = t.diff_word_new_bg;
            e.filler = rf;
            e.filler_end = rf_end;
            e.set_content(rt, lang, cx);
        });
        let sh = self.merge.cmp_scroll.clone();
        self.merge
            .cmp_l
            .update(cx, |e, _| e.set_scroll_handle(sh.clone()));
        self.merge.cmp_r.update(cx, |e, _| e.set_scroll_handle(sh));
        let park = first.unwrap_or(0).saturating_sub(SCROLL_CONTEXT_ROWS) as f32;
        self.merge
            .cmp_scroll
            .set_offset(gpui::point(px(0.0), px(-park * editor::line_height_px())));
    }

    /// Rebuild the result pane text + all three panes' decorations (chunk backgrounds +
    /// alignment fillers) from the model and the current resolutions. `reset_scroll`
    /// parks the view at the first undecided chunk (initial load only).
    fn reload_merge_result(&mut self, lang: Lang, reset_scroll: bool, cx: &mut Context<Self>) {
        let Some(m) = self.merge.model.as_ref() else {
            return;
        };
        let t = theme::get();
        let res = self.merge.res.clone();
        let (lines, ranges) = m.result_lines(|i| res.get(i).copied().unwrap_or_default());
        let text = lines.join("\n");

        type BgMap = std::collections::HashMap<usize, kyde_color::Color>;
        type FillMap = std::collections::HashMap<usize, usize>;
        let (mut o_bg, mut r_bg, mut t_bg) = (BgMap::new(), BgMap::new(), BgMap::new());
        let (mut o_fill, mut r_fill, mut t_fill) = (FillMap::new(), FillMap::new(), FillMap::new());
        let (mut o_end, mut r_end, mut t_end) = (0usize, 0usize, 0usize);
        let mut first_pending_row = None;
        let mut row = 0usize;
        // N blank rows after a chunk = before the pane's next line (or trailing).
        let pad = |fill: &mut FillMap, end: &mut usize, next: usize, n: usize, len: usize| {
            if n == 0 {
                return;
            }
            if next >= len {
                *end += n;
            } else {
                *fill.entry(next).or_insert(0) += n;
            }
        };
        let paint = |bg: &mut BgMap, r: &std::ops::Range<usize>, c: kyde_color::Color| {
            for i in r.clone() {
                bg.insert(i, c);
            }
        };
        // A clean chunk's side tint: pending/applied = the modified blue; ignored = the
        // muted deleted grey (the change is being discarded).
        let side_tint = |st: SideState| match st {
            SideState::Ignored => t.diff_deleted_bg,
            _ => t.diff_modified_bg,
        };
        for (i, c) in m.chunks.iter().enumerate() {
            let rr = &ranges[i];
            let rows = c.ours.len().max(c.theirs.len()).max(rr.len());
            let st = res.get(i).copied().unwrap_or_default();
            match c.kind {
                ChunkKind::Stable => {}
                ChunkKind::Ours => {
                    paint(&mut o_bg, &c.ours, side_tint(st.ours));
                    if st.ours == SideState::Applied {
                        paint(&mut r_bg, rr, t.diff_modified_bg);
                    }
                }
                ChunkKind::Theirs => {
                    paint(&mut t_bg, &c.theirs, side_tint(st.theirs));
                    if st.theirs == SideState::Applied {
                        paint(&mut r_bg, rr, t.diff_modified_bg);
                    }
                }
                ChunkKind::Same => {
                    paint(&mut o_bg, &c.ours, side_tint(st.ours));
                    paint(&mut t_bg, &c.theirs, side_tint(st.ours));
                    if st.ours == SideState::Applied {
                        paint(&mut r_bg, rr, t.diff_modified_bg);
                    }
                }
                ChunkKind::Conflict => {
                    if st.resolved() {
                        // Applied side(s) blue, ignored grey; the result blue when
                        // anything landed.
                        paint(&mut o_bg, &c.ours, side_tint(st.ours));
                        paint(&mut t_bg, &c.theirs, side_tint(st.theirs));
                        if st.ours == SideState::Applied || st.theirs == SideState::Applied {
                            paint(&mut r_bg, rr, t.diff_modified_bg);
                        }
                    } else {
                        // Unresolved — the conflict red across all three panes.
                        paint(&mut o_bg, &c.ours, t.diff_conflict_bg);
                        paint(&mut t_bg, &c.theirs, t.diff_conflict_bg);
                        paint(&mut r_bg, rr, t.diff_conflict_bg);
                    }
                }
            }
            if first_pending_row.is_none() && !c.decided(st) {
                first_pending_row = Some(row);
            }
            pad(
                &mut o_fill,
                &mut o_end,
                c.ours.end,
                rows - c.ours.len(),
                m.ours.len(),
            );
            pad(
                &mut r_fill,
                &mut r_end,
                rr.end,
                rows - rr.len(),
                lines.len(),
            );
            pad(
                &mut t_fill,
                &mut t_end,
                c.theirs.end,
                rows - c.theirs.len(),
                m.theirs.len(),
            );
            row += rows;
        }
        self.merge.res_ranges = ranges;
        self.merge.ours.update(cx, |e, _| {
            e.line_bg = o_bg;
            e.filler = o_fill;
            e.filler_end = o_end;
        });
        self.merge.theirs.update(cx, |e, _| {
            e.line_bg = t_bg;
            e.filler = t_fill;
            e.filler_end = t_end;
        });
        self.merge.result.update(cx, |e, cx| {
            e.line_bg = r_bg;
            e.filler = r_fill;
            e.filler_end = r_end;
            e.set_content(text, lang, cx);
        });
        // All three panes share one scroll handle → aligned rows stay in sync.
        let sh = self.merge.scroll.clone();
        self.merge
            .ours
            .update(cx, |e, _| e.set_scroll_handle(sh.clone()));
        self.merge
            .result
            .update(cx, |e, _| e.set_scroll_handle(sh.clone()));
        self.merge.theirs.update(cx, |e, _| e.set_scroll_handle(sh));
        if reset_scroll {
            let park = first_pending_row
                .unwrap_or(0)
                .saturating_sub(SCROLL_CONTEXT_ROWS) as f32;
            self.merge
                .scroll
                .set_offset(gpui::point(px(0.0), px(-park * editor::line_height_px())));
        }
    }

    /// "Accept Yours"/"Accept Theirs": resolve a whole file with that side's version
    /// (`git checkout --ours/--theirs` semantics — a deleted side deletes the file),
    /// stage it, and return to the list. Acts on the resolve-stage file, else the
    /// list selection.
    pub(crate) fn merge_accept_file(&mut self, take_ours: bool, cx: &mut Context<Self>) {
        let Some(idx) = self.merge.selected.or(self.merge.list_sel) else {
            return;
        };
        let Some(entry) = self.merge.files.get(idx).cloned() else {
            return;
        };
        if self.merge.resolved.contains(&entry.path) {
            return;
        }
        let Some(repo) = self.repo() else {
            return;
        };
        let side = if take_ours { entry.ours } else { entry.theirs };
        let result = if side == git::ConflictSide::Deleted {
            // That side deleted the file → accepting it deletes our copy too.
            let exists = self
                .repo_root
                .as_ref()
                .is_some_and(|r| r.join(&entry.path).exists());
            if exists {
                repo.delete_file(&entry.path)
            } else {
                Ok(())
            }
            .and_then(|()| repo.stage(&entry.path))
        } else {
            let content = repo.conflict_stage(&entry.path, if take_ours { 2 } else { 3 });
            repo.save_file(&entry.path, &content)
                .and_then(|()| repo.stage(&entry.path))
        };
        if let Err(e) = result {
            self.fail("Resolving file", e);
            return;
        }
        self.finish_merge_file(entry.path, cx);
    }

    /// Whether every chunk of the SELECTED file has been decided (clean AND conflict).
    fn merge_file_ready(&self) -> bool {
        self.merge_pending() == 0
    }

    /// Chunks of the selected file still awaiting a decision.
    fn merge_pending(&self) -> usize {
        self.merge.model.as_ref().map_or(0, |m| {
            m.undecided(|i| self.merge.res.get(i).copied().unwrap_or_default())
        })
    }

    /// "Apply": write the resolved result to disk, stage it, and return to the list.
    /// No-op until every chunk in the file is decided.
    pub(crate) fn merge_apply_file(&mut self, cx: &mut Context<Self>) {
        if !self.merge_file_ready() {
            return;
        }
        let (Some(m), Some(idx)) = (self.merge.model.as_ref(), self.merge.selected) else {
            return;
        };
        let Some(entry) = self.merge.files.get(idx).cloned() else {
            return;
        };
        let res = self.merge.res.clone();
        let text = m.merged_text(|i| res.get(i).copied().unwrap_or_default());
        let Some(repo) = self.repo() else {
            return;
        };
        if let Err(e) = repo.save_file(&entry.path, &text) {
            self.fail("Applying merge result", e);
            return;
        }
        if let Err(e) = repo.stage(&entry.path) {
            self.fail("Staging resolved file", e);
            return;
        }
        self.finish_merge_file(entry.path, cx);
    }

    /// Mark a file resolved and return to the conflicts list, its selection on the next
    /// unresolved file (the footer offers Commit Merge once none is left).
    fn finish_merge_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.merge.resolved.insert(path);
        self.merge.selected = None;
        self.merge.model = None;
        self.merge.compare = MergeCompare::MergeView3;
        self.merge.list_sel = self.first_unresolved();
        self.refresh(cx);
        cx.notify();
    }

    /// Conclude the merge (`git commit --no-edit` → git's prepared `MERGE_MSG`), off the
    /// UI thread. Closes the window and clears the merge state on success.
    pub(crate) fn merge_commit(&mut self, cx: &mut Context<Self>) {
        if self.merge.busy {
            return;
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        self.merge.busy = true;
        cx.notify();
        let src = self.merge_source_label();
        let dst = self.current_branch.clone().unwrap_or_else(|| "HEAD".into());
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { Repo::discover(&root).and_then(|r| r.commit_merge()) })
                .await;
            this.update(cx, |this, cx| {
                this.merge.busy = false;
                match res {
                    Ok(()) => {
                        this.clear_merge_state();
                        this.merge.note = Some(format!("Merged “{src}” into “{dst}”."));
                        this.close_modal_window(ModalKind::Merge, cx);
                    }
                    Err(e) => this.fail_pending("Commit merge", e),
                }
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Abort the in-progress merge (`git merge --abort`), restoring the pre-merge tree.
    pub(crate) fn merge_abort_op(&mut self, cx: &mut Context<Self>) {
        if self.merge.busy {
            return;
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        self.merge.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { Repo::discover(&root).and_then(|r| r.merge_abort()) })
                .await;
            this.update(cx, |this, cx| {
                this.merge.busy = false;
                match res {
                    Ok(()) => {
                        this.clear_merge_state();
                        this.close_modal_window(ModalKind::Merge, cx);
                    }
                    Err(e) => this.fail_pending("Abort merge", e),
                }
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Drop all per-merge session state (after commit/abort).
    fn clear_merge_state(&mut self) {
        self.merge.source = None;
        self.merge.files.clear();
        self.merge.resolved.clear();
        self.merge.list_sel = None;
        self.merge.selected = None;
        self.merge.model = None;
        self.merge.res.clear();
        self.merge.res_ranges.clear();
        self.merge.compare = MergeCompare::MergeView3;
    }

    // ── banners ───────────────────────────────────────────────────

    /// Bottom banner while a merge is in progress: source branch + live conflicted
    /// count, with Resolve/Commit + Abort. Shown for merges started here AND outside.
    pub(crate) fn render_merge_banner(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let src = self.merge_source_label();
        // Live from `git status` (kept fresh by refresh) — not the window's session list.
        let n = self
            .files
            .iter()
            .filter(|f| f.status == git::FileStatus::Conflict)
            .count();
        let msg = if n > 0 {
            format!(
                "Merging “{src}” — {n} conflicted file{}",
                if n == 1 { "" } else { "s" }
            )
        } else {
            format!("Merging “{src}” — all conflicts resolved")
        };
        let action = if n > 0 {
            btn_primary("merge-banner-resolve", "Resolve Conflicts…").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.open_merge_window(cx)),
            )
        } else {
            btn_primary("merge-banner-commit", "Commit Merge").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.merge_commit(cx)),
            )
        };
        let abort = btn_secondary("merge-banner-abort", "Abort").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| this.merge_abort_op(cx)),
        );
        // Big and obvious on purpose: full-width, conflict-red tinted, directly under the
        // titlebar — the way back into the conflicts window can never be lost.
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .bg(t.diff_conflict_bg)
            .border_b_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(px(t.ui_font_size + 1.0))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.text)
            .child(div().flex_none().text_color(t.status_conflict).child("⚠"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(msg)),
            )
            .child(abort)
            .child(action)
            .into_any_element()
    }

    /// Neutral one-line success note ("Merged X into Y."), dismissed by its ×.
    pub(crate) fn render_merge_note(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let Some(msg) = self.merge.note.clone() else {
            return div().into_any_element();
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .py_1p5()
            .bg(t.bg_mid)
            .border_t_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(px(t.ui_font_size))
            .text_color(t.text)
            .child(div().flex_none().text_color(t.status_added).child("✓"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(msg)),
            )
            .child(
                div()
                    .id("merge-note-close")
                    .flex_none()
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(t.line_number)
                    .hover(|s| s.bg(t.bg_light).text_color(t.text))
                    .child("×")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.merge.note = None;
                            cx.notify();
                        }),
                    ),
            )
            .into_any_element()
    }

    /// The open Browse file, when it currently has unresolved merge conflicts (from the
    /// live `git status`) — drives the per-file editor banner.
    pub(crate) fn open_file_conflict(&self) -> Option<PathBuf> {
        let p = self.browse.open_path.as_ref()?;
        self.files
            .iter()
            .find(|f| &f.path == p && f.status == git::FileStatus::Conflict)
            .map(|f| f.path.clone())
    }

    /// "Resolve conflicts…" from the per-file banner: open the merge window and drill
    /// straight into that file's 3-pane view (the list stays one ‹ Back away).
    pub(crate) fn resolve_conflict_for(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.open_merge_window(cx);
        if let Some(i) = self.merge.files.iter().position(|e| e.path == path) {
            self.select_merge_file(i, cx);
        }
    }

    /// Top-of-editor banner when the OPEN file has unresolved merge conflicts (`IntelliJ`'s
    /// "File has unresolved merge conflicts — Resolve conflicts…"). Rendered by Browse
    /// under the tab bar, same slot as the install banner.
    pub(crate) fn render_conflict_file_banner(
        &self,
        path: PathBuf,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_4()
            .px_3()
            .py_2()
            .bg(t.diff_conflict_bg)
            .border_b_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(fs)
            .text_color(t.text)
            .child(div().flex_none().text_color(t.status_conflict).child("⚠"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child("File has unresolved merge conflicts"),
            )
            .child(
                div()
                    .id("resolve-conflicts-link")
                    .flex_none()
                    .text_color(t.primary)
                    .cursor_pointer()
                    .hover(|s| s.text_color(t.text))
                    .child("Resolve conflicts…")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.resolve_conflict_for(path.clone(), cx);
                        }),
                    ),
            )
            .into_any_element()
    }

    // ── the resolve window ────────────────────────────────────────

    /// Body of the Merge window: the conflicts-list stage, or (after Merge…) the
    /// resolve stage for one file.
    pub(crate) fn render_merge_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.merge.selected.is_some() && self.merge.model.is_some() {
            self.render_merge_resolve(cx)
        } else {
            self.render_conflicts_list(cx)
        }
    }

    /// Stage 1 — the conflicts list: every conflicted file with what each side did,
    /// plus Accept Yours / Accept Theirs / Merge… for the selected row.
    fn render_conflicts_list(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        let src = self.merge_source_label();
        let dst = self.current_branch.clone().unwrap_or_else(|| "HEAD".into());

        let heading = div()
            .px_4()
            .pt_3()
            .pb_2()
            .text_color(t.text)
            .child(SharedString::from(format!(
                "Merging branch “{src}” into branch “{dst}”"
            )));

        const COL_W: f32 = 110.0;
        let col = |label: String| {
            div()
                .w(px(COL_W))
                .flex_none()
                .truncate()
                .child(SharedString::from(label))
        };
        let head_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .mx_2()
            .px_2()
            .h(px(24.0))
            .border_b_1()
            .border_color(t.divider)
            .text_color(t.line_number)
            .child(div().flex_1().min_w_0().child("Name"))
            .child(col(format!("Yours ({dst})")))
            .child(col(format!("Theirs ({src})")));

        let side_color = |s: git::ConflictSide| match s {
            git::ConflictSide::Modified => t.status_modified,
            git::ConflictSide::Added => t.status_added,
            git::ConflictSide::Deleted => t.status_conflict,
        };
        let files = self.merge.files.clone();
        let rows: Vec<gpui::AnyElement> = files
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let resolved = self.merge.resolved.contains(&entry.path);
                let sel = !resolved && self.merge.list_sel == Some(i);
                let name = entry
                    .path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let dir = entry
                    .path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let row = div()
                    .id(SharedString::from(format!("conflict-{i}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .mx_2()
                    .px_2()
                    .h(px(28.0))
                    .rounded_md()
                    .when(sel, |d| d.bg(t.selected_bg))
                    .when(!resolved, |d| {
                        d.cursor_pointer().hover(|s| s.bg(t.bg_light))
                    })
                    .child(
                        div()
                            .flex_none()
                            .child(badge_inner(file_badge(&entry.path), 2.0)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(if resolved { t.line_number } else { t.text })
                            .child(SharedString::from(name)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(t.line_number)
                            .child(SharedString::from(dir)),
                    );
                let row = if resolved {
                    row.child(
                        div()
                            .w(px(COL_W * 2.0 + 8.0))
                            .flex_none()
                            .text_color(t.status_added)
                            .child("✓ Resolved"),
                    )
                } else {
                    row.child(
                        div()
                            .w(px(COL_W))
                            .flex_none()
                            .text_color(side_color(entry.ours))
                            .child(entry.ours.label()),
                    )
                    .child(
                        div()
                            .w(px(COL_W))
                            .flex_none()
                            .text_color(side_color(entry.theirs))
                            .child(entry.theirs.label()),
                    )
                };
                let row = if resolved {
                    row
                } else {
                    row.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                            this.merge.list_sel = Some(i);
                            // Double-click = Merge… (same as the button).
                            if e.click_count >= 2 {
                                this.select_merge_file(i, cx);
                            }
                            cx.notify();
                        }),
                    )
                };
                row.into_any_element()
            })
            .collect();

        // Right-hand action column, acting on the selected row.
        let can_act = self
            .merge
            .list_sel
            .and_then(|i| self.merge.files.get(i))
            .is_some_and(|e| !self.merge.resolved.contains(&e.path))
            && !self.merge.busy;
        let act = |btn: gpui::Stateful<gpui::Div>| btn.w_full().when(!can_act, |d| d.opacity(0.5));
        let buttons = div()
            .flex()
            .flex_col()
            .gap_2()
            .w(px(150.0))
            .flex_none()
            .pt(px(24.0))
            .pr_3()
            .child(
                act(btn_secondary("list-accept-ours", "Accept Yours")).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.merge_accept_file(true, cx)),
                ),
            )
            .child(
                act(btn_secondary("list-accept-theirs", "Accept Theirs")).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.merge_accept_file(false, cx)),
                ),
            )
            .child(act(btn_primary("list-merge", "Merge…")).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    if let Some(i) = this.merge.list_sel {
                        this.select_merge_file(i, cx);
                    }
                }),
            ));

        let table = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(head_row)
            .child(
                div()
                    .id("conflict-list")
                    .overflow_y_scroll()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .py_1()
                    .children(rows),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.panel_bg)
            .font_family(ui)
            .text_size(px(t.ui_font_size))
            .text_color(t.text)
            .child(heading)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(table)
                    .child(buttons),
            )
            .child(self.render_merge_footer(false, cx))
            .into_any_element()
    }

    /// The shared footer: Abort Merge, progress, and — in the resolve stage — Apply,
    /// or Commit Merge once every file is resolved.
    fn render_merge_footer(&mut self, resolve_stage: bool, cx: &mut Context<Self>) -> gpui::Div {
        let t = theme::get();
        let files = &self.merge.files;
        let all_done =
            !files.is_empty() && files.iter().all(|f| self.merge.resolved.contains(&f.path));
        let pending = self.merge_pending();
        let ready = pending == 0;
        let progress = format!(
            "{} of {} files resolved",
            self.merge.resolved.len(),
            files.len()
        );
        let mut footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(t.divider)
            .child(btn_secondary("merge-abort", "Abort Merge").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.merge_abort_op(cx)),
            ))
            .child(
                div()
                    .flex_1()
                    .text_color(t.line_number)
                    .child(SharedString::from(progress)),
            );
        if resolve_stage {
            footer = footer
                .child(
                    btn_secondary("merge-accept-ours", "Accept Yours").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.merge_accept_file(true, cx)),
                    ),
                )
                .child(
                    btn_secondary("merge-accept-theirs", "Accept Theirs").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.merge_accept_file(false, cx)),
                    ),
                );
            let label = if ready {
                "Apply".to_string()
            } else {
                format!("Apply ({pending} pending)")
            };
            footer = footer.child(
                btn_primary("merge-apply", SharedString::from(label))
                    .when(!ready, |d| d.opacity(0.5))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.merge_apply_file(cx)),
                    ),
            );
        } else if all_done {
            footer = footer.child(btn_primary("merge-commit", "Commit Merge").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.merge_commit(cx)),
            ));
        }
        footer
    }

    /// Stage 2 — the resolve view for one file: toolbar (back, Compare Contents,
    /// apply-non-conflicting, whitespace mode), the panes, and the footer.
    fn render_merge_resolve(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let t = theme::get();
        let fname = self
            .merge
            .selected
            .and_then(|i| self.merge.files.get(i))
            .map(|e| e.path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let pending = self.merge_pending();
        let conflicts_left = self.merge.model.as_ref().map_or(0, |m| {
            m.chunks
                .iter()
                .enumerate()
                .filter(|(i, c)| {
                    c.kind == ChunkKind::Conflict
                        && !self
                            .merge
                            .res
                            .get(*i)
                            .copied()
                            .unwrap_or_default()
                            .resolved()
                })
                .count()
        });

        // Compare Contents dropdown — `ui::select` so the open panel is a DEFERRED
        // overlay painted above the panes (a hand-rolled absolute panel was clipped by
        // the later-painted pane siblings).
        let cmp_labels: Vec<&'static str> = MergeCompare::ALL.iter().map(|(_, l)| *l).collect();
        let cmp_sel = MergeCompare::ALL
            .iter()
            .position(|(m, _)| *m == self.merge.compare);
        let cmp_select = ui::select(
            cx,
            "merge-compare-select",
            180.0,
            &cmp_labels,
            cmp_sel,
            self.merge.compare_open,
            |this, cx| {
                this.merge.compare_open = !this.merge.compare_open;
                this.merge.ws_open = false;
                cx.notify();
            },
            |this, i, cx| {
                if let Some((mode, _)) = MergeCompare::ALL.get(i) {
                    this.merge_set_compare(*mode, cx);
                }
            },
        );

        // Whitespace dropdown (same deferred `ui::select`).
        const WS: &[(WhitespaceMode, &str)] = &[
            (WhitespaceMode::Exact, "Do not ignore whitespaces"),
            (WhitespaceMode::Trim, "Trim whitespaces"),
            (WhitespaceMode::IgnoreAll, "Ignore whitespaces"),
        ];
        let ws_labels: Vec<&'static str> = WS.iter().map(|(_, l)| *l).collect();
        let ws_sel = WS.iter().position(|(m, _)| *m == self.merge.ws);
        let ws_select = ui::select(
            cx,
            "merge-ws-select",
            230.0,
            &ws_labels,
            ws_sel,
            self.merge.ws_open,
            |this, cx| {
                this.merge.ws_open = !this.merge.ws_open;
                this.merge.compare_open = false;
                cx.notify();
            },
            |this, i, cx| {
                if let Some((mode, _)) = WS.get(i) {
                    this.merge_set_ws(*mode, cx);
                }
            },
        );

        // "Apply non-conflicting changes: » Left  »« All  « Right".
        let apply_btn = |id: &'static str, glyph: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .px_2()
                .py_0p5()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(t.bg_light))
                .text_color(t.secondary_text)
                .child(div().text_color(t.primary).child(glyph))
                .child(label)
        };
        let apply_group = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(
                div()
                    .text_color(t.line_number)
                    .child("Apply non-conflicting changes:"),
            )
            .child(apply_btn("merge-apply-left", "»", "Left").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.merge_apply_clean(true, false, cx)),
            ))
            .child(apply_btn("merge-apply-all", "»«", "All").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.merge_apply_clean(true, true, cx)),
            ))
            .child(apply_btn("merge-apply-right", "«", "Right").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.merge_apply_clean(false, true, cx)),
            ));

        let back = div()
            .id("merge-back")
            .flex_none()
            .px_2()
            .py_0p5()
            .rounded_md()
            .cursor_pointer()
            .text_color(t.secondary_text)
            .hover(|s| s.bg(t.bg_mid).text_color(t.text))
            .child("‹ Back")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.merge_back_to_list(cx)),
            );

        // Far-left ↑/↓ — jump to the previous/next changed chunk (wraps around).
        let nav_arrow = |id: &'static str, icon: &'static str, tip: &'static str, next: bool| {
            div()
                .id(id)
                .size(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(t.bg_light))
                .tooltip(move |_w, cx| cx.new(|_| Tip(tip.into())).into())
                .child(svg().path(icon).size(px(15.0)).text_color(t.secondary_text))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.merge_nav_chunk(next, cx)),
                )
        };
        let nav = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_0p5()
            .child(nav_arrow(
                "merge-nav-prev",
                "icons/arrow-up.svg",
                "Previous conflict",
                false,
            ))
            .child(nav_arrow(
                "merge-nav-next",
                "icons/arrow-down.svg",
                "Next conflict",
                true,
            ))
            // Thin separator between the nav pair and Back.
            .child(div().w(px(1.0)).h(px(16.0)).mx_1().bg(t.divider));

        let toolbar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_2()
            .py_1p5()
            .border_b_1()
            .border_color(t.divider)
            .child(nav)
            .child(back)
            .child(
                div()
                    .max_w(px(260.0))
                    .min_w_0()
                    .truncate()
                    .text_color(t.text)
                    .child(SharedString::from(fname)),
            )
            .child(cmp_select)
            .child(apply_group)
            .child(ws_select)
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_color(if conflicts_left > 0 {
                        t.status_conflict
                    } else {
                        t.status_added
                    })
                    .child(SharedString::from(if conflicts_left > 0 {
                        format!(
                            "{conflicts_left} conflict{} remaining",
                            if conflicts_left == 1 { "" } else { "s" }
                        )
                    } else if pending > 0 {
                        format!(
                            "{pending} change{} pending",
                            if pending == 1 { "" } else { "s" }
                        )
                    } else {
                        "All changes resolved".to_string()
                    })),
            );

        let fs = px(t.editor_font_size);
        // The 3-pane view AND the interactive Middle pairs render the live merge panes
        // (the Middle pairs are 2-pane subsets with the apply gutter — the middle IS the
        // editable result); the Base pairs + Left/Right are read-only comparisons.
        let center = match self.merge.compare {
            MergeCompare::MergeView3 | MergeCompare::LeftMiddle | MergeCompare::RightMiddle => {
                self.render_merge_panes(fs, cx)
            }
            _ => self.render_compare_panes(fs, cx),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.panel_bg)
            .font_family(ui)
            .text_size(px(t.ui_font_size))
            .text_color(t.text)
            .child(toolbar)
            .child(center)
            .child(self.render_merge_footer(true, cx))
            .into_any_element()
    }

    /// The three aligned panes + two control gutters for the selected file.
    fn render_merge_panes(&mut self, fs: gpui::Pixels, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let ui = theme::font::UI_FAMILY;
        let Some(m) = self.merge.model.as_ref() else {
            return div().into_any_element();
        };
        let row_h = px(editor::line_height_px());

        // Row geometry: each chunk spans max(len over the three sides) display rows.
        let mut starts = Vec::with_capacity(m.chunks.len());
        let mut total = 0usize;
        for (i, c) in m.chunks.iter().enumerate() {
            starts.push(total);
            let rl = self.merge.res_ranges.get(i).map_or(0, std::ops::Range::len);
            total += c.ours.len().max(c.theirs.len()).max(rl);
        }
        let total_h = row_h * total as f32;

        // Gutter controls for every non-stable chunk: (chunk idx, row, kind).
        let res = self.merge.res.clone();
        let changed: Vec<(usize, usize, ChunkKind)> = m
            .chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.kind != ChunkKind::Stable)
            .map(|(i, c)| (i, starts[i], c.kind))
            .collect();

        // One control box: a glyph button that sets `state` on (chunk, side).
        let ctl = |id: String,
                   glyph: &'static str,
                   tip: &'static str,
                   chunk: usize,
                   ours_side: bool,
                   state: SideState,
                   color: kyde_color::Color,
                   cx: &mut Context<Self>| {
            let tip = SharedString::from(tip);
            div()
                .id(SharedString::from(id))
                .flex()
                .items_center()
                .justify_center()
                .size(px(22.0))
                .rounded_sm()
                .text_size(px(17.0))
                .line_height(px(17.0))
                .text_color(color)
                .hover(|s| s.bg(t.bg_light).text_color(t.primary))
                .cursor_pointer()
                .tooltip(move |_w, cx| cx.new(|_| Tip(tip.clone())).into())
                .child(glyph)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.merge_set_side(chunk, ours_side, state, cx);
                    }),
                )
        };
        // Build one gutter's controls: apply/ignore while pending, an undo chip after.
        let mut left_ctls: Vec<gpui::AnyElement> = Vec::new();
        let mut right_ctls: Vec<gpui::AnyElement> = Vec::new();
        for &(i, row, kind) in &changed {
            let st = res.get(i).copied().unwrap_or_default();
            let row_box = |children: Vec<gpui::AnyElement>| {
                div()
                    .absolute()
                    .top(row_h * row as f32)
                    .left_0()
                    .right_0()
                    .h(row_h)
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .gap_0p5()
                    .children(children)
                    .into_any_element()
            };
            // `ours_side` = which Resolution FIELD the control drives (a `Same` chunk is
            // one decision carried on the ours side, whichever gutter it's clicked from).
            let side =
                |gutter_left: bool, ours_side: bool, st: SideState, cx: &mut Context<Self>| {
                    let arrow = if gutter_left { "»" } else { "«" };
                    match st {
                        SideState::Pending => vec![
                            ctl(
                                format!("m-apply-{i}-{gutter_left}"),
                                arrow,
                                "Apply this change",
                                i,
                                ours_side,
                                SideState::Applied,
                                t.line_number,
                                cx,
                            )
                            .into_any_element(),
                            ctl(
                                format!("m-ignore-{i}-{gutter_left}"),
                                "×",
                                "Ignore this change",
                                i,
                                ours_side,
                                SideState::Ignored,
                                t.line_number,
                                cx,
                            )
                            .into_any_element(),
                        ],
                        SideState::Applied => vec![ctl(
                            format!("m-undo-{i}-{gutter_left}"),
                            "✓",
                            "Applied — click to undo",
                            i,
                            ours_side,
                            SideState::Pending,
                            t.status_added,
                            cx,
                        )
                        .into_any_element()],
                        SideState::Ignored => vec![ctl(
                            format!("m-undo-{i}-{gutter_left}"),
                            "−",
                            "Ignored — click to undo",
                            i,
                            ours_side,
                            SideState::Pending,
                            t.line_number,
                            cx,
                        )
                        .into_any_element()],
                    }
                };
            match kind {
                ChunkKind::Ours => left_ctls.push(row_box(side(true, true, st.ours, cx))),
                ChunkKind::Theirs => right_ctls.push(row_box(side(false, false, st.theirs, cx))),
                ChunkKind::Same => {
                    left_ctls.push(row_box(side(true, true, st.ours, cx)));
                    right_ctls.push(row_box(side(false, true, st.ours, cx)));
                }
                ChunkKind::Conflict => {
                    left_ctls.push(row_box(side(true, true, st.ours, cx)));
                    right_ctls.push(row_box(side(false, false, st.theirs, cx)));
                }
                ChunkKind::Stable => {}
            }
        }

        let scroll_y = self.merge.scroll.offset().y;
        let gutter = |id: &'static str, ctls: Vec<gpui::AnyElement>| {
            div()
                .id(id)
                .w(px(MERGE_GUTTER_W))
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
                )
        };

        // Panes: shared vertical scroll, per-pane content width (long lines scroll with
        // the shared handle horizontally too — same trade-off as the diff panes).
        let ow = self.merge.ours.read(cx).content_width();
        let rw = self.merge.result.read(cx).content_width();
        let tw = self.merge.theirs.read(cx).content_width();
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
        let scroll = self.merge.scroll.clone();
        // Which of the live panes this mode shows. The Middle pairs are 2-pane SUBSETS
        // of the 3-pane view — same editors, decorations, geometry, and apply gutter, so
        // switching modes never reloads and edits stay live. (Alignment rows still span
        // max over all three sides — the hidden pane can leave a filler row, which keeps
        // the gutter rows + scroll position identical across mode switches.)
        let mode = self.merge.compare;
        let show_ours = matches!(mode, MergeCompare::MergeView3 | MergeCompare::LeftMiddle);
        let show_theirs = matches!(mode, MergeCompare::MergeView3 | MergeCompare::RightMiddle);
        let mut panes = div().flex().flex_row().flex_1().min_h_0();
        if show_ours {
            panes = panes
                .child(pane(
                    "merge-ours",
                    ow,
                    self.merge.ours.clone().into_any_element(),
                    &scroll,
                ))
                .child(gutter("merge-gutter-l", left_ctls));
        }
        panes = panes.child(pane(
            "merge-result",
            rw,
            self.merge.result.clone().into_any_element(),
            &scroll,
        ));
        if show_theirs {
            panes = panes
                .child(gutter("merge-gutter-r", right_ctls))
                .child(pane(
                    "merge-theirs",
                    tw,
                    self.merge.theirs.clone().into_any_element(),
                    &scroll,
                ));
        }

        // Column headers over the panes (mirroring whichever panes are shown).
        let hdr = |label: String| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(t.secondary_text)
                .child(SharedString::from(label))
        };
        let mut heads = div()
            .flex()
            .flex_row()
            .items_center()
            .px_2()
            .py_1()
            .font_family(ui)
            .text_size(px(t.ui_font_size));
        if show_ours {
            heads = heads
                .child(hdr(format!(
                    "Yours — {}",
                    self.current_branch.as_deref().unwrap_or("HEAD")
                )))
                .child(div().w(px(MERGE_GUTTER_W)).flex_none());
        }
        heads = heads.child(hdr("Result".to_string()));
        if show_theirs {
            heads = heads
                .child(div().w(px(MERGE_GUTTER_W)).flex_none())
                .child(hdr(format!("Theirs — {}", self.merge_source_label())));
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(heads)
            .child(panes)
            .into_any_element()
    }

    /// A Compare Contents pair: two read-only aligned panes (loaded by
    /// `load_compare_panes`), no gutter controls.
    fn render_compare_panes(
        &mut self,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let ui = theme::font::UI_FAMILY;
        let Some((_, _, ll, rl)) = self.compare_pair() else {
            return div().into_any_element();
        };
        let lw = self.merge.cmp_l.read(cx).content_width();
        let rw = self.merge.cmp_r.read(cx).content_width();
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
        let hdr = |label: String| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(t.secondary_text)
                .child(SharedString::from(label))
        };
        let heads = div()
            .flex()
            .flex_row()
            .items_center()
            .px_2()
            .py_1()
            .font_family(ui)
            .text_size(px(t.ui_font_size))
            .child(hdr(ll))
            .child(div().w(px(12.0)).flex_none())
            .child(hdr(rl));
        let scroll = self.merge.cmp_scroll.clone();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(heads)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(pane(
                        "merge-cmp-l",
                        lw,
                        self.merge.cmp_l.clone().into_any_element(),
                        &scroll,
                    ))
                    .child(
                        div()
                            .w(px(12.0))
                            .flex_none()
                            .h_full()
                            .bg(t.diff_separator_bg)
                            .border_l(px(1.0))
                            .border_r(px(1.0))
                            .border_color(t.divider),
                    )
                    .child(pane(
                        "merge-cmp-r",
                        rw,
                        self.merge.cmp_r.clone().into_any_element(),
                        &scroll,
                    )),
            )
            .into_any_element()
    }
}
