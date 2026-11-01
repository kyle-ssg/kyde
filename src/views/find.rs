//! In-editor find / replace bar (⌘F / ⌘R) — targets whichever editor has focus: the Browse
//! editor, any diff pane in the main window, or a pane of the Compare / Merge / Local History
//! windows. Crate-root child module.

use crate::*;

impl Kyde {
    /// Find / replace bar (⌘F / ⌘R), floated at the top right of the panes it searches.
    /// Live-highlights matches; ↩ / ⌘G cycle, the buttons replace.
    fn render_find_bar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let (next, prev) = FIND_STEP_KEYS;
        let count = self.find.matches.len();
        let has_query = !self.find.query.read(cx).text().is_empty();
        let current = if count == 0 { 0 } else { self.find.idx + 1 };
        let mut bar = ui::find::Bar::new(
            "find-bar",
            self.find.query.clone(),
            ui::find::match_label(has_query, count, current),
        )
        .on_up(
            format!("Previous match  ({prev})"),
            cx.listener(|this, _e, w, cx| this.find_prev(&FindPrev, w, cx)),
        )
        .on_down(
            format!("Next match  ({next})"),
            cx.listener(|this, _e, w, cx| this.find_next(&FindNext, w, cx)),
        )
        .on_close(cx.listener(|this, _e, w, cx| this.close_find(&CloseFind, w, cx)));
        if self.find.replace {
            bar = bar.replace(
                self.find.replace_query.clone(),
                cx.listener(|this, _e, w, cx| this.replace_one(&ReplaceOne, w, cx)),
                cx.listener(|this, _e, w, cx| this.replace_all(&ReplaceAll, w, cx)),
            );
        }
        ui::find::floating(bar).into_any_element()
    }

    pub(crate) fn act_find(&mut self, _: &FindInFile, window: &mut Window, cx: &mut Context<Self>) {
        self.open_find(FindHost::Main, false, window, cx);
    }

    pub(crate) fn act_replace(
        &mut self,
        _: &ReplaceInFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_find(FindHost::Main, true, window, cx);
    }

    /// The editors ⌘F can search in `host` right now, preferred default first (used when
    /// none of them has focus). Empty = nothing searchable on screen, so ⌘F is a no-op.
    fn find_candidates(&self, host: FindHost) -> Vec<Entity<CodeEditor>> {
        let diff = || vec![self.diff.right.clone(), self.diff.left.clone()];
        match host {
            FindHost::Main if self.diff_view_open => diff(),
            FindHost::Main => match self.mode {
                Mode::Browse if self.browse.open_path.is_some() => {
                    vec![self.browse.editor.clone()]
                }
                Mode::Commit | Mode::History if self.diff.path.is_some() => diff(),
                _ => Vec::new(),
            },
            FindHost::Modal(ModalKind::Compare) => {
                vec![self.compare.right.clone(), self.compare.left.clone()]
            }
            FindHost::Modal(ModalKind::LocalHistory) => {
                vec![self.lh.right.clone(), self.lh.left.clone()]
            }
            FindHost::Modal(ModalKind::Merge) if self.merge.selected.is_some() => {
                if self.merge.compare == MergeCompare::MergeView3 {
                    vec![
                        self.merge.result.clone(),
                        self.merge.ours.clone(),
                        self.merge.theirs.clone(),
                    ]
                } else {
                    vec![self.merge.cmp_r.clone(), self.merge.cmp_l.clone()]
                }
            }
            FindHost::Modal(_) => Vec::new(),
        }
    }

    pub(crate) fn open_find(
        &mut self,
        host: FindHost,
        replace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cands = self.find_candidates(host);
        // The focused pane wins; else the preferred pane, unless it's empty and another
        // isn't (a deleted file's diff shows only its base side).
        let Some(target) = cands
            .iter()
            .find(|e| e.read(cx).focus_handle.is_focused(window))
            .or_else(|| cands.iter().find(|e| !e.read(cx).text().is_empty()))
            .or(cands.first())
            .cloned()
        else {
            return;
        };
        if let Some(old) = self.find.target.take().filter(|old| *old != target) {
            old.update(cx, |e, cx| {
                e.find_bg.clear();
                e.find_current = None;
                cx.notify();
            });
        }
        // Re-scan as the target's text changes under an open bar (another file selected,
        // an edit, a reload) — without moving the caret the user may be typing at.
        self.find.target_sub = Some(cx.subscribe(&target, |this, ed, ev, cx| {
            if matches!(ev, EditorEvent::Changed)
                && this.find.open
                && this.find.target.as_ref() == Some(&ed)
                && ed.read(cx).content_gen() != this.find.scanned_gen
            {
                this.scan_find(false, cx);
            }
        }));
        // Replace needs an editable target — base panes, committed diffs and snapshots are
        // read-only, so ⌘R there opens find only.
        self.find.replace = replace && !target.read(cx).read_only;
        self.find.target = Some(target);
        self.find.host = host;
        self.find.open = true;
        self.recompute_find(cx);
        let handle = self.find.query.read(cx).focus_handle.clone();
        window.focus(&handle);
        window.defer(cx, move |window, _cx| window.focus(&handle));
        cx.notify();
    }

    /// Whether the bar is open in `host` over a pane that's on screen there — switching
    /// views (or a Merge window stage) hides it along with its pane.
    pub(crate) fn find_shown(&self, host: FindHost) -> bool {
        self.find.open
            && self.find.host == host
            && self
                .find
                .target
                .as_ref()
                .is_some_and(|t| self.find_candidates(host).contains(t))
    }

    /// The find bar, when it's shown in `host` (see `find_shown`) — floated, so the caller
    /// adds it to a `relative` container over the panes it searches.
    pub(crate) fn find_bar(
        &self,
        host: FindHost,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        self.find_shown(host).then(|| self.render_find_bar(cx))
    }

    pub(crate) fn close_find(
        &mut self,
        _: &CloseFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ed) = self.dismiss_find(cx) {
            let handle = ed.read(cx).focus_handle.clone();
            window.focus(&handle);
        }
    }

    /// The window hosting the bar closed — drop the bar with it.
    pub(crate) fn find_host_closed(&mut self, host: FindHost, cx: &mut Context<Self>) {
        if self.find.open && self.find.host == host {
            self.dismiss_find(cx);
        }
    }

    /// Close the bar and clear its highlights; returns the editor it was searching.
    pub(crate) fn dismiss_find(&mut self, cx: &mut Context<Self>) -> Option<Entity<CodeEditor>> {
        self.find.open = false;
        self.find.matches.clear();
        self.find.target_sub = None;
        let ed = self.find.target.clone()?;
        ed.update(cx, |e, cx| {
            e.find_bg.clear();
            e.find_current = None;
            cx.notify();
        });
        cx.notify();
        Some(ed)
    }

    /// The query changed — re-scan and jump to the (first) match.
    pub(crate) fn recompute_find(&mut self, cx: &mut Context<Self>) {
        self.scan_find(true, cx);
    }

    /// Recompute match ranges for the current query (ASCII case-insensitive) and repaint
    /// the highlights; `select` also selects the current match (scrolling it into view).
    fn scan_find(&mut self, select: bool, cx: &mut Context<Self>) {
        let Some(ed) = self.find.target.clone() else {
            return;
        };
        let q = self.find.query.read(cx).text().to_string();
        let (matches, by_line, gen) = {
            let e = ed.read(cx);
            let matches = find_matches(e.text(), &q);
            let by_line = matches_by_line(e.text(), &matches);
            (matches, by_line, e.content_gen())
        };
        self.find.matches = matches;
        self.find.scanned_gen = gen;
        if self.find.idx >= self.find.matches.len() {
            self.find.idx = 0;
        }
        let current = self.find.matches.get(self.find.idx).cloned();
        ed.update(cx, |e, cx| {
            e.find_bg = by_line;
            e.find_current = current.clone().map(|r| line_range(e.text(), r));
            if let Some(r) = current.filter(|_| select) {
                e.select_range(r, cx);
            }
            cx.notify();
        });
        cx.notify();
    }

    /// Select the match at `find.idx` in the target.
    fn select_current_match(&mut self, cx: &mut Context<Self>) {
        let (Some(ed), Some(r)) = (
            self.find.target.clone(),
            self.find.matches.get(self.find.idx).cloned(),
        ) else {
            return;
        };
        ed.update(cx, |e, cx| {
            e.find_current = Some(line_range(e.text(), r.clone()));
            e.select_range(r, cx);
        });
        cx.notify();
    }

    pub(crate) fn find_next(&mut self, _: &FindNext, _w: &mut Window, cx: &mut Context<Self>) {
        if !self.find.open || self.find.matches.is_empty() {
            return;
        }
        self.find.idx = (self.find.idx + 1) % self.find.matches.len();
        self.select_current_match(cx);
    }

    pub(crate) fn find_prev(&mut self, _: &FindPrev, _w: &mut Window, cx: &mut Context<Self>) {
        if !self.find.open || self.find.matches.is_empty() {
            return;
        }
        self.find.idx = (self.find.idx + self.find.matches.len() - 1) % self.find.matches.len();
        self.select_current_match(cx);
    }

    pub(crate) fn replace_one(&mut self, _: &ReplaceOne, _w: &mut Window, cx: &mut Context<Self>) {
        let rep = self.find.replace_query.read(cx).text().to_string();
        let (Some(ed), Some(r)) = (
            self.find.target.clone(),
            self.find.matches.get(self.find.idx).cloned(),
        ) else {
            return;
        };
        ed.update(cx, |e, cx| e.replace_range_text(r, &rep, cx));
        // Re-scan now (selecting the next match); the edit's own change event then finds
        // nothing new to scan.
        self.recompute_find(cx);
    }

    pub(crate) fn replace_all(&mut self, _: &ReplaceAll, _w: &mut Window, cx: &mut Context<Self>) {
        let rep = self.find.replace_query.read(cx).text().to_string();
        let Some(ed) = self.find.target.clone() else {
            return;
        };
        // Replace right-to-left so earlier ranges stay valid.
        let ranges: Vec<_> = self.find.matches.clone();
        ed.update(cx, |e, cx| {
            for r in ranges.into_iter().rev() {
                e.replace_range_text(r, &rep, cx);
            }
        });
        self.recompute_find(cx);
    }
}

