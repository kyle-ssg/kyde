//! `EditorElement` — the custom gpui `Element` that lays out, shapes, and paints a
//! `CodeEditor`. Split out of the entity module (`super`) so the ~700-line paint half
//! lives on its own. As a child module it reaches `CodeEditor`'s private state directly.
use super::*;

// ── the painting element ──────────────────────────────────────────
pub(super) struct EditorElement {
    pub(super) editor: Entity<CodeEditor>,
}

pub(super) struct Prepaint {
    /// Shaped lines, keyed by display row — sparse, only on-screen rows (+overscan) present.
    lines: HashMap<usize, ShapedLine>,
    /// Total display rows (dense), since `lines` is sparse.
    display_rows: usize,
    /// Byte offset of each display row's line start (display-indexed, dense).
    line_starts: Vec<usize>,
    /// Display row → buffer line index.
    visible_lines: Vec<usize>,
    /// Fold chevrons to draw: `(display_row, is_folded)`.
    gutter: Vec<(usize, bool)>,
    gutter_w: Pixels,
    /// Width of the line-number sub-column within `gutter_w` (0 if no numbers).
    num_w: Pixels,
    /// Gutter rendered on the right of the text (diff base pane) instead of the left.
    gutter_right: bool,
    caret: Option<gpui::PaintQuad>,
    /// Display row of the caret, for the current-line background highlight.
    caret_row: Option<usize>,
    selections: Vec<gpui::PaintQuad>,
    line_height: Pixels,
    /// Line-number gutter hitbox (created in prepaint) → default arrow cursor in paint.
    gutter_hitbox: Option<gpui::Hitbox>,
    /// Fold-chevron column hitbox → pointer cursor in paint (it's clickable).
    fold_hitbox: Option<gpui::Hitbox>,
}

