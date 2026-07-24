//! Fuzzy file finder + Find-in-Files + command palette — the transient `⌘P`/`⌘⇧F`/`⌘⇧A`
//! overlay (`FinderMode::Files|Content|Actions`). Crate-root child module.

use crate::*;

impl Kyde {
    pub(crate) fn render_finder(
        &self,
        ui: &'static str,
        fs: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        // gpui only tracks hover on *identified* elements, so each row needs an `.id()` or the
        // `.hover(...)` style never applies (the tree rows that hover all carry an id).
        // The finder panel itself is `bg_mid`, so the hover shade must be lighter
        // (`bg_light`) to be visible — `bg_mid` on `bg_mid` shows nothing.
        let row_base = |i: usize, sel: bool| {
            ui::picker::row(("finder-row", i), sel, t.bg_light)
                .flex()
                .flex_row()
                .items_center()
                // Inset the rounded pill 2px from the panel edges so the hover/selected
                // background doesn't run into the corners.
                .mx(px(2.0))
                .px_3()
                .py_1()
                .text_color(t.text)
        };
        // Color changed files by their git status, matching the Browse tree / commit view
        // (issue #60). O(1) lookup per row — same map the tree builds per frame.
        let status_by_path: std::collections::HashMap<&PathBuf, FileStatus> =
            self.files.iter().map(|f| (&f.path, f.status)).collect();
        let rows: Vec<gpui::AnyElement> = match self.finder.mode {
            FinderMode::Files => self
                .finder
                .results
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let sel = i == self.finder.selected;
                    let name: SharedString = p.to_string_lossy().into_owned().into();
                    let name_color = status_by_path.get(p).map_or(t.text, |&s| status_color(s));
                    let pc = p.clone();
                    let icon = div()
                        .w(px(20.0))
                        .flex_none()
                        .mr(px(8.0))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_end()
                        .child(badge_inner(file_badge(p), 0.0));
                    row_base(i, sel)
                        .child(icon)
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(name_color)
                                .child(name),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, window, cx| {
                                this.open_file(pc.clone(), cx);
                                this.mode = Mode::Browse;
                                this.finder.open = false;
                                window.focus(&this.focus_handle);
                                cx.notify();
                            }),
                        )
                        .into_any_element()
                })
                .collect(),
            FinderMode::Content => self
                .finder
                .content_results
                .iter()
                .enumerate()
                .map(|(i, hit)| {
                    let sel = i == self.finder.selected;
                    let path = hit.path.clone();
                    let line = hit.line;
                    let loc: SharedString =
                        format!("{}:{}", hit.path.to_string_lossy(), hit.line).into();
                    // The matched line, trimmed + capped so a huge minified line can't blow up.
                    let snippet: SharedString = hit
                        .text
                        .trim_start()
                        .chars()
                        .take(200)
                        .collect::<String>()
                        .into();
                    let icon = div()
                        .w(px(20.0))
                        .flex_none()
                        .mr(px(8.0))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_end()
                        .child(badge_inner(file_badge(&path), 0.0));
                    row_base(i, sel)
                        .child(icon)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(theme::font::FAMILY)
                                .child(snippet),
                        )
                        .child(
                            div()
                                .flex_none()
                                .ml(px(10.0))
                                .max_w(px(220.0))
                                .truncate()
                                .text_color(t.line_number)
                                .child(loc),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, window, cx| {
                                this.finder.open = false;
                                this.open_file_at_line(path.clone(), line, window, cx);
                            }),
                        )
                        .into_any_element()
                })
                .collect(),
            FinderMode::Actions => self
                .finder
                .action_results
                .iter()
                .enumerate()
                .map(|(row, &ai)| {
                    let (label, kind, key_action) = PALETTE[ai];
                    let sel = row == self.finder.selected;
                    // Right-aligned shortcut chip, when this action has a bound key.
                    let shortcut = (!key_action.is_empty())
                        .then(|| self.keymap.key_for(key_action))
                        .flatten()
                        .map(|k| {
                            div()
                                .px_2()
                                .bg(t.bg_mid)
                                .rounded_md()
                                .text_color(t.line_number)
                                .child(SharedString::from(pretty_key(&k)))
                        });
                    row_base(row, sel)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(label)),
                        )
                        .children(shortcut)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, window, cx| {
                                this.run_palette(kind, window, cx);
                            }),
                        )
                        .into_any_element()
                })
                .collect(),
            FinderMode::Scratch => self
                .finder
                .action_results
                .iter()
                .enumerate()
                .map(|(row, &li)| {
                    let (label, ext) = scratch::LANGS[li];
                    let sel = row == self.finder.selected;
                    let icon = div()
                        .w(px(20.0))
                        .flex_none()
                        .mr(px(8.0))
                        .flex()
                        .items_center()
                        .justify_end()
                        .child(badge_inner(
                            file_badge(std::path::Path::new(&format!("x.{ext}"))),
                            0.0,
                        ));
                    row_base(row, sel)
                        .child(icon)
                        .child(SharedString::from(label))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| this.create_scratch(ext, cx)),
                        )
                        .into_any_element()
                })
                .collect(),
        };

        let modal = div()
            .key_context("FileFinder")
            .on_action(cx.listener(Self::finder_up))
            .on_action(cx.listener(Self::finder_down))
            .on_action(cx.listener(Self::finder_confirm))
            .on_action(cx.listener(Self::finder_close))
            .w(px(640.0))
            .max_h(px(440.0))
            .flex()
            .flex_col()
            .bg(theme::get().bg_mid)
            .border_1()
            .border_color(theme::get().bg_light)
            .rounded_md()
            .shadow_lg()
            .font_family(ui)
            .text_size(fs)
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(theme::get().divider)
                    .child(self.finder.query.clone()),
            )
            .when(
                matches!(self.finder.mode, FinderMode::Content)
                    && !self.finder.query.read(cx).text().is_empty(),
                |d| {
                    let n = self.finder.content_results.len();
                    let files = self
                        .finder
                        .content_results
                        .iter()
                        .map(|h| &h.path)
                        .collect::<std::collections::HashSet<_>>()
                        .len();
                    let label: SharedString = if n == 0 {
                        "No matches".into()
                    } else {
                        format!(
                            "{n} match{} in {files} file{}",
                            if n == 1 { "" } else { "es" },
                            if files == 1 { "" } else { "s" }
                        )
                        .into()
                    };
                    d.child(
                        div()
                            .px_3()
                            .py_1()
                            .border_b_1()
                            .border_color(t.divider)
                            .text_color(t.line_number)
                            .child(label),
                    )
                },
            )
            .child(
                div()
                    .id("finder-results")
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    // 2px vertical gap between the rounded rows, matching their 2px side inset.
                    .gap(px(2.0))
                    .children(rows),
            )
            // Clicks inside the panel must not reach the backdrop (which would close the finder
            // right after a row's handler ran — breaking palette actions that re-open a finder).
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

        overlay(cx, true).child(modal).into_any_element()
    }

    // ── finder ────────────────────────────────────────────────────
    /// Debounced, background Find-in-Files. `git grep` on a large repo is far too expensive
    /// to run synchronously per keystroke (a 1-char query took ~20s / 29MB on a 2.7k-file
    /// repo and froze the UI). So: enforce a minimum query length, debounce keystroke bursts,
    /// run the grep off the UI thread, and apply results only if no newer keystroke
    /// superseded this one (generation check, like the diff-edit autosave).
    pub(crate) fn schedule_content_search(&mut self, cx: &mut Context<Self>) {
        let q = self.finder.query.read(cx).text().trim().to_string();
        self.finder.search_gen = self.finder.search_gen.wrapping_add(1);
        let gen = self.finder.search_gen;
        self.finder.selected = 0;
        // Too short → clear immediately, never grep (matches almost every line in the repo).
        if q.len() < CONTENT_MIN_QUERY {
            self.finder.content_results = Vec::new();
            cx.notify();
            return;
        }
        let Some(root) = self.repo_root.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(CONTENT_SEARCH_DEBOUNCE)
                .await;
            // Superseded by a newer keystroke during the debounce window? Skip the grep.
            if this
                .update(cx, |this, _| this.finder.search_gen)
                .unwrap_or(gen)
                != gen
            {
                return;
            }
            let hits = cx
                .background_executor()
                .spawn(async move {
                    Repo::discover(&root)
                        .map(|r| r.grep(&q))
                        .unwrap_or_default()
                })
                .await;
            this.update(cx, |this, cx| {
                // Apply only if still the latest search and the finder's still in Content mode.
                if this.finder.search_gen == gen
                    && this.finder.open
                    && this.finder.mode == FinderMode::Content
                {
                    this.finder.content_results = hits
                        .into_iter()
                        .map(|(path, line, text)| ContentHit { path, line, text })
                        .collect();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn recompute_finder(&mut self, cx: &Context<Self>) {
        let q = self.finder.query.read(cx).text().to_string();
        let matcher = SkimMatcherV2::default();
        self.finder.selected = 0;
        match self.finder.mode {
            FinderMode::Files => {
                let mut scored: Vec<(i64, &PathBuf)> = self
                    .browse
                    .all_files
                    .iter()
                    .filter_map(|p| {
                        let s = p.to_string_lossy();
                        if q.is_empty() {
                            Some((0, p))
                        } else {
                            matcher.fuzzy_match(&s, &q).map(|sc| (sc, p))
                        }
                    })
                    .collect();
                scored.sort_by_key(|x| std::cmp::Reverse(x.0));
                self.finder.results = scored
                    .into_iter()
                    .take(FINDER_RESULT_CAP)
                    .map(|(_, p)| p.clone())
                    .collect();
            }
            FinderMode::Actions => {
                // "Reveal in Finder/Terminal" only make sense with an active/selected file.
                let have_file =
                    self.browse.open_path.is_some() || self.browse.selected_path.is_some();
                let mut scored: Vec<(i64, usize)> = PALETTE
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, a, _))| {
                        have_file
                            || !matches!(
                                a,
                                PaletteAction::RevealInFinder | PaletteAction::RevealInTerminal
                            )
                    })
                    .filter_map(|(i, (label, _, _))| {
                        if q.is_empty() {
                            Some((0, i))
                        } else {
                            matcher.fuzzy_match(label, &q).map(|sc| (sc, i))
                        }
                    })
                    .collect();
                scored.sort_by_key(|x| std::cmp::Reverse(x.0));
                self.finder.action_results = scored.into_iter().map(|(_, i)| i).collect();
            }
            FinderMode::Content => {
                // Literal (non-fuzzy) full-text search via `git grep`. Empty query → no hits.
                self.finder.content_results = if q.is_empty() {
                    Vec::new()
                } else {
                    self.repo()
                        .map(|r| r.grep(&q))
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(path, line, text)| ContentHit { path, line, text })
                        .collect()
                };
            }
            FinderMode::Scratch => {
                let mut scored: Vec<(i64, usize)> = scratch::LANGS
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (label, _))| {
                        if q.is_empty() {
                            Some((0, i))
                        } else {
                            matcher.fuzzy_match(label, &q).map(|sc| (sc, i))
                        }
                    })
                    .collect();
                scored.sort_by_key(|x| std::cmp::Reverse(x.0));
                self.finder.action_results = scored.into_iter().map(|(_, i)| i).collect();
            }
        }
    }

    pub(crate) fn open_finder(
        &mut self,
        mode: FinderMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finder.mode = mode;
        self.finder.open = true;
        let placeholder = match mode {
            FinderMode::Files => "Type to search files…",
            FinderMode::Content => "Type to search file contents…",
            FinderMode::Actions => "Type to search actions…",
            FinderMode::Scratch => "Pick a language…",
        };
        self.finder.query.update(cx, |e, cx| {
            e.placeholder = placeholder.into();
            e.set_content(String::new(), Lang::PlainText, cx);
        });
        self.recompute_finder(cx);
        let handle = self.finder.query.read(cx).focus_handle.clone();
        window.focus(&handle);
        // First open: the input element isn't in the tree yet, so also focus next frame.
        window.defer(cx, move |window, _cx| window.focus(&handle));
        cx.notify();
    }

    pub(crate) fn act_go_to_file(
        &mut self,
        _: &GoToFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_finder(FinderMode::Files, window, cx);
    }

    pub(crate) fn act_find_in_files(
        &mut self,
        _: &FindInFiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_finder(FinderMode::Content, window, cx);
    }

    pub(crate) fn act_actions(&mut self, _: &Actions, window: &mut Window, cx: &mut Context<Self>) {
        self.open_finder(FinderMode::Actions, window, cx);
    }

    /// Open `rel` in the editor, switch to Browse, select the 1-based `line`, and scroll
    /// it near the top — used by the Find-in-Files results to jump straight to a match.
    pub(crate) fn open_file_at_line(
        &mut self,
        rel: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file(rel, cx);
        self.mode = Mode::Browse;
        let line0 = line.saturating_sub(1) as usize;
        self.browse.editor.update(cx, |e, cx| {
            // Compute the line's byte range first (immutable borrow), then select it.
            let range = {
                let text = e.text();
                let start: usize = text.split_inclusive('\n').take(line0).map(str::len).sum();
                let len = text[start..].find('\n').unwrap_or(text.len() - start);
                start..start + len
            };
            e.select_range(range, cx);
        });
        // Scroll so the target line sits a few rows below the top (negative offset = down).
        let lh = editor::line_height_px();
        let y = -(line0.saturating_sub(SCROLL_CONTEXT_ROWS) as f32) * lh;
        let sh = self.browse.editor_scroll.clone();
        window.defer(cx, move |_w, _cx| {
            sh.set_offset(gpui::point(gpui::px(0.0), gpui::px(y)));
        });
        window.focus(&self.focus_handle);
        cx.notify();
    }

    pub(crate) fn run_palette(
        &mut self,
        a: PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finder.open = false;
        match a {
            PaletteAction::GoToFile => self.open_finder(FinderMode::Files, window, cx),
            PaletteAction::FindInFiles => self.open_finder(FinderMode::Content, window, cx),
            PaletteAction::NewScratch => self.open_finder(FinderMode::Scratch, window, cx),
            PaletteAction::CommitView => self.enter_commit(cx),
            PaletteAction::BrowseView => {
                self.mode = Mode::Browse;
                window.focus(&self.focus_handle);
                cx.notify();
            }
            PaletteAction::SelectInTree => {
                window.focus(&self.focus_handle);
                self.reveal_in_tree(cx);
            }
            PaletteAction::Rollback => {
                window.focus(&self.focus_handle);
                self.open_rollback_path(PathBuf::new(), cx);
            }
            PaletteAction::Settings => {
                self.onboarding.choice = self.keymap.preset;
                self.onboarding.open = true;
                window.focus(&self.focus_handle);
                cx.notify();
            }
            PaletteAction::RevealInFinder => {
                window.focus(&self.focus_handle);
                if let Some(p) = self
                    .browse
                    .open_path
                    .clone()
                    .or_else(|| self.browse.selected_path.clone())
                {
                    self.reveal_in_os(&p, cx);
                }
            }
            PaletteAction::RevealInTerminal => {
                window.focus(&self.focus_handle);
                if let Some(p) = self
                    .browse
                    .open_path
                    .clone()
                    .or_else(|| self.browse.selected_path.clone())
                {
                    self.reveal_in_terminal(&p, cx);
                }
            }
            PaletteAction::Plugins => {
                self.open_modal_window(ModalKind::Plugins, "Language Plugins", 520.0, 560.0, cx);
            }
            PaletteAction::Fonts => {
                self.open_modal_window(ModalKind::Fonts, "Fonts", 760.0, 620.0, cx);
            }
            PaletteAction::SortLines => {
                window.focus(&self.focus_handle);
                self.sort_selected_lines(cx);
            }
            PaletteAction::SortKeys => {
                window.focus(&self.focus_handle);
                self.sort_object_keys_at_caret(cx);
            }
            PaletteAction::NavBack => {
                window.focus(&self.focus_handle);
                self.nav_back(cx);
            }
            PaletteAction::NavForward => {
                window.focus(&self.focus_handle);
                self.nav_forward(cx);
            }
        }
    }

    pub(crate) fn finder_close(
        &mut self,
        _: &FinderClose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finder.open = false;
        window.focus(&self.focus_handle);
        cx.notify();
    }

    pub(crate) fn finder_up(&mut self, _: &FinderUp, _: &mut Window, cx: &mut Context<Self>) {
        self.finder.selected = ui::picker::nav_up(self.finder.selected);
        cx.notify();
    }

    pub(crate) fn finder_down(&mut self, _: &FinderDown, _: &mut Window, cx: &mut Context<Self>) {
        let len = match self.finder.mode {
            FinderMode::Files => self.finder.results.len(),
            FinderMode::Content => self.finder.content_results.len(),
            FinderMode::Actions | FinderMode::Scratch => self.finder.action_results.len(),
        };
        self.finder.selected = ui::picker::nav_down(self.finder.selected, len);
        cx.notify();
    }

    pub(crate) fn finder_confirm(
        &mut self,
        _: &FinderConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.finder.mode {
            FinderMode::Files => {
                if let Some(p) = self.finder.results.get(self.finder.selected).cloned() {
                    self.open_file(p, cx);
                    self.mode = Mode::Browse;
                }
                self.finder.open = false;
                window.focus(&self.focus_handle);
                cx.notify();
            }
            FinderMode::Content => {
                if let Some(hit) = self
                    .finder
                    .content_results
                    .get(self.finder.selected)
                    .cloned()
                {
                    self.finder.open = false;
                    self.open_file_at_line(hit.path, hit.line, window, cx);
                } else {
                    self.finder.open = false;
                    window.focus(&self.focus_handle);
                }
                cx.notify();
            }
            FinderMode::Actions => {
                if let Some(&i) = self.finder.action_results.get(self.finder.selected) {
                    self.run_palette(PALETTE[i].1, window, cx);
                } else {
                    self.finder.open = false;
                    window.focus(&self.focus_handle);
                    cx.notify();
                }
            }
            FinderMode::Scratch => {
                let ext = self
                    .finder
                    .action_results
                    .get(self.finder.selected)
                    .map(|&i| scratch::LANGS[i].1);
                self.finder.open = false;
                window.focus(&self.focus_handle);
                if let Some(ext) = ext {
                    self.create_scratch(ext, cx);
                } else {
                    cx.notify();
                }
            }
        }
    }
}
