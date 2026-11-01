//! Side-by-side diff: the two-pane editor, center-gutter staging, hunk navigation, and the
//! full-screen Show-Diff view. Crate-root child module.

use crate::*;

impl Kyde {
    /// Full-screen Show-Diff view in the MAIN window (rollback / push diff). A header with a
    /// Back button + the file path over the inline `render_diff`, so it reuses every editor
    /// feature — find (⌘F), divider drag, scrollbars, change navigation. Escape / Back exits.
    pub(crate) fn render_diff_view(
        &mut self,
        ui: &'static str,
        fs: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let title: SharedString = self
            .diff
            .path
            .as_ref()
            .map_or_else(|| "Diff".to_string(), |p| p.to_string_lossy().into_owned())
            .into();
        let back = div()
            .id("diff-back")
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_0p5()
            .rounded_md()
            .cursor_pointer()
            .text_color(t.secondary_text)
            .hover(|s| s.bg(t.bg_mid).text_color(t.text))
            .child("‹ Back")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.close_diff_view(cx)),
            );
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .flex_none()
            .px_1()
            .pb(px(theme::FRAME_GAP))
            .font_family(ui)
            .text_size(px(t.ui_font_size))
            .child(back)
            .child(div().min_w_0().truncate().text_color(t.text).child(title));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(header)
            .child(self.render_diff_with_find(ui, fs, window, cx))
            .into_any_element()
    }

    /// The inline diff with the install banner (when its file's language pack isn't installed)
    /// over it, and the find/replace bar (⌘F / ⌘R, while it's searching one of the panes)
    /// floated at its top right. Every main-window view showing the diff renders it through
    /// here.
    pub(crate) fn render_diff_with_find(
        &mut self,
        ui: &'static str,
        fs: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let diff = self.render_diff(fs, window, cx);
        let banner = self
            .diff_pending_pack()
            .zip(self.diff.path.clone())
            .map(|(pack, path)| self.render_install_banner(pack, &path, true, ui, fs, cx));
        let bar = self.find_bar(FindHost::Main, cx);
        if banner.is_none() && bar.is_none() {
            return diff;
        }
        let diff = match bar {
            Some(bar) => div()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .child(diff)
                .child(bar)
                .into_any_element(),
            None => diff,
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .children(banner)
            .child(diff)
            .into_any_element()
    }

    /// IntelliJ-style side-by-side diff = two editors in one rounded island: left is the
    /// read-only base (HEAD/index), right is the editable working copy (live-saved), aligned
    /// row for row. The center gutter carries a `»` chevron (revert the hunk) and a checkbox
    /// (stage it) and doubles as the draggable split divider. Both panes syntax-highlight
    /// when the language pack is installed.
    pub(crate) fn render_diff(
        &mut self,
        fs: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let island = || {
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .bg(t.main_bg)
                .rounded(px(theme::ISLAND_RADIUS))
                .overflow_hidden()
                .font_family(theme::font::FAMILY)
                .text_size(fs)
                .text_color(t.text)
        };

        // Image file selected → preview it centered + scaled (same as Browse), not a text diff.
        if let Some(rel) = self.diff.image.clone() {
            let abs = self.repo_root.as_ref().map(|r| r.join(&rel)).unwrap_or(rel);
            return island()
                .id("diff-image-scroll")
                .overflow_scroll()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .child(img(abs).max_w_full().max_h_full())
                .into_any_element();
        }

        let Some(d) = self
            .diff
            .current
            .as_ref()
            .filter(|_| self.diff.path.is_some())
        else {
            return island()
                .flex()
                .justify_center()
                .items_center()
                .text_color(t.line_number)
                .child("Select a file")
                .into_any_element();
        };

        // Center gutter: a `»` (revert this hunk) on each hunk's first row, sharing
        // `diff.scroll` so it tracks the editors. Chevrons are positioned ABSOLUTELY at
        // `row * row_h` inside a fixed-height column — a flex column of per-row divs let
        // empty rows collapse (gpui ignores `.h()` on a childless div), which bunched every
        // chevron toward the top instead of onto its hunk row.
        let row_h = px(editor::line_height_px());
        let (n_rows, starts) = hunk_starts(d);
        let total_h = row_h * n_rows as f32;
        // Read-only diffs (push view) show a committed change with no working-tree edit,
        // so there's nothing to revert — drop the gutter chevrons.
        let chevrons: Vec<gpui::AnyElement> = if self.diff.readonly {
            Vec::new()
        } else {
            // Include-in-commit checkboxes only make sense for a working change (the file is
            // in `self.files`) — not e.g. a Show-Diff opened outside the commit flow.
            let staging = self
                .diff
                .path
                .as_ref()
                .is_some_and(|p| self.files.iter().any(|f| &f.path == p));
            let excluded = self
                .diff
                .path
                .as_ref()
                .and_then(|p| self.commit.excluded_hunks.get(p))
                .cloned()
                .unwrap_or_default();
            starts
                .iter()
                .map(|&(i, hi)| {
                    // Flex-center the glyph in a fixed box so it's truly centered
                    // (a line-height hack left it sitting low).
                    let revert = div()
                        .id(SharedString::from(format!("revert-{hi}")))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(24.0))
                        .rounded_sm()
                        .text_size(px(20.0))
                        .line_height(px(20.0))
                        .text_color(t.line_number)
                        .hover(|s| s.bg(t.bg_light).text_color(t.primary))
                        .cursor_pointer()
                        .child("»")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                this.diff_revert_hunk(hi, cx);
                            }),
                        );
                    // Checkbox = include this hunk in the commit (IntelliJ-style partial
                    // commit); unticked hunks stay on disk but out of the commit.
                    let include = staging.then(|| {
                        div()
                            .id(SharedString::from(format!("include-{hi}")))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(24.0))
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|s| s.bg(t.bg_light))
                            .tooltip(ui::tip("Include this change in the commit"))
                            .child(checkbox_box(!excluded.contains(&hi)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    cx.stop_propagation();
                                    this.toggle_hunk_included(hi, cx);
                                }),
                            )
                    });
                    // Position the row's line box exactly over the editor line (`line_height`
                    // = `row_h`), so the `»` baselines with the line's text instead of being
                    // half-centered a few px high.
                    ui::diff::gutter_row(i, row_h)
                        .children(include)
                        .child(revert)
                        .into_any_element()
                })
                .collect()
        };

        // Each pane scrolls a content-width editor through the ONE shared 2D `diff.scroll`
        // handle, so the two sides stay aligned row for row and column for column.
        let frac = self.diff.split.clamp(0.15, 0.85);
        let lw = self.diff.left.read(cx).content_width();
        let rw = self.diff.right.read(cx).content_width();
        let left_inner = ui::diff::pane_scroller(
            "diff-left-scroll",
            lw,
            self.diff.left.clone().into_any_element(),
            &self.diff.scroll,
        );
        let right_inner = ui::diff::pane_scroller(
            "diff-right-scroll",
            rw,
            self.diff.right.clone().into_any_element(),
            &self.diff.scroll,
        );
        // One shared horizontal scrollbar driven by `diff.scroll`; its travel is the wider
        // pane's content. Placed on the island (full width) below.
        let h_bar = self.diff_hscrollbar(lw.max(rw), window, cx);
        // New file (empty left) or deleted file (empty right): show ONLY the populated side,
        // full-width. A side-by-side with one empty pane is noise — and the empty pane drives
        // the shared scroll handle's bounds to ~0, which blanks the viewport-culled editor on a
        // large file. Full-width, the surviving editor owns the layout and paints normally.
        let left_empty = self.diff.left.read(cx).text().is_empty();
        let right_empty = self.diff.right.read(cx).text().is_empty();
        if left_empty != right_empty {
            let inner = if left_empty { right_inner } else { left_inner };
            let scrollbar = self.diff_vscrollbar(total_h, window, cx);
            return island()
                .relative()
                .flex()
                .flex_row()
                .child(div().relative().flex_1().min_w_0().h_full().child(inner))
                .children(h_bar)
                .children(scrollbar)
                .children(self.render_diff_nav(cx))
                .into_any_element();
        }

        // Left pane width: an explicit pixel width from the SAME formula the resize handler
        // inverts; the right pane flexes to fill the rest. Both keep `min_w_0` so neither pane
        // can be pushed off-window.
        let left_px = (frac
            * (full_island_w(f32::from(window.viewport_size().width)) - DIFF_GUTTER_W))
            .max(40.0);
        let left = div()
            .relative()
            .min_w_0()
            .h_full()
            .w(px(left_px))
            .flex_none()
            .child(left_inner);
        let right = div()
            .relative()
            .min_w_0()
            .h_full()
            .flex_1()
            .child(right_inner);
        // The gutter (chevrons) shares the editors' vertical scroll by translating its content
        // by the SAME offset; it also doubles as the draggable divider (drag to resize the
        // split). Clicks on a `»` still revert their hunk (chevrons are children).
        let gutter = ui::diff::gutter_column(
            "diff-gutter",
            DIFF_GUTTER_W,
            total_h,
            self.diff.scroll.offset().y,
            chevrons,
        )
        .cursor_col_resize()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                this.start_divider_drag(Divider::DiffPane, e.position, window);
                cx.notify();
            }),
        );

        // Single vertical scrollbar overlaid on the right edge of the island; both panes
        // share `diff.scroll`, so one bar drives the whole diff. `total_h` is the exact
        // aligned-row content height (both panes are padded to it), so the bar is driven by
        // that rather than the shared handle's `max_offset` — which reflects whichever pane
        // painted last and is ~0 when one side is empty (e.g. an all-added/untracked file).
        let scrollbar = self.diff_vscrollbar(total_h, window, cx);

        island()
            .relative()
            .flex()
            .flex_row()
            .child(left)
            .child(gutter)
            .child(right)
            .children(h_bar)
            .children(scrollbar)
            .children(self.render_diff_nav(cx))
            .into_any_element()
    }

    /// A small floating pill at the diff's top-right: the change count + the file's `+a −r`
    /// line stats, plus prev/next arrows that jump between hunks (only when there are 2+ to
    /// jump between). Shown whenever the current diff has at least one change.
    fn render_diff_nav(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let n = self.diff.current.as_ref().map_or(0, |d| d.hunks.len());
        if n == 0 {
            return None;
        }
        let nav = n >= 2;
        let t = theme::get();
        let ui = theme::font::UI_FAMILY;
        let label = if n == 1 {
            "1 change".to_string()
        } else {
            format!("{n} changes")
        };
        let stats = self.diff.current.as_ref().and_then(|d| {
            let (a, r) = d.stats();
            crate::ui::line_stats(a, r)
        });
        let arrow = |id: &'static str, icon: &'static str, tip: &'static str, next: bool| {
            div()
                .id(id)
                .size(px(20.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(t.bg_light))
                .tooltip(ui::tip(tip))
                .child(svg().path(icon).size(px(14.0)).text_color(t.secondary_text))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.diff_nav_hunk(next, cx);
                    }),
                )
        };
        Some(
            div()
                .absolute()
                .top(px(8.0))
                // Clear the 12px vertical scrollbar gutter on the right edge.
                .right(px(16.0))
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                // Swallow clicks on the control (label + padding included) so they don't reach
                // the editor underneath and start a text selection.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .rounded_md()
                .border_1()
                .border_color(t.divider)
                .bg(t.bg_mid)
                .pl_2()
                .pr_1()
                .py_0p5()
                .font_family(ui)
                .text_size(px(t.ui_font_size))
                .text_color(t.secondary_text)
                .child(div().mr_1().child(label))
                .when_some(stats, |d, s| d.child(div().mr_1().child(s)))
                .when(nav, |d| {
                    d.child(arrow(
                        "diff-prev",
                        "icons/arrow-up.svg",
                        "Previous change",
                        false,
                    ))
                    .child(arrow(
                        "diff-next",
                        "icons/arrow-down.svg",
                        "Next change",
                        true,
                    ))
                })
                .into_any_element(),
        )
    }

    /// A vertical scrollbar thumb overlaid on the right edge of the diff island, driven by
    /// `diff.scroll` (both panes share it). Returns `None` when the diff fits. Absolutely
    /// positioned — unlike `with_scrollbars` it doesn't need a concrete pane width, so it
    /// works over the diff's split. Requests one settle frame when the painted dims change,
    /// so the bar appears on first paint, before any scroll.
    fn diff_vscrollbar(
        &mut self,
        total_h: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let t = theme::get();
        let scroll = self.diff.scroll.clone();
        // Viewport height is reliable from the shared handle (both panes are `h_full` in the
        // same island, so they paint the same visible height). The scroll *distance* is the
        // content height beyond the viewport — computed from `total_h`, not `max_offset`.
        let vp_h = scroll.bounds().size.height;
        let off = scroll.offset();
        let max_scroll = (total_h - vp_h).max(px(0.0));
        const BAR: f32 = 12.0;

        // Scroll metrics are zero until the panes have painted, so the first frame after a
        // file loads can't know whether a bar is needed. Track the painted dims and ask for
        // one more frame when they change, so the bar settles in without a scroll/resize.
        let dims: ScrollDims = (px(0.0), px(0.0), vp_h, px(0.0), total_h);
        if self.scroll_dims.get(&SbView::Diff) != Some(&dims) {
            self.scroll_dims.insert(SbView::Diff, dims);
            let entity = cx.entity();
            window.on_next_frame(move |_, cx| entity.update(cx, |_, cx| cx.notify()));
        }

        if max_scroll <= px(1.0) {
            return None; // fits — nothing to scroll
        }

        const END: f32 = 8.0;
        const THUMB: f32 = 6.0;
        let (thumb_h, top) = scrollbar_thumb(
            f32::from(vp_h),
            f32::from(max_scroll),
            f32::from(off.y),
            END,
        );
        let m = (BAR - THUMB) / 2.0;
        let sc = scroll.clone();
        let thumb = div()
            .id("diff-sb-v")
            .absolute()
            .top(px(top))
            .left(px(m))
            .w(px(THUMB))
            .h(px(thumb_h))
            .rounded_full()
            .bg(t.line_number)
            .opacity(0.5)
            .hover(|s| s.opacity(0.85))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                    // Don't let the editor/tree underneath also handle this — it would start a
                    // text selection / move the caret behind the scrollbar.
                    cx.stop_propagation();
                    this.sb_drag = Some(crate::SbDrag {
                        handle: sc.clone(),
                        horizontal: false,
                        start_cursor: f32::from(e.position.y),
                        start_off: f32::from(sc.offset().y),
                    });
                    cx.notify();
                }),
            );
        Some(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(BAR))
                // Swallow clicks anywhere in the scrollbar gutter (track included) so they never
                // reach the editor underneath and start a text selection.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(thumb)
                .into_any_element(),
        )
    }

    /// The diff's single horizontal scrollbar, overlaid at the bottom of the island: driven
    /// by the shared 2D `diff.scroll` and `content_w` (the wider pane's widest line). Returns
    /// `None` when the lines fit.
    fn diff_hscrollbar(
        &mut self,
        content_w: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let t = theme::get();
        let scroll = self.diff.scroll.clone();
        let vp_w = scroll.bounds().size.width;
        let off = scroll.offset();
        let max_scroll = px(content_w) - vp_w;
        const BAR: f32 = 12.0;
        let dims: ScrollDims = (vp_w, px(content_w), px(0.0), px(0.0), px(0.0));
        if self.scroll_dims.get(&SbView::DiffH) != Some(&dims) {
            self.scroll_dims.insert(SbView::DiffH, dims);
            let entity = cx.entity();
            window.on_next_frame(move |_, cx| entity.update(cx, |_, cx| cx.notify()));
        }
        if max_scroll <= px(1.0) {
            return None; // lines fit — no horizontal scroll
        }
        const END: f32 = 8.0;
        const THUMB: f32 = 6.0;
        let (thumb_w, left) = scrollbar_thumb(
            f32::from(vp_w),
            f32::from(max_scroll),
            f32::from(off.x),
            END,
        );
        let m = (BAR - THUMB) / 2.0;
        let sc = scroll.clone();
        let thumb = div()
            .id("diff-sb-h")
            .absolute()
            .left(px(left))
            .top(px(m))
            .h(px(THUMB))
            .w(px(thumb_w))
            .rounded_full()
            .bg(t.line_number)
            .opacity(0.5)
            .hover(|s| s.opacity(0.85))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.sb_drag = Some(crate::SbDrag {
                        handle: sc.clone(),
                        horizontal: true,
                        start_cursor: f32::from(e.position.x),
                        start_off: f32::from(sc.offset().x),
                    });
                    cx.notify();
                }),
            );
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(BAR))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(thumb)
                .into_any_element(),
        )
    }

    /// Push the diff `d`'s per-line / word backgrounds + filler onto both panes. Pure
    /// decoration: it never touches content, language, read-only, line numbers, or scroll
    /// — those are owned by `load_diff_panes` on the initial load and intentionally left
    /// alone on a re-diff. Shared by `load_diff_panes`, `recompute_diff`, and the `»` revert.
    fn apply_diff_decorations(&mut self, d: &FileDiff, cx: &mut Context<Self>) {
        let (old_bg, new_bg) = diff_line_bgs(d);
        let (old_words, new_words) = diff_word_bgs(d);
        let (lf, lf_end, rf, rf_end) = diff_fillers(d);
        let t = theme::get();
        self.diff.left.update(cx, |e, _| {
            e.line_bg = old_bg;
            e.word_bg = old_words;
            e.word_bg_color = t.diff_word_old_bg;
            e.filler = lf;
            e.filler_end = lf_end;
        });
        self.diff.right.update(cx, |e, _| {
            e.line_bg = new_bg;
            e.word_bg = new_words;
            e.word_bg_color = t.diff_word_new_bg;
            e.filler = rf;
            e.filler_end = rf_end;
        });
    }

    /// Compute the `before`→`after` diff, store it (`diff.current`/`diff.base`/`diff.path`),
    /// highlight both sides, and load both panes — content + decorations + shared scroll —
    /// opening scrolled to the first hunk (a few lines of context above). The left (base)
    /// pane is always read-only; `readonly` locks the right pane too (committed/push diffs)
    /// and is mirrored into `diff.readonly`. Shared by `select_with` (editable, `false`) and
    /// `push_show_diff` (committed, `true`).
    pub(crate) fn load_diff_panes(
        &mut self,
        path: std::path::PathBuf,
        before: String,
        after: String,
        lang: Lang,
        readonly: bool,
        cx: &mut Context<Self>,
    ) {
        let d = FileDiff::compute(&before, &after);
        // Row of the first change (for the open-at-first-hunk scroll below). The leading
        // region before the first hunk is all-equal, so its display-row count ==
        // the hunk's old_range.start.
        let first_hunk_row = d.hunks.first().map(|h| h.old_range.start);
        self.diff.path = Some(path);
        self.diff.readonly = readonly;
        self.diff.base = before.clone();
        self.apply_diff_decorations(&d, cx);
        self.diff.current = Some(d);
        // Content goes in its own update closure — `set_content` leaves the decoration
        // fields set just above intact. Left is always locked (base); right tracks `readonly`.
        self.diff.left.update(cx, |e, cx| {
            e.read_only = true;
            e.line_numbers = true;
            e.set_content(before, lang, cx);
        });
        self.diff.right.update(cx, |e, cx| {
            e.read_only = readonly;
            e.line_numbers = true;
            e.set_content(after, lang, cx);
        });
        // Both panes scroll via the shared `diff.scroll`, so caret-follow / drag auto-scroll
        // and the first-hunk offset below move both panes + the gutter together.
        let dh = self.diff.scroll.clone();
        self.diff
            .left
            .update(cx, |e, _| e.set_scroll_handle(dh.clone()));
        self.diff.right.update(cx, |e, _| e.set_scroll_handle(dh));
        if let Some(start) = first_hunk_row {
            let row = start.saturating_sub(SCROLL_CONTEXT_ROWS) as f32;
            self.diff
                .scroll
                .set_offset(gpui::point(px(0.0), px(-row * editor::line_height_px())));
        }
    }

    /// Live-save the editable (right) diff pane to disk, then re-diff + recolor.
    pub(crate) fn diff_autosave(&mut self, cx: &mut Context<Self>) {
        let (Some(rel), text) = (
            self.diff.path.clone(),
            self.diff.right.read(cx).text().to_string(),
        ) else {
            return;
        };
        if let Some(repo) = self.repo() {
            // A failed save means the pane's edits never reached disk — surface it (banner)
            // instead of silently re-diffing as if they had.
            if let Err(e) = repo.save_file(&rel, &text) {
                self.fail("Saving file", e);
            }
            self.files = repo.status().unwrap_or_default();
        }
        self.recompute_diff(&text, cx);
    }

    /// Re-diff the working text against the cached base and push backgrounds/filler onto
    /// both panes. Shared by live autosave and the `»` revert.
    fn recompute_diff(&mut self, text: &str, cx: &mut Context<Self>) {
        // An edit shifts hunk indices, so any include-in-commit unticks no longer point at
        // the hunks the user meant — reset the file to fully-included.
        if let Some(p) = &self.diff.path {
            self.commit.excluded_hunks.remove(p);
        }
        let d = FileDiff::compute(&self.diff.base, text);
        self.apply_diff_decorations(&d, cx);
        self.diff.current = Some(d);
        self.rebuild_commit_view(false);
        cx.notify();
    }

    /// `»` in the diff gutter: discard one hunk's working change by replacing its new
    /// lines with the base lines, then save + re-diff. (Clean text op, no `git apply`.)
    pub(crate) fn diff_revert_hunk(&mut self, hi: usize, cx: &mut Context<Self>) {
        let Some(d) = self.diff.current.clone() else {
            return;
        };
        let Some(h) = d.hunks.get(hi) else {
            return;
        };
        // Local history: the discarded hunk stays recoverable.
        if let Some(rel) = self.diff.path.clone() {
            self.lh_snapshot_now(vec![rel], "Before hunk revert", cx);
        }
        let mut lines = d.new.clone();
        let replacement = d.old[h.old_range.clone()].to_vec();
        lines.splice(h.new_range.clone(), replacement);
        let content = lines.join("\n");
        let lang = self
            .diff
            .path
            .clone()
            .map_or(Lang::PlainText, |p| self.effective_lang(&p));
        self.diff
            .right
            .update(cx, |e, cx| e.set_content(content.clone(), lang, cx));
        if let (Some(rel), Some(repo)) = (self.diff.path.clone(), self.repo()) {
            // A failed save means the revert never reached disk — surface it (banner).
            if let Err(e) = repo.save_file(&rel, &content) {
                self.fail("Reverting hunk", e);
            }
            self.files = repo.status().unwrap_or_default();
        }
        self.recompute_diff(&content, cx);
        self.exit_commit_if_clean();
    }

    /// Alt+↓ — jump to the next changed region in the diff.
    pub(crate) fn act_diff_next(
        &mut self,
        _: &crate::DiffNextChange,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_nav_hunk(true, cx);
    }

    /// Alt+↑ — jump to the previous changed region in the diff.
    pub(crate) fn act_diff_prev(
        &mut self,
        _: &crate::DiffPrevChange,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_nav_hunk(false, cx);
    }

    /// Display-row index where each hunk begins (in the aligned two-pane layout). Drives the
    /// diff's change count + the prev/next navigation.
    fn diff_hunk_rows(&self) -> Vec<usize> {
        let Some(d) = self.diff.current.as_ref() else {
            return Vec::new();
        };
        crate::hunk_starts(d)
            .1
            .into_iter()
            .map(|(row, _)| row)
            .collect()
    }

    /// Jump the diff to the next (`next`) or previous changed region, wrapping around. The
    /// anchor is the hunk currently scrolled to the top (`load_diff_panes` parks each hunk
    /// `SCROLL_CONTEXT_ROWS` below the viewport top, so we add that back).
    pub(crate) fn diff_nav_hunk(&mut self, next: bool, cx: &mut Context<Self>) {
        let rows = self.diff_hunk_rows();
        if rows.is_empty() {
            return;
        }
        let lh = editor::line_height_px();
        let top = (-f32::from(self.diff.scroll.offset().y) / lh).round() as i64;
        let anchor = top + SCROLL_CONTEXT_ROWS as i64;
        // `rows` is non-empty (checked above), so first/last are valid by construction — no
        // `.unwrap()` needed (rule: no unwrap in non-test code).
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
        self.diff
            .scroll
            .set_offset(gpui::point(gpui::px(0.0), gpui::px(-row * lh)));
        cx.notify();
    }

    /// Empty the diff panes (both sides + cached diff/path) — used when a tab has no file to
    /// show, so a stale file doesn't linger from the other tab.
    pub(crate) fn clear_diff_panes(&mut self, cx: &mut Context<Self>) {
        self.diff.path = None;
        self.diff.current = None;
        self.diff.left.update(cx, |e, cx| {
            e.set_content(String::new(), Lang::PlainText, cx);
        });
        self.diff.right.update(cx, |e, cx| {
            e.set_content(String::new(), Lang::PlainText, cx);
        });
    }

    /// Leave the full-screen Show-Diff view, back to whatever mode the main window was in.
    pub(crate) fn close_diff_view(&mut self, cx: &mut Context<Self>) {
        self.diff_view_open = false;
        cx.notify();
    }

    /// Commit → "Show Diff": open the floating diff viewer for that changed file.
    pub(crate) fn menu_show_diff(&mut self, idx: usize, cx: &mut Context<Self>) {
        // `select_with(.., Some(cx))` — not `select` — so the diff editors + `diff.path`
        // actually populate (plain `select` only updates `diff.current`).
        self.select_with(idx, Some(cx));
        self.context_menu = None;
        // Show the diff full-screen in the MAIN window (reuses every editor feature). Close the
        // Rollback window we were launched from so the main window comes forward.
        self.diff_view_open = true;
        self.close_modal_window(ModalKind::Rollback, cx);
        cx.notify();
    }
}
