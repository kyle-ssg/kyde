//! Compare-two-files (issue #42): a native window with two read-only aligned
//! panes (the merge Compare Contents pattern) plus a center gutter that applies
//! any hunk in EITHER direction — `»` copies the left side's lines into the
//! right file, `«` copies the right side's into the left. Applying writes the
//! target file to disk (the panes are views, not buffers) and re-diffs.

use crate::*;

impl Kyde {
    /// Open (or re-focus) the Compare window for `left` ↔ `right`. Paths are
    /// repo-relative (or absolute for scratch files), like every open-file path.
    pub(crate) fn open_compare(&mut self, left: PathBuf, right: PathBuf, cx: &mut Context<Self>) {
        let name = |p: &PathBuf| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let title = format!("Compare: {} ↔ {}", name(&left), name(&right));
        self.compare.left_path = Some(left);
        self.compare.right_path = Some(right);
        self.reload_compare(true, cx);
        self.open_modal_window(ModalKind::Compare, title, 1200.0, 760.0, cx);
    }

    /// Read one side's current text from disk (same resolution as `open_file`:
    /// absolute = scratch, else through the repo / project root).
    fn read_compare_side(&self, rel: &PathBuf) -> String {
        if rel.is_absolute() {
            return std::fs::read_to_string(rel).unwrap_or_default();
        }
        if let Some(repo) = self.repo() {
            return repo.working_content(rel).ok().unwrap_or_default();
        }
        self.repo_root
            .as_ref()
            .and_then(|root| std::fs::read_to_string(root.join(rel)).ok())
            .unwrap_or_default()
    }

    /// (Re)load both panes from disk and re-diff: contents, per-side syntax,
    /// diff decorations (line tints, word emphasis, alignment fillers), shared
    /// scroll. `park` scrolls to just above the first hunk (window open only —
    /// an apply keeps the user's scroll position).
    pub(crate) fn reload_compare(&mut self, park: bool, cx: &mut Context<Self>) {
        let (Some(lp), Some(rp)) = (
            self.compare.left_path.clone(),
            self.compare.right_path.clone(),
        ) else {
            return;
        };
        let lt = self.read_compare_side(&lp);
        let rt = self.read_compare_side(&rp);
        let d = FileDiff::compute(&lt, &rt);
        let first = d.hunks.first().map(|h| h.old_range.start);
        let (lbg, rbg) = diff_line_bgs(&d);
        let (lw, rw) = diff_word_bgs(&d);
        let (lf, lf_end, rf, rf_end) = diff_fillers(&d);
        let (l_lang, r_lang) = (self.effective_lang(&lp), self.effective_lang(&rp));
        let t = theme::get();
        self.compare.left.update(cx, |e, cx| {
            e.gutter_right = true; // numbers toward the center gutter, like the diff base pane
            e.line_bg = lbg;
            e.word_bg = lw;
            e.word_bg_color = t.diff_word_old_bg;
            e.filler = lf;
            e.filler_end = lf_end;
            e.set_content(lt, l_lang, cx);
        });
        self.compare.right.update(cx, |e, cx| {
            e.line_bg = rbg;
            e.word_bg = rw;
            e.word_bg_color = t.diff_word_new_bg;
            e.filler = rf;
            e.filler_end = rf_end;
            e.set_content(rt, r_lang, cx);
        });
        let sh = self.compare.scroll.clone();
        self.compare
            .left
            .update(cx, |e, _| e.set_scroll_handle(sh.clone()));
        self.compare
            .right
            .update(cx, |e, _| e.set_scroll_handle(sh));
        if park {
            let row = first.unwrap_or(0).saturating_sub(SCROLL_CONTEXT_ROWS) as f32;
            self.compare
                .scroll
                .set_offset(gpui::point(px(0.0), px(-row * editor::line_height_px())));
        }
        self.compare.diff = Some(d);
        cx.notify();
    }

    /// Copy hunk `hi` across: `to_right` = the right file takes the LEFT side's
    /// lines for that hunk, else the left file takes the right's. Outside hunks
    /// the sides are identical, so both directions come from
    /// `partial_new_content`: right←left = every hunk EXCEPT `hi` applied;
    /// left←right = ONLY `hi` applied. Writes the target file and re-diffs.
    pub(crate) fn compare_apply_hunk(&mut self, hi: usize, to_right: bool, cx: &mut Context<Self>) {
        let Some(d) = self.compare.diff.as_ref() else {
            return;
        };
        let text = if to_right {
            d.partial_new_content(|j| j != hi)
        } else {
            d.partial_new_content(|j| j == hi)
        };
        self.compare_write_side(to_right, &text, cx);
    }

    /// Make one whole side match the other (the header's `»`/`«` All buttons).
    pub(crate) fn compare_apply_all(&mut self, to_right: bool, cx: &mut Context<Self>) {
        let Some(d) = self.compare.diff.as_ref() else {
            return;
        };
        if d.hunks.is_empty() {
            return;
        }
        let text = if to_right {
            d.partial_new_content(|_| false) // right becomes the left text
        } else {
            d.partial_new_content(|_| true) // left becomes the right text
        };
        self.compare_write_side(to_right, &text, cx);
    }

