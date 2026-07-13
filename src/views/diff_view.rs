//! Side-by-side diff: the two-pane editor, center-gutter staging, hunk navigation, the
//! full-screen Show-Diff view + the Diff modal body. Crate-root child module.

use crate::*;

impl Kyde {
    /// Side-by-side diff = two editors in one rounded island: left is the read-only base
    /// (HEAD/index), right is the editable working copy (live-saved). A draggable divider
    /// sets the 50/50 split. Both syntax-highlight when the language pack is installed.
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
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(header);
        // The find/replace bar (⌘F / ⌘R), targeting whichever diff pane is focused.
        if self.find.open {
            col = col.child(self.render_find_bar(ui, cx));
        }
        col.child(self.render_diff(ui, fs, Some(window), cx))
            .into_any_element()
    }

    /// IntelliJ-style side-by-side diff: aligned rows, with a center gutter showing the old
    /// and new line numbers, a `»` chevron (revert the hunk) and a checkbox (stage it).
    pub(crate) fn render_diff(
        &mut self,
        _ui: &'static str,
        fs: gpui::Pixels,
        mut window: Option<&mut Window>,
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

        // The Diff modal window renders the same `diff_left`/`diff_right` editors; rendering one
        // editor entity in two windows desyncs scroll + garbles layout. So while the modal is
        // open, the INLINE diff (the only caller that passes `Some(window)`) yields the panes to
        // it and shows a placeholder. The modal itself passes `None`, so it renders normally.
        if window.is_some() && self.diff_modal_open {
            return island()
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.line_number)
                .child("Viewing diff in window…")
                .into_any_element();
        }

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
        // `diff_scroll` so it tracks the editors. Chevrons are positioned ABSOLUTELY at
        // `row * row_h` inside a fixed-height column — a flex column of per-row divs let
        // empty rows collapse (gpui ignores `.h()` on a childless div), which bunched every
        // chevron toward the top instead of onto its hunk row.
        let row_h = px(editor::line_height_px());
        let rows = aligned_rows(d);
        let total_h = row_h * rows.len() as f32;
        // Read-only diffs (push view) show a committed change with no working-tree edit,
        // so there's nothing to revert — drop the gutter chevrons.
        let chevrons: Vec<gpui::AnyElement> = if self.diff.readonly {
            Vec::new()
        } else {
            rows.iter()
                .enumerate()
                .filter_map(|(i, r)| {
                    let hi = r.hunk_start.then_some(r.hunk).flatten()?;
                    Some(
                        // Position the row's line box exactly over the editor line (`line_height`
                        // = `row_h`), so the `»` baselines with the line's text instead of being
                        // half-centered a few px high.
                        div()
                            .absolute()
                            .top(row_h * i as f32)
                            .left_0()
                            .right_0()
                            .h(row_h)
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .child(
                                // Flex-center the glyph in a fixed box so it's truly centered
                                // (a line-height hack left it sitting low).
                                div()
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
                                    ),
                            )
                            .into_any_element(),
                    )
                })
                .collect()
        };

        // Pane = shared VERTICAL scroll (`diff_scroll`, keeps the two sides' rows aligned)
        // wrapping an INDEPENDENT horizontal scroll around a content-width editor, so long
        // lines scroll sideways per pane without breaking row alignment.
        let frac = self.diff.split.clamp(0.15, 0.85);
        let lw = self.diff.left.read(cx).content_width();
        let rw = self.diff.right.read(cx).content_width();
        let pane_scroll =
            |id: &'static str, scroll: &ScrollHandle, w: f32, editor: gpui::AnyElement| {
                div()
                    .id(id)
                    .h_full()
                    .w_full()
                    .overflow_scroll()
                    .track_scroll(scroll)
                    .child(div().w(px(w)).child(editor))
            };
        let left_inner = pane_scroll(
            "diff-left-scroll",
            &self.diff.scroll,
            lw,
            self.diff.left.clone().into_any_element(),
        );
        let right_inner = pane_scroll(
            "diff-right-scroll",
            &self.diff.scroll,
            rw,
            self.diff.right.clone().into_any_element(),
        );
        // One shared horizontal scrollbar driven by `diff_scroll`; its travel is the wider
        // pane's content. Placed on the island (full width) below.
        let h_bar = self.diff_hscrollbar(
            &self.diff.scroll.clone(),
            lw.max(rw),
            SbView::DiffLeftH,
            window.as_deref_mut(),
            cx,
        );
        // New file (empty left) or deleted file (empty right): show ONLY the populated side,
        // full-width. A side-by-side with one empty pane is noise — and the empty pane drives
        // the shared scroll handle's bounds to ~0, which blanks the viewport-culled editor on a
        // large file. Full-width, the surviving editor owns the layout and paints normally.
        let left_empty = self.diff.left.read(cx).text().is_empty();
        let right_empty = self.diff.right.read(cx).text().is_empty();
        if left_empty != right_empty {
            let inner = if left_empty { right_inner } else { left_inner };
            let scrollbar = self.diff_vscrollbar(total_h, window.as_deref_mut(), cx);
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

        // Left pane width: when we know the viewport (the inline diff, the only resizable
        // caller — the modal passes `None`), size it to an explicit pixel width from the SAME
        // formula the resize handler inverts, and let the right pane flex to fill the rest. The
        // modal falls back to proportional flex_basis (its divider isn't dragged through the
        // main-window handler). Both keep `min_w_0` so neither pane can be pushed off-window.
        let left_px = window.as_deref().map(|w| {
            (frac * (full_island_w(f32::from(w.viewport_size().width)) - DIFF_GUTTER_W)).max(40.0)
        });
        let left = div().relative().min_w_0().h_full();
        let left = match left_px {
            Some(w) => left.w(px(w)).flex_none(),
            None => left.flex_basis(gpui::relative(frac)).flex_shrink(),
        }
        .child(left_inner);
        let right = div().relative().min_w_0().h_full();
        let right = match left_px {
            Some(_) => right.flex_1(),
            None => right.flex_basis(gpui::relative(1.0 - frac)).flex_shrink(),
        }
        .child(right_inner);
        // The gutter (chevrons) shares the editors' vertical scroll by translating its content
        // by the SAME offset; it also doubles as the draggable divider (drag to resize the
        // split). Clicks on a `»` still revert their hunk (chevrons are children).
        let scroll_y = self.diff.scroll.offset().y;
        let gutter = div()
            .id("diff-gutter")
            .w(px(DIFF_GUTTER_W))
            .flex_none()
            .h_full()
            .overflow_hidden()
            .bg(t.diff_separator_bg)
            // Thin divider lines on both edges so the center gutter reads as the
            // pane separator (otherwise it blends into the panes at most font sizes).
            .border_l(px(1.0))
            .border_r(px(1.0))
            .border_color(t.divider)
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                    this.start_divider_drag(Divider::DiffPane, e.position, window);
                    cx.notify();
                }),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(total_h)
                    .top(scroll_y)
                    .children(chevrons),
            );

        // Single vertical scrollbar overlaid on the right edge of the island; both panes
        // share `diff_scroll`, so one bar drives the whole diff. `total_h` is the exact
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

    /// A small floating control at the diff's top-right: the change count + prev/next arrows
    /// that jump between hunks. Shown whenever the current diff has at least one change.
    fn render_diff_nav(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let n = self.diff.current.as_ref().map_or(0, |d| d.hunks.len());
        // Nothing to navigate with a single (or zero) change — hide the control.
        if n < 2 {
            return None;
        }
        let t = theme::get();
        let ui = theme::font::UI_FAMILY;
        let label = format!("{n} changes");
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
                .tooltip(move |_w, cx| cx.new(|_| Tip(tip.into())).into())
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
                .child(arrow(
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
                .into_any_element(),
        )
    }

    /// A vertical scrollbar thumb overlaid on the right edge of the diff island, driven by
    /// `diff_scroll` (both panes share it). Returns `None` when the diff fits. Absolutely
    /// positioned — unlike `with_scrollbars` it doesn't need a concrete pane width, so it
    /// works over the diff's flex 50/50 split. `window` (when `Some`, i.e. the inline commit
    /// view) requests one settle frame so the bar appears on first paint, before any scroll;
    /// the modal diff passes `None` and the bar shows after the first scroll/repaint.
    fn diff_vscrollbar(
        &mut self,
        total_h: gpui::Pixels,
        window: Option<&mut Window>,
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
        if let Some(window) = window {
            let dims: ScrollDims = (px(0.0), px(0.0), vp_h, px(0.0), total_h);
            if self.scroll_dims.get(&SbView::Diff) != Some(&dims) {
                self.scroll_dims.insert(SbView::Diff, dims);
                let entity = cx.entity();
                window.on_next_frame(move |_, cx| entity.update(cx, |_, cx| cx.notify()));
            }
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

    /// Horizontal scrollbar overlaid at the bottom of one diff pane, driven by `scroll` (that
    /// pane's independent horizontal handle) and `content_w` (its widest line). Returns `None`
    /// when the lines fit. The caller overlays it on a `relative` pane wrapper.
    fn diff_hscrollbar(
        &mut self,
        scroll: &ScrollHandle,
        content_w: f32,
        view: SbView,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let t = theme::get();
        let vp_w = scroll.bounds().size.width;
        let off = scroll.offset();
        let max_scroll = px(content_w) - vp_w;
        const BAR: f32 = 12.0;
        if let Some(window) = window {
            let dims: ScrollDims = (vp_w, px(content_w), px(0.0), px(0.0), px(0.0));
            if self.scroll_dims.get(&view) != Some(&dims) {
                self.scroll_dims.insert(view, dims);
                let entity = cx.entity();
                window.on_next_frame(move |_, cx| entity.update(cx, |_, cx| cx.notify()));
            }
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
        let id = if matches!(view, SbView::DiffLeftH) {
            "diff-sb-h-l"
        } else {
            "diff-sb-h-r"
        };
        let thumb = div()
            .id(id)
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

    /// Floating "Show Diff" viewer over the Commit view (IntelliJ-style).
    /// Show-Diff window body (own native window; titlebar shows the file path). Just the
    /// side-by-side diff filling the window.
    pub(crate) fn render_diff_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = theme::font::UI_FAMILY;
        let fs = px(theme::get().editor_font_size);
        let t = theme::get();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.main_bg)
            .font_family(ui)
            .text_size(fs)
            .text_color(t.text)
            .child(self.render_diff(ui, fs, None, cx))
            .into_any_element()
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

    /// Compute the `before`→`after` diff, store it (`current_diff`/`diff_base`/`diff_path`),
    /// highlight both sides, and load both panes — content + decorations + shared scroll —
    /// opening scrolled to the first hunk (a few lines of context above). The left (base)
    /// pane is always read-only; `readonly` locks the right pane too (committed/push diffs)
    /// and is mirrored into `diff_readonly`. Shared by `select_with` (editable, `false`) and
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
        self.diff.old_spans = highlight::highlight(&before, lang);
        self.diff.new_spans = highlight::highlight(&after, lang);
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
        // Both panes scroll via the shared `diff_scroll`, so caret-follow / drag auto-scroll
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

    /// Re-diff the working text against the cached base and push backgrounds/filler/spans
    /// onto both panes. Shared by live autosave and the `»` revert.
    fn recompute_diff(&mut self, text: &str, cx: &mut Context<Self>) {
        let d = FileDiff::compute(&self.diff.base, text);
        let lang = self
            .diff
            .path
            .clone()
            .map_or(Lang::PlainText, |p| self.effective_lang(&p));
        self.diff.old_spans = highlight::highlight(&self.diff.base, lang);
        self.diff.new_spans = highlight::highlight(text, lang);
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
        crate::aligned_rows(d)
            .iter()
            .enumerate()
            .filter(|(_, r)| r.hunk_start)
            .map(|(i, _)| i)
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
        // `select_with(.., Some(cx))` — not `select` — so the diff editors + `diff_path`
        // actually populate (plain `select` only updates `current_diff`).
        self.select_with(idx, Some(cx));
        self.context_menu = None;
        // Show the diff full-screen in the MAIN window (reuses every editor feature). Close the
        // Rollback window we were launched from so the main window comes forward.
        self.diff_view_open = true;
        self.close_modal_window(ModalKind::Rollback, cx);
        cx.notify();
    }
}