impl IntoElement for EditorElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let ed = self.editor.read(cx);
        let filler: usize = ed.filler.values().sum::<usize>() + ed.filler_end;
        // Visible row count without re-splitting the whole file: total lines minus any
        // hidden (folded) ones. With nothing folded (the common case) it's just the line
        // count — a cheap newline tally, not a 37k-element Vec.
        let line_count = if ed.content.is_empty() {
            1
        } else {
            ed.content.bytes().filter(|&b| b == b'\n').count() + 1
        };
        let visible = if ed.folded.is_empty() {
            line_count
        } else {
            ed.visible_line_indices().len()
        };
        let lines = (visible + filler).max(1);
        let mut style = Style::default();
        style.size.height = (window.line_height() * lines as f32).into();
        // Fill the parent (scroll viewport) when the file is shorter/narrower than it: the
        // element's painted bounds then cover the whole area, so a click anywhere below the
        // last line still lands in the editor and `offset_for_position` snaps to the last
        // line. Long files keep growing past the viewport (size wins) and scroll normally.
        if ed.single_line {
            style.size.width = relative(1.0).into();
        } else {
            // Fill the width of the parent (an explicit-width wrapper sized to `content_width()`,
            // or the side-by-side markdown pane). A custom element doesn't hold a width wider
            // than its flex container the way a plain div does, so the *wrapper* owns the
            // content width and this element just fills it — that's what makes long lines
            // overflow the scroll viewport and trigger the horizontal scrollbar.
            style.size.width = relative(1.0).into();
            style.min_size.height = relative(1.0).into();
        }
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let editor = self.editor.read(cx);
        let style = window.text_style();
        let default_color: Hsla = theme::get().text.into();
        let font = style.font();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();

        let empty = editor.content.is_empty();
        // Cached on the editor (recomputed only on content change), not per paint.
        let spans: &[highlight::Span] = if empty { &[] } else { &editor.spans };

        // Byte offset of every buffer line start — ONE pass over the content per frame.
        // (Previously the content was `split('\n')`-scanned 3-4× per frame — for a 37k-line
        // file that alone dropped scrolling to ~30fps.)
        let mut all_starts: Vec<usize> = Vec::new();
        if !empty {
            all_starts.push(0);
            for (i, b) in editor.content.bytes().enumerate() {
                if b == b'\n' {
                    all_starts.push(i + 1);
                }
            }
        }
        let line_count = all_starts.len().max(1);

        // Fold gutter only for multi-line buffers that actually have foldable regions.
        let fold_w = if !editor.single_line && !editor.foldable.is_empty() {
            GUTTER_W
        } else {
            px(0.0)
        };
        // Line-number column (diff panes / line-numbered editors).
        let num_w = if editor.line_numbers {
            let digits = line_count.to_string().len().max(2) as f32;
            px(digits * 7.5 + 12.0)
        } else {
            px(0.0)
        };
        let gutter_w = fold_w + num_w;
        // Soft-wrap width: the text area inside the element (minus the gutter, a touch of
        // slack so the caret after the last glyph stays inside). Only used when `soft_wrap`.
        let wrap_w = f32::from(bounds.size.width) - f32::from(gutter_w) - 3.0;

        // `lines` is sparse (display row → shaped line) — only rows inside the on-screen band
        // are inserted. `line_starts`/`visible_lines` stay dense (one entry per display row).
        let mut lines: HashMap<usize, ShapedLine> = HashMap::new();
        let mut line_starts = Vec::new(); // display row → byte start
        let mut visible_lines = Vec::new(); // display row → buffer line
        let mut gutter: Vec<(usize, bool)> = Vec::new(); // (display row, folded) for fold-start rows

        // On-screen band (+ overscan) in element-local Y. Only rows inside it are shaped.
        let clip = window.content_mask().bounds;
        let overscan = line_height * 12.0;
        let vis_top = clip.origin.y - overscan;
        let vis_bottom = clip.origin.y + clip.size.height + overscan;
        let on_screen = |dr: usize| {
            let y = bounds.top() + line_height * dr as f32;
            y + line_height >= vis_top && y <= vis_bottom
        };

        if empty {
            // Placeholder text: dim, no folds, identity buffer-line mapping (always small).
            let mut off = 0usize;
            for (i, line) in editor.placeholder.split('\n').enumerate() {
                line_starts.push(off);
                visible_lines.push(i);
                let runs = vec![TextRun {
                    len: line.len(),
                    font: font.clone(),
                    color: Hsla {
                        a: 0.35,
                        ..default_color
                    },
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }];
                lines.insert(
                    i,
                    window.text_system().shape_line(
                        line.to_string().into(),
                        font_size,
                        &runs,
                        None,
                    ),
                );
                off += line.len() + 1;
            }
        } else {
            // Slice a buffer line out of `content` using the precomputed `all_starts`
            // (no per-line allocation; only the ~visible lines are ever sliced).
            let content = &editor.content;
            let line_at = |b: usize| -> &str {
                let s = all_starts.get(b).copied().unwrap_or(content.len());
                let e = all_starts
                    .get(b + 1)
                    .map(|&n| n - 1)
                    .unwrap_or(content.len());
                content.get(s..e).unwrap_or("")
            };
            // Visible buffer lines (fold-filtered). No folds collapsed → just 0..line_count,
            // built from the cached count instead of re-splitting the whole file.
            let visible_buf: Vec<usize> = if editor.folded.is_empty() {
                (0..line_count).collect()
            } else {
                (0..line_count)
                    .filter(|&l| !is_hidden(&editor.folded, &editor.foldable, l))
                    .collect()
            };
            // Virtualization: this element requests full content height and the parent
            // `overflow_y_scroll` clips it. We only shape (text-layout) rows inside the band;
            // off-screen rows are simply absent from `lines` (a blank), keeping the dense
            // display-row math intact. Big files stay snappy: we never shape — nor allocate a
            // shaped line for — tens of thousands of off-screen rows per frame.
            let push_filler = |line_starts: &mut Vec<usize>,
                               visible_lines: &mut Vec<usize>,
                               n: usize,
                               start: usize| {
                for _ in 0..n {
                    line_starts.push(start);
                    visible_lines.push(usize::MAX); // sentinel = filler (blank, no line number)
                }
            };
            for &b in visible_buf.iter() {
                let start = all_starts.get(b).copied().unwrap_or(0);
                if let Some(&k) = editor.filler.get(&b) {
                    push_filler(&mut line_starts, &mut visible_lines, k, start);
                }
                let line = line_at(b);
                // Soft-wrap: split this buffer line into visual segments that each fit `wrap_w`,
                // emitting one display row per segment. Each row is a normal uniform-height
                // `ShapedLine`, so caret/click/selection (which key off `line_starts[dr]` and a
                // uniform `line_height`) keep working — a wrapped line is just "more rows".
                if editor.soft_wrap && !line.is_empty() && wrap_w > 4.0 {
                    let mut seg = 0usize;
                    while seg < line.len() {
                        let rest = &line[seg..];
                        let m_runs = line_runs(rest, start + seg, spans, &font, default_color);
                        let measured = window.text_system().shape_line(
                            rest.to_string().into(),
                            font_size,
                            &m_runs,
                            None,
                        );
                        let mut take = if f32::from(measured.width) <= wrap_w {
                            rest.len()
                        } else {
                            let i = measured.closest_index_for_x(px(wrap_w)).min(rest.len());
                            // Prefer breaking at the last space in the row (word wrap); fall back
                            // to the char break for a single long word.
                            match rest[..i].rfind(' ') {
                                Some(sp) if sp + 1 < i => sp + 1,
                                _ => i,
                            }
                        };
                        if take == 0 {
                            take = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                        }
                        let seg_str = &rest[..take];
                        let dr = line_starts.len();
                        line_starts.push(start + seg);
                        visible_lines.push(b);
                        if on_screen(dr) {
                            let s_runs =
                                line_runs(seg_str, start + seg, spans, &font, default_color);
                            lines.insert(
                                dr,
                                window.text_system().shape_line(
                                    seg_str.to_string().into(),
                                    font_size,
                                    &s_runs,
                                    None,
                                ),
                            );
                        }
                        seg += take;
                    }
                    continue;
                }
                let dr = line_starts.len();
                line_starts.push(start);
                visible_lines.push(b);
                if editor.is_foldable_start(b) {
                    gutter.push((dr, editor.folded.contains(&b)));
                }
                if on_screen(dr) {
                    let runs = line_runs(line, start, spans, &font, default_color);
                    lines.insert(
                        dr,
                        window.text_system().shape_line(
                            line.to_string().into(),
                            font_size,
                            &runs,
                            None,
                        ),
                    );
                }
                continue;
            }
            // (filler-end rows below the last line are blank → no shaped entries needed)
            for _ in 0..editor.filler_end {
                line_starts.push(content.len());
                visible_lines.push(usize::MAX);
            }
        }
        let display_rows = line_starts.len();
        // Gutter on the left (default) or right (diff base pane). `gutter_x` is the gutter
        // region's left edge; numbers occupy `[gutter_x, gutter_x+num_w]`, fold chevrons the
        // rest; the text sits on the opposite side. A right gutter is pinned to the viewport's
        // right edge (matches the paint phase) so it stays against the center gutter.
        let gutter_right = editor.gutter_right;
        let gutter_x = if gutter_right {
            clip.right() - gutter_w
        } else {
            bounds.left()
        };
        let text_x = if gutter_right {
            bounds.left()
        } else {
            bounds.left() + gutter_w
        };
        // Reserve a hitbox over the line-number column so paint can give it the default arrow
        // cursor (the editor body is I-beam). `insert_hitbox` is only valid during prepaint.
        let num_x = gutter_x + num_w;
        let gutter_hitbox = (gutter_w > px(0.0)).then(|| {
            let b =
                Bounds::from_corners(point(gutter_x, bounds.top()), point(num_x, bounds.bottom()));
            window.insert_hitbox(b, gpui::HitboxBehavior::Normal)
        });
        // The fold-chevron sub-column [num_w, gutter_w) is clickable (toggles folds) → give it
        // a pointer cursor so it reads as interactive.
        let fold_hitbox = (gutter_w > num_w).then(|| {
            let b = Bounds::from_corners(
                point(num_x, bounds.top()),
                point(gutter_x + gutter_w, bounds.bottom()),
            );
            window.insert_hitbox(b, gpui::HitboxBehavior::Normal)
        });

        // caret + selection (line_starts/lines are display-indexed; locate_in
        // returns the display row directly).
        let cursor = editor.cursor();
        let sel = editor.selected_range.clone();
        let (mut caret, mut selections) = (None, Vec::new());
        let mut caret_row = None;
        // Current-line highlight (full row incl. gutter): only on real code editors (those
        // with a line-number gutter — the file editor + diff panes), never on plain inputs
        // like the commit box or search fields. Shown even unfocused, with a collapsed caret.
        if editor.line_numbers && !editor.content.is_empty() && sel.is_empty() {
            caret_row = Some(locate_in(&line_starts, &editor.content, cursor).0);
        }
        // The blinking caret itself only shows when focused — including on an empty field
        // (placeholder showing), so a focused input reads as focused.
        if editor.focus_handle.is_focused(window) && sel.is_empty() && editor.blink_on {
            let (li, start) = locate_in(&line_starts, &editor.content, cursor);
            if let Some(line) = lines.get(&li) {
                let x = line.x_for_index(cursor - start);
                let y = bounds.top() + line_height * li as f32;
                // 1px-wide (thinner) caret, nudged 1px left so it has a touch more margin on
                // its right edge before the next glyph.
                caret = Some(fill(
                    Bounds::new(
                        point(text_x + x - px(1.0), y),
                        gpui::size(px(1.0), line_height),
                    ),
                    theme::get().caret,
                ));
            }
        } else if !sel.is_empty() {
            // paint a rect per covered display row
            let (l0, s0) = locate_in(&line_starts, &editor.content, sel.start);
            let (l1, s1) = locate_in(&line_starts, &editor.content, sel.end);
            // `li` is a logical row number used to index two maps AND as a y-coord /
            // boundary test — an iterator rewrite would be less clear, not more.
            #[allow(clippy::needless_range_loop)]
            for li in l0..=l1 {
                let Some(line) = lines.get(&li) else { continue };
                let line_start = line_starts[li];
                let from = if li == l0 { sel.start - s0 } else { 0 };
                let to = if li == l1 {
                    sel.end - s1
                } else {
                    // To the end of this *display* row: the next row's start (so a wrapped
                    // middle row stops at the wrap point, not the logical line end). x_for_index
                    // clamps to the shaped width, so a trailing newline doesn't over-extend.
                    line_starts
                        .get(li + 1)
                        .copied()
                        .unwrap_or(editor.content.len())
                        .saturating_sub(line_start)
                };
                let x0 = line.x_for_index(from);
                let x1 = line.x_for_index(to);
                let y = bounds.top() + line_height * li as f32;
                selections.push(fill(
                    Bounds::from_corners(
                        point(text_x + x0, y),
                        point(text_x + x1.max(x0 + px(2.0)), y + line_height),
                    ),
                    theme::get().selected_bg,
                ));
            }
        }

        Prepaint {
            lines,
            display_rows,
            line_starts,
            visible_lines,
            gutter,
            gutter_w,
            num_w,
            gutter_right,
            caret,
            caret_row,
            selections,
            line_height,
            gutter_hitbox,
            fold_hitbox,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        pre: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (focus, line_bg, line_numbers, word_bg, word_bg_color) = {
            let ed = self.editor.read(cx);
            (
                ed.focus_handle.clone(),
                ed.line_bg.clone(),
                ed.line_numbers,
                ed.word_bg.clone(),
                ed.word_bg_color,
            )
        };
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );
        let lh = pre.line_height;
        // Gutter on the right (diff base pane) leaves text flush left; otherwise text is shifted
        // right past the gutter.
        let text_x = if pre.gutter_right {
            bounds.left()
        } else {
            bounds.left() + pre.gutter_w
        };

        // Visible band (+ overscan) so the per-row paint loops below stay O(visible), not
        // O(file size) — shaping a line number for every row of a 15k-line file each frame
        // was the big-file paint lag.
        let clip = window.content_mask().bounds;
        // A right gutter is PINNED to the viewport's right edge (it doesn't scroll with the
        // text), so the base pane's numbers always sit against the center gutter. A left gutter
        // is content-relative at the element's left edge.
        let gutter_x = if pre.gutter_right {
            clip.right() - pre.gutter_w
        } else {
            bounds.left()
        };
        let vis_top = clip.origin.y - lh * 12.0;
        let vis_bottom = clip.origin.y + clip.size.height + lh * 12.0;
        let on_screen = |i: usize| {
            let y = bounds.top() + lh * i as f32;
            y + lh >= vis_top && y <= vis_bottom
        };

        // The gutter (line numbers + fold chevrons) is not text — show the default arrow
        // cursor there instead of the editor-wide I-beam. The hitbox is created in prepaint
        // (the only phase that allows it); `set_cursor_style` must run here in paint.
        if let Some(hitbox) = pre.gutter_hitbox.take() {
            window.set_cursor_style(gpui::CursorStyle::Arrow, &hitbox);
        }
        // Fold-chevron column is clickable → pointer cursor.
        if let Some(hitbox) = pre.fold_hitbox.take() {
            window.set_cursor_style(gpui::CursorStyle::PointingHand, &hitbox);
        }

        // Full-row highlights must span the visible width, not just the element's content
        // width (which is only as wide as the longest line, for horizontal scroll) — else the
        // current-line/diff tints cut off mid-pane on short lines.
        let row_w = bounds.size.width.max(clip.size.width);
        // Current-line highlight: full row (incl. the gutter) behind everything. Painted
        // first so a diff line tint still wins on a changed caret line.
        if let Some(row) = pre.caret_row {
            let y = bounds.top() + lh * row as f32;
            window.paint_quad(fill(
                Bounds::new(point(bounds.left(), y), gpui::size(row_w, lh)),
                theme::get().caret_row,
            ));
        }

        // Per-line diff background (behind text + selection), full row width. Filler rows
        // (sentinel usize::MAX, inserted for diff alignment) get the separator tint.
        if !line_bg.is_empty() {
            let filler_bg = theme::get().diff_separator_bg;
            for (i, &b) in pre.visible_lines.iter().enumerate() {
                if !on_screen(i) {
                    continue; // virtualize: a big diff has thousands of tinted lines
                }
                let bg = if b == usize::MAX {
                    Some(filler_bg)
                } else {
                    line_bg.get(&b).copied()
                };
                if let Some(bg) = bg {
                    let y = bounds.top() + lh * i as f32;
                    window.paint_quad(fill(
                        Bounds::new(point(bounds.left(), y), gpui::size(row_w, lh)),
                        bg,
                    ));
                }
            }
        }
        // Word-level diff highlight: a stronger tint over just the changed bytes within a
        // modified line (inline word diff). Painted after the line tint, before text.
        if !word_bg.is_empty() {
            for (i, &b) in pre.visible_lines.iter().enumerate() {
                if b == usize::MAX || !on_screen(i) {
                    continue;
                }
                let (Some(ranges), Some(line)) = (word_bg.get(&b), pre.lines.get(&i)) else {
                    continue;
                };
                let y = bounds.top() + lh * i as f32;
                for r in ranges {
                    let x0 = line.x_for_index(r.start);
                    let x1 = line.x_for_index(r.end);
                    window.paint_quad(fill(
                        Bounds::from_corners(
                            point(text_x + x0, y),
                            point(text_x + x1.max(x0 + px(1.0)), y + lh),
                        ),
                        word_bg_color,
                    ));
                }
            }
        }
        for sel in pre.selections.drain(..) {
            window.paint_quad(sel);
        }
        // Line numbers, right-aligned in their column. A LEFT gutter paints here (before the
        // text — they don't overlap). A RIGHT (pinned) gutter paints AFTER the text instead,
        // with an opaque backing, so scrolled text passes underneath it — see below.
        if !pre.gutter_right && line_numbers && pre.num_w > px(0.0) {
            let style = window.text_style();
            let font = style.font();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let dim: Hsla = theme::get().line_number.into();
            // On a changed (hunk-tinted) line, the dim grey is unreadable on the colored
            // band — use the bright text color there instead.
            let bright: Hsla = theme::get().text.into();
            for (i, &b) in pre.visible_lines.iter().enumerate() {
                if b == usize::MAX || !on_screen(i) {
                    continue; // filler row, or off-screen → don't shape a number for it
                }
                let color = if line_bg.contains_key(&b) {
                    bright
                } else {
                    dim
                };
                let label = (b + 1).to_string();
                let runs = [TextRun {
                    len: label.len(),
                    font: font.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }];
                let shaped = window
                    .text_system()
                    .shape_line(label.into(), font_size, &runs, None);
                let y = bounds.top() + lh * i as f32;
                // Numbers occupy the sub-column `[gutter_x, gutter_x+num_w)`; right-align
                // within it. Sit closer to the code when there's no fold-chevron column.
                // Equal padding on both sides of the number (the column reserves 2×6px), so
                // the numbers sit equidistant from the text and the center gutter in both panes.
                let pad = px(6.0);
                let x = gutter_x + pre.num_w - pad - shaped.width;
                let _ = shaped.paint(point(x, y), lh, window, cx);
            }
        }
        // Divider between a LEFT gutter and the text (the right-gutter divider is drawn in the
        // pinned block after the text). Hug the text (1px in) so the right-aligned number keeps
        // its full `pad` of clear space on the text side instead of sitting on the divider.
        if !pre.gutter_right && pre.gutter_w > px(0.0) {
            let x = text_x - px(1.0);
            window.paint_quad(fill(
                Bounds::new(
                    point(x, bounds.top()),
                    gpui::size(px(1.0), bounds.size.height),
                ),
                theme::get().divider,
            ));
        }
        // `pre.lines` is sparse — only on-screen rows are present, so iterate it directly
        // (no full-file scan) and position each by its display row.
        for (&i, line) in pre.lines.iter() {
            let origin = point(text_x, bounds.top() + lh * i as f32);
            let _ = line.paint(origin, lh, window, cx);
        }
        // Fold gutter: a chevron per fold-start display row, plus a `⋯` marker
        // trailing the text of collapsed rows.
        if pre.gutter_w > px(0.0) {
            let style = window.text_style();
            let font = style.font();
            // Chevron a touch bigger than the code so it reads as an affordance.
            let font_size = style.font_size.to_pixels(window.rem_size()) + px(3.0);
            let chevron_color: Hsla = theme::get().line_number.into();
            for &(dr, folded) in &pre.gutter {
                // Virtualize: a JSON file has a foldable start on nearly every line, so
                // `pre.gutter` can hold thousands of entries — shaping a chevron for every
                // one each frame (no band guard) was the real big-file paint cost (~30ms on
                // a 37k-line package-lock). Only shape the on-screen ones.
                if !on_screen(dr) {
                    continue;
                }
                // Same triangle glyphs as the file tree (they sit centered on the text
                // baseline, unlike `⌄`/`›` which floated high).
                let glyph = if folded { "▸" } else { "▾" };
                let runs = [TextRun {
                    len: glyph.len(),
                    font: font.clone(),
                    color: chevron_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }];
                let shaped = window
                    .text_system()
                    .shape_line(glyph.into(), font_size, &runs, None);
                let y = bounds.top() + lh * dr as f32;
                // Chevron column sits right of the line numbers; `num_w + 2` leaves a couple
                // px of padding before the divider (at `text_x - 6`, the fold column is wider
                // than the glyph).
                let _ = shaped.paint(point(gutter_x + pre.num_w + px(2.0), y), lh, window, cx);
                if folded {
                    // `⋯` just past the end of the collapsed line's text.
                    let ell = [TextRun {
                        len: "⋯".len(),
                        font: font.clone(),
                        color: chevron_color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }];
                    let ell_shaped =
                        window
                            .text_system()
                            .shape_line("⋯".into(), font_size, &ell, None);
                    if let Some(line) = pre.lines.get(&dr) {
                        let x = text_x + line.width + px(6.0);
                        let _ = ell_shaped.paint(point(x, y), lh, window, cx);
                    }
                }
            }
        }
        // Pinned RIGHT gutter (diff base pane): drawn AFTER the text so scrolled-under code is
        // hidden by an opaque per-row backing, against the viewport's right edge. Per-row bg
        // keeps the hunk tint / filler separator showing in the gutter, matching the left pane.
        if pre.gutter_right && line_numbers && pre.num_w > px(0.0) {
            let t = theme::get();
            let pad = px(6.0);
            // Backing + divider start at the number column's text-facing edge (`gutter_x`), so
            // the number keeps the same `pad` clearance on the code side as the right pane's
            // number does — no extra space.
            let left = gutter_x;
            let filler_bg = t.diff_separator_bg;
            let style = window.text_style();
            let font = style.font();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let dim: Hsla = t.line_number.into();
            let bright: Hsla = t.text.into();
            for (i, &b) in pre.visible_lines.iter().enumerate() {
                if !on_screen(i) {
                    continue;
                }
                let y = bounds.top() + lh * i as f32;
                // Row backing: the line's tint (or the filler separator) when present, else the
                // pane bg — covers any code that scrolled under the pinned gutter.
                let bg = if b == usize::MAX {
                    filler_bg
                } else {
                    line_bg.get(&b).copied().unwrap_or(t.main_bg)
                };
                window.paint_quad(fill(
                    Bounds::from_corners(point(left, y), point(clip.right(), y + lh)),
                    bg,
                ));
                if b == usize::MAX {
                    continue; // filler row → no number
                }
                let color = if line_bg.contains_key(&b) {
                    bright
                } else {
                    dim
                };
                let label = (b + 1).to_string();
                let runs = [TextRun {
                    len: label.len(),
                    font: font.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }];
                let shaped = window
                    .text_system()
                    .shape_line(label.into(), font_size, &runs, None);
                let x = gutter_x + pre.num_w - pad - shaped.width;
                let _ = shaped.paint(point(x, y), lh, window, cx);
            }
            // Divider on the gutter's text-facing edge.
            window.paint_quad(fill(
                Bounds::new(
                    point(left, clip.origin.y),
                    gpui::size(px(1.0), clip.size.height),
                ),
                t.divider,
            ));
        }
        // Caret-follow scrolling: a caret/selection move (keyboard nav, or a find-match
        // selection while the find box — not the editor — is focused) set `reveal_pending`.
        // Compute the caret's pixel rect from the cursor + this frame's layout (NOT the
        // painted caret quad, which is absent when the editor isn't focused) and nudge the
        // parent scroll so it's in view, both axes.
        let (reveal, scroll, cur) = {
            let ed = self.editor.read(cx);
            (ed.reveal_pending, ed.scroll.clone(), ed.cursor())
        };
        if reveal {
            // Display row holding the cursor (`line_starts` is display-indexed, ascending).
            let dr = match pre.line_starts.binary_search(&cur) {
                Ok(i) => i,
                Err(i) => i.saturating_sub(1),
            };
            let col = cur.saturating_sub(pre.line_starts.get(dr).copied().unwrap_or(0));
            let cx_x = pre
                .lines
                .get(&dr)
                .map(|l| l.x_for_index(col))
                .unwrap_or(px(0.0));
            if let Some(scroll) = scroll {
                let top = bounds.top() + lh * dr as f32;
                let bottom = top + lh;
                let caret_x = bounds.left() + pre.gutter_w + cx_x;
                let cur_off = scroll.offset();
                let max = scroll.max_offset();
                let mut off = cur_off;
                if top < clip.top() {
                    off.y += clip.top() - top;
                } else if bottom > clip.bottom() {
                    off.y -= bottom - clip.bottom();
                }
                if caret_x < clip.left() {
                    off.x += clip.left() - caret_x;
                } else if caret_x > clip.right() {
                    off.x -= caret_x - clip.right();
                }
                off.x = off.x.clamp(-max.width, px(0.0));
                off.y = off.y.clamp(-max.height, px(0.0));
                if off != cur_off {
                    scroll.set_offset(off);
                    // Re-render so virtualization shapes the now-visible rows at the new offset.
                    let entity = self.editor.clone();
                    window.on_next_frame(move |_, cx| entity.update(cx, |_, cx| cx.notify()));
                }
            }
            self.editor.update(cx, |ed, _| ed.reveal_pending = false);
        }
        if let Some(caret) = pre.caret.take() {
            window.paint_quad(caret);
        }
        // stash caches for mouse + vertical movement
        let lines = std::mem::take(&mut pre.lines);
        let starts = std::mem::take(&mut pre.line_starts);
        let visible = std::mem::take(&mut pre.visible_lines);
        let gutter_w = pre.gutter_w;
        let display_rows = pre.display_rows;
        self.editor.update(cx, |ed, _| {
            ed.line_layouts = lines;
            ed.display_rows = display_rows;
            ed.line_starts = starts;
            ed.visible_lines = visible;
            ed.gutter_w = gutter_w;
            ed.num_w = pre.num_w;
            ed.bounds = Some(bounds);
            ed.line_height = lh;
            ed.viewport = Some(clip);
        });
    }
}