/// Byte ranges of every ASCII-case-insensitive, non-overlapping occurrence of `query`.
fn find_matches(content: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    if query.is_empty() || query.len() > content.len() {
        return out;
    }
    // `to_ascii_lowercase` preserves byte length, so positions map 1:1 to `content`.
    let hay = content.to_ascii_lowercase();
    let needle = query.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(pos) = hay[from..].find(&needle) {
        let s = from + pos;
        out.push(s..s + needle.len());
        from = s + needle.len();
    }
    out
}

/// Group sorted `matches` by buffer line → line-relative byte ranges (clipped at the line
/// end), the shape the editor's `find_bg` paints. One forward pass over `content`, so a
/// query matching everywhere stays linear rather than re-counting lines per match.
fn matches_by_line(content: &str, matches: &[std::ops::Range<usize>]) -> LineWordBgs {
    let bytes = content.as_bytes();
    let mut out = LineWordBgs::new();
    let (mut line, mut line_start, mut pos) = (0usize, 0usize, 0usize);
    for r in matches {
        while pos < r.start {
            if bytes[pos] == b'\n' {
                line += 1;
                line_start = pos + 1;
            }
            pos += 1;
        }
        let line_end = memchr_newline(bytes, line_start).unwrap_or(bytes.len());
        out.entry(line)
            .or_default()
            .push(r.start - line_start..r.end.min(line_end) - line_start);
    }
    out
}

