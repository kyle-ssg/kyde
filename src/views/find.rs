//! In-editor find / replace bar (⌘F / ⌘R) — targets the Browse editor or a diff pane.
//! Crate-root child module.

use crate::*;

impl Kyde {
    /// Find / replace bar shown atop the editor (cmd-f / cmd-r). Live-highlights matches;
    /// enter / cmd-g cycle, the buttons replace.
    pub(crate) fn render_find_bar(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let count = self.find_matches.len();
        let label = if count == 0 {
            "No results".to_string()
        } else {
            format!("{}/{}", self.find_idx + 1, count)
        };
        let input_box = |child: gpui::Entity<CodeEditor>| {
            div()
                .flex_1()
                .min_w_0()
                .h(px(26.0))
                .px_2()
                .flex()
                .items_center()
                .bg(t.main_bg)
                .border_1()
                .border_color(t.divider)
                .rounded_md()
                .child(child)
        };
        let btn = |glyph: &str, id: &'static str| {
            div()
                .id(SharedString::from(id))
                .size(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .text_color(t.secondary_text)
                .hover(|s| s.bg(t.bg_light).text_color(t.text))
                .cursor_pointer()
                .child(SharedString::from(glyph.to_string()))
        };

        let find_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(input_box(self.find_query.clone()))
            .child(
                div()
                    .flex_none()
                    .text_color(t.line_number)
                    .text_size(px(theme::get().ui_font_size - 1.0))
                    .child(label),
            )
            .child(btn("‹", "find-prev").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, w, cx| this.find_prev(&FindPrev, w, cx)),
            ))
            .child(btn("›", "find-next").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, w, cx| this.find_next(&FindNext, w, cx)),
            ))
            .child(btn("×", "find-close").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, w, cx| this.close_find(&CloseFind, w, cx)),
            ));

        let mut col = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1p5()
            .bg(t.panel_bg)
            .border_b_1()
            .border_color(t.divider)
            .font_family(ui)
            .text_size(px(theme::get().ui_font_size))
            .child(find_row);

        if self.find_replace {
            let replace_row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(input_box(self.replace_query.clone()))
                .child(
                    div()
                        .id("replace-one")
                        .px_2()
                        .h(px(24.0))
                        .flex()
                        .items_center()
                        .rounded_md()
                        .text_color(t.secondary_text)
                        .hover(|s| s.bg(t.bg_light).text_color(t.text))
                        .cursor_pointer()
                        .child("Replace")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, w, cx| this.replace_one(&ReplaceOne, w, cx)),
                        ),
                )
                .child(
                    div()
                        .id("replace-all")
                        .px_2()
                        .h(px(24.0))
                        .flex()
                        .items_center()
                        .rounded_md()
                        .text_color(t.secondary_text)
                        .hover(|s| s.bg(t.bg_light).text_color(t.text))
                        .cursor_pointer()
                        .child("All")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, w, cx| this.replace_all(&ReplaceAll, w, cx)),
                        ),
                );
            col = col.child(replace_row);
        }
        col.into_any_element()
    }

    pub(crate) fn act_find(&mut self, _: &FindInFile, window: &mut Window, cx: &mut Context<Self>) {
        self.open_find(false, window, cx);
    }

    pub(crate) fn act_replace(
        &mut self,
        _: &ReplaceInFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_find(true, window, cx);
    }

    /// The editor the find/replace bar currently acts on.
    fn find_ed(&self) -> Entity<CodeEditor> {
        match self.find_target {
            crate::FindTarget::File => self.file_editor.clone(),
            crate::FindTarget::DiffLeft => self.diff_left.clone(),
            crate::FindTarget::DiffRight => self.diff_right.clone(),
        }
    }

    pub(crate) fn open_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Works in the Browse editor (with a file open) and the Show-Diff view's panes.
        let target = if self.diff_view_open {
            // Default to the working (right) pane; the base (left) pane is read-only.
            if self.diff_left.read(cx).focus_handle.is_focused(window) {
                crate::FindTarget::DiffLeft
            } else {
                crate::FindTarget::DiffRight
            }
        } else if self.mode == Mode::Browse && self.open_path.is_some() {
            crate::FindTarget::File
        } else {
            return;
        };
        self.find_target = target;
        self.find_open = true;
        // Replace needs an editable target — the diff base pane / committed diffs are read-only,
        // so ⌘R there opens find only.
        self.find_replace = replace && !self.find_ed().read(cx).read_only;
        self.recompute_find(cx);
        let handle = self.find_query.read(cx).focus_handle.clone();
        window.focus(&handle);
        window.defer(cx, move |window, _cx| window.focus(&handle));
        cx.notify();
    }

    pub(crate) fn close_find(
        &mut self,
        _: &CloseFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.find_open = false;
        self.find_matches.clear();
        let ed = self.find_ed();
        // Only the file editor's `word_bg` is owned by find — the diff panes use `word_bg` for
        // word-level diff highlighting, so don't clobber it there.
        if self.find_target == crate::FindTarget::File {
            ed.update(cx, |e, _| e.word_bg.clear());
        }
        let handle = ed.read(cx).focus_handle.clone();
        window.focus(&handle);
        cx.notify();
    }

    /// Recompute match ranges for the current query (ASCII case-insensitive) and repaint
    /// the highlights + select the current match.
    pub(crate) fn recompute_find(&mut self, cx: &mut Context<Self>) {
        let q = self.find_query.read(cx).text().to_string();
        let content = self.find_ed().read(cx).text().to_string();
        self.find_matches.clear();
        if !q.is_empty() && q.len() <= content.len() {
            // `to_ascii_lowercase` preserves byte length, so positions map 1:1 to `content`.
            let hay = content.to_ascii_lowercase();
            let needle = q.to_ascii_lowercase();
            let mut from = 0usize;
            while let Some(pos) = hay[from..].find(&needle) {
                let s = from + pos;
                self.find_matches.push(s..s + needle.len());
                from = s + needle.len();
            }
        }
        if self.find_idx >= self.find_matches.len() {
            self.find_idx = 0;
        }
        self.apply_find_highlight(cx);
    }

    /// Paint match highlights on the editor (via its `word_bg`) and select the current one.
    fn apply_find_highlight(&mut self, cx: &mut Context<Self>) {
        let ed = self.find_ed();
        let content = ed.read(cx).text().to_string();
        let mut map: std::collections::HashMap<usize, Vec<std::ops::Range<usize>>> =
            std::collections::HashMap::new();
        for r in &self.find_matches {
            let line = content[..r.start].bytes().filter(|&b| b == b'\n').count();
            let line_start = content[..r.start].rfind('\n').map_or(0, |i| i + 1);
            let line_end = content[line_start..]
                .find('\n')
                .map_or(content.len(), |i| line_start + i);
            let s = r.start - line_start;
            let e = (r.end.min(line_end)) - line_start;
            map.entry(line).or_default().push(s..e);
        }
        // The diff panes already use `word_bg` for word-level diff highlighting, so only paint
        // the amber match highlight on the file editor; in the diff the selection marks the
        // match (and find_next/prev navigate).
        if self.find_target == crate::FindTarget::File {
            ed.update(cx, |e, _| {
                e.word_bg = map;
                e.word_bg_color = kyde_color::Color::rgb(0x6E5A1E); // amber search highlight
            });
        }
        if let Some(r) = self.find_matches.get(self.find_idx).cloned() {
            ed.update(cx, |e, cx| e.select_range(r, cx));
        }
        cx.notify();
    }

    pub(crate) fn find_next(&mut self, _: &FindNext, _w: &mut Window, cx: &mut Context<Self>) {
        if self.find_matches.is_empty() {
            return;
        }
        self.find_idx = (self.find_idx + 1) % self.find_matches.len();
        if let Some(r) = self.find_matches.get(self.find_idx).cloned() {
            self.find_ed().update(cx, |e, cx| e.select_range(r, cx));
        }
        cx.notify();
    }

    pub(crate) fn find_prev(&mut self, _: &FindPrev, _w: &mut Window, cx: &mut Context<Self>) {
        if self.find_matches.is_empty() {
            return;
        }
        self.find_idx = (self.find_idx + self.find_matches.len() - 1) % self.find_matches.len();
        if let Some(r) = self.find_matches.get(self.find_idx).cloned() {
            self.find_ed().update(cx, |e, cx| e.select_range(r, cx));
        }
        cx.notify();
    }

    pub(crate) fn replace_one(&mut self, _: &ReplaceOne, _w: &mut Window, cx: &mut Context<Self>) {
        let rep = self.replace_query.read(cx).text().to_string();
        if let Some(r) = self.find_matches.get(self.find_idx).cloned() {
            self.find_ed()
                .update(cx, |e, cx| e.replace_range_text(r, &rep, cx));
            // The edit fires autosave + Changed; re-scan against the new content.
            self.recompute_find(cx);
        }
    }

    pub(crate) fn replace_all(&mut self, _: &ReplaceAll, _w: &mut Window, cx: &mut Context<Self>) {
        let rep = self.replace_query.read(cx).text().to_string();
        // Replace right-to-left so earlier ranges stay valid.
        let ranges: Vec<_> = self.find_matches.clone();
        self.find_ed().update(cx, |e, cx| {
            for r in ranges.into_iter().rev() {
                e.replace_range_text(r, &rep, cx);
            }
        });
        self.recompute_find(cx);
    }
}