    /// Persist an apply's result to the target side's file, refresh the open
    /// editor if it shows that file (never clobbering unsaved edits), and
    /// re-diff the panes in place.
    fn compare_write_side(&mut self, to_right: bool, text: &str, cx: &mut Context<Self>) {
        let target = if to_right {
            self.compare.right_path.clone()
        } else {
            self.compare.left_path.clone()
        };
        let Some(target) = target else { return };
        // Local history: the overwritten side is recoverable.
        self.lh_snapshot_now(vec![target.clone()], "Before compare apply", cx);
        if let Err(e) = self.write_open_file(&target, text) {
            self.fail("Compare apply", e);
            cx.notify();
            return;
        }
        // The applied file may be open in Browse — reload it unless it has
        // unsaved edits (the reload_external never-clobber rule).
        if self.browse.open_path.as_ref() == Some(&target) && !self.browse.editor.read(cx).dirty {
            let lang = self.effective_lang(&target);
            let content = text.to_string();
            self.browse
                .editor
                .update(cx, |e, cx| e.set_content(content, lang, cx));
        }
        self.reload_compare(false, cx);
        self.refresh(cx); // the write changed git status
    }

    /// The Compare window body: header (file names + whole-file apply buttons +
    /// difference count) over the two aligned panes and the center apply gutter.
    pub(crate) fn render_compare_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let ui = theme::font::UI_FAMILY;
        let fs = px(t.editor_font_size);
        let row_h = px(editor::line_height_px());
        let Some(d) = self.compare.diff.as_ref() else {
            return div().into_any_element();
        };

        // Per-hunk gutter controls at each hunk's first aligned display row.
        let (n_rows, hunk_rows) = hunk_starts(d);
        let total_h = row_h * n_rows as f32;
        let ctl = |id: String,
                   glyph: &'static str,
                   tip: &'static str,
                   hi: usize,
                   to_right: bool,
                   cx: &mut Context<Self>| {
            ui::diff::gutter_glyph(SharedString::from(id), glyph, t.line_number)
                .tooltip(ui::tip(tip))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.compare_apply_hunk(hi, to_right, cx);
                    }),
                )
        };
        let mut ctls: Vec<gpui::AnyElement> = Vec::new();
        for &(row, hi) in &hunk_rows {
            ctls.push(
                ui::diff::gutter_row(row, row_h)
                    .child(ctl(
                        format!("cmp-left-{hi}"),
                        "«",
                        "Copy this change to the left file",
                        hi,
                        false,
                        cx,
                    ))
                    .child(ctl(
                        format!("cmp-right-{hi}"),
                        "»",
                        "Copy this change to the right file",
                        hi,
                        true,
                        cx,
                    ))
                    .into_any_element(),
            );
        }
        let gutter = ui::diff::gutter_column(
            "compare-gutter",
            ui::diff::GUTTER_W,
            total_h,
            self.compare.scroll.offset().y,
            ctls,
        );

        // Panes: shared scroll, content width for horizontal overflow.
        let lw = self.compare.left.read(cx).content_width();
        let rw = self.compare.right.read(cx).content_width();
        let pane = |id: &'static str, w: f32, ed: gpui::AnyElement, scroll: &ScrollHandle| {
            ui::diff::editor_pane(id, w, ed, scroll, fs)
        };
        let scroll = self.compare.scroll.clone();

        // Header: file names over their panes, whole-file apply + diff count in
        // the middle over the gutter column.
        let path_label = |p: Option<&PathBuf>| {
            p.map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let n = d.hunks.len();
        let count = if n == 0 {
            "No differences".to_string()
        } else {
            format!("{n} difference{}", if n == 1 { "" } else { "s" })
        };
        let all = |id: &'static str,
                   glyph: &'static str,
                   tip: &'static str,
                   to_right: bool,
                   cx: &mut Context<Self>| {
            let tip = SharedString::from(tip);
            div()
                .id(id)
                .px_2()
                .py_0p5()
                .rounded_sm()
                .border_1()
                .border_color(t.divider)
                .text_color(t.secondary_text)
                .hover(|s| s.bg(t.bg_light).text_color(t.primary))
                .cursor_pointer()
                .tooltip(ui::tip(tip))
                .child(glyph)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        this.compare_apply_all(to_right, cx);
                    }),
                )
        };
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .font_family(ui)
            .text_size(px(t.ui_font_size))
            .border_b(px(1.0))
            .border_color(t.divider)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(t.text)
                    .child(SharedString::from(path_label(
                        self.compare.left_path.as_ref(),
                    ))),
            )
            .child(all(
                "cmp-all-left",
                "«",
                "Copy ALL changes to the left file",
                false,
                cx,
            ))
            .child(
                div()
                    .flex_none()
                    .text_color(t.secondary_text)
                    .child(SharedString::from(count)),
            )
            .child(all(
                "cmp-all-right",
                "»",
                "Copy ALL changes to the right file",
                true,
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_right()
                    .text_color(t.text)
                    .child(SharedString::from(path_label(
                        self.compare.right_path.as_ref(),
                    ))),
            );

        div()
            .size_full()
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
                        "compare-left",
                        lw,
                        self.compare.left.clone().into_any_element(),
                        &scroll,
                    ))
                    .child(gutter)
                    .child(pane(
                        "compare-right",
                        rw,
                        self.compare.right.clone().into_any_element(),
                        &scroll,
                    )),
            )
            .into_any_element()
    }
}