/// A match's `(buffer line, byte range within that line)`, clipped at the line end — the
/// shape the editor paints `find_current` from.
fn line_range(content: &str, r: std::ops::Range<usize>) -> (usize, std::ops::Range<usize>) {
    let before = &content[..r.start];
    let line = before.bytes().filter(|&b| b == b'\n').count();
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let line_end = memchr_newline(content.as_bytes(), line_start).unwrap_or(content.len());
    (line, r.start - line_start..r.end.min(line_end) - line_start)
}

/// Index of the first `\n` at or after `from`.
fn memchr_newline(bytes: &[u8], from: usize) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| from + i)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Perf guard (see CLAUDE.md "Performance regression tests"): ⌘F re-scans on every
    /// keystroke, and a one-letter query matches nearly everywhere — line bucketing must
    /// stay linear in the buffer, not re-count lines per match.
    #[test]
    fn perf_find_scan_with_dense_matches() {
        let content = "let value = some_call(argument, other);\n".repeat(40_000); // ~1.6MB
        let t = std::time::Instant::now();
        let m = find_matches(&content, "e");
        let by_line = matches_by_line(&content, &m);
        let dt = t.elapsed();
        assert_eq!(by_line.len(), 40_000);
        assert_eq!(by_line[&7][0], 1..2, "line-relative: the `e` of `let`");
        assert!(dt.as_secs_f32() < 2.0, "find scan took {dt:?}");
    }
}
