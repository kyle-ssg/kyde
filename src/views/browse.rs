//! Browse (code) view — file tree, editor pane, markdown preview, font preview, and the
//! open-file / preview-tab logic. Crate-root child module.

use crate::*;

impl Kyde {
    /// The flattened Browse tree rows: the repo root, its expanded descendants (only when
    /// the root is open), then a virtual "Scratches" folder + its files at the bottom.
    /// Depths are offset by one so everything nests under the root row. The "Scratches"
    /// sentinel path can't collide with a real file.
    fn browse_visible_rows(&self) -> Vec<tree::Row> {
        let mut visible = vec![tree::Row {
            path: PathBuf::new(),
            is_dir: true,
            depth: 0,
        }];
        if self.browse.expanded.contains(&PathBuf::new()) {
            for mut r in self.browse.tree.visible(&self.browse.expanded) {
                r.depth += 1;
                visible.push(r);
            }
        }
        let scratch_group = scratch_group_path();
        if !self.browse.scratches.is_empty() {
            visible.push(tree::Row {
                path: scratch_group.clone(),
                is_dir: true,
                depth: 0,
            });
            if self.browse.expanded.contains(&scratch_group) {
                for s in &self.browse.scratches {
                    visible.push(tree::Row {
                        path: s.clone(),
                        is_dir: false,
                        depth: 1,
                    });
                }
            }
        }
        visible
    }

    /// The Browse right-hand pane: the no-file shortcuts screen when no tab is open,
    /// otherwise the tab bar + optional install/find banners + the editor area (inline
    /// image preview, font view, code editor, or the editor | markdown-preview split).
    fn render_editor_pane(
        &mut self,
        ui: &'static str,
        fs: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let right = if self.browse.open_tabs.is_empty() {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .child(self.render_no_file(ui, cx))
        } else {
            let image = self.browse.open_path.clone().filter(|p| is_image(p));
            let editor_area = if let Some(rel) = &image {
                // Inline image preview, centered, scaled to fit the pane.
                let abs = self
                    .repo_root
                    .as_ref()
                    .map_or_else(|| rel.clone(), |r| r.join(rel));
                div()
                    .id("image-scroll")
                    .overflow_scroll()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_4()
                    .child(img(abs).max_w_full().max_h_full())
                    .into_any_element()
            } else if self
                .browse
                .open_path
                .as_ref()
                .is_some_and(|p| is_font_file(p))
            {
                self.render_font_view(cx).into_any_element()
            } else {
                // Outer flex item is `flex_1 min_w_0 overflow_hidden` — it shrinks to its flex
                // share regardless of the (content-wide) editor element inside. The INNER
                // `overflow_scroll` div holds the wide editor and scrolls it. Decoupling the
                // two is what lets a side-by-side pane (markdown preview) keep its 50%: a
                // content-wide element directly on the flex item refuses to shrink below it.
                // Single scroll div holding the content-wide editor element — the explicit width
                // (set in `editor_with_scrollbars`) goes on THIS div, the same one that has
                // `overflow_scroll` and the editor as a direct child. (An extra wrapper level
                // around it lets the content's min-width win and the pane refuses to shrink.)
                // The editor sits inside an explicit-width wrapper (its content width) so long
                // lines overflow this `overflow_scroll` viewport and the horizontal scrollbar
                // appears; `min_h_full` keeps the click target covering short files.
                let content_w = self.browse.editor.read(cx).content_width();
                let editor_pane = div()
                    .id("editor-scroll")
                    .overflow_scroll()
                    .track_scroll(&self.browse.editor_scroll)
                    // A click in the empty area below a short file forwards to the editor,
                    // so the caret jumps to the last line (the editor consumes its own text
                    // clicks via `stop_propagation`, so this only fires for the gap).
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                            let fe = this.browse.editor.clone();
                            fe.update(cx, |ed, cx| {
                                ed.click_at(e.position, e.modifiers.shift, window, cx);
                            });
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(content_w))
                            .min_h_full()
                            .child(self.browse.editor.clone()),
                    );
                let island_w = self.editor_island_w(window);
                let editor = self.with_scrollbars(
                    editor_pane,
                    self.browse.editor_scroll.clone(),
                    island_w,
                    SbView::Editor,
                    window,
                    cx,
                );
                // Markdown + the markdown plugin installed → editor | live preview side by side.
                let md = self
                    .browse
                    .open_path
                    .as_ref()
                    .is_some_and(|p| matches!(Lang::from_path(p), Lang::Markdown))
                    && self.plugins.is_installed("markdown");
                if md {
                    let text = self.browse.editor.read(cx).text().to_string();
                    // Two side-by-side panes, each with its own reusable scrollbars: the code
                    // editor (left, `md_editor_w` wide, drag-resizable divider) and the rendered
                    // preview (right, the remaining width). Each is wrapped in an explicit-width
                    // container so the split keeps its proportions.
                    let preview_w =
                        (island_w - self.browse.md_editor_w - theme::FRAME_GAP).max(120.0);
                    let code_pane = div()
                        .id("md-editor-scroll")
                        .overflow_scroll()
                        .track_scroll(&self.browse.md_editor_scroll)
                        .child(
                            div()
                                .flex_none()
                                .w(px(content_w))
                                .min_h_full()
                                .child(self.browse.editor.clone()),
                        );
                    // `flex flex_col` is load-bearing: the `with_scrollbars` output is `flex_1`,
                    // which only resolves to the pane height when its parent is a flex column.
                    // Without it the pane grew to content height → never overflowed → no thumb.
                    let left = div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .w(px(self.browse.md_editor_w))
                        .h_full()
                        .min_h_0()
                        .child(self.with_scrollbars(
                            code_pane,
                            self.browse.md_editor_scroll.clone(),
                            self.browse.md_editor_w,
                            SbView::MdEditor,
                            window,
                            cx,
                        ));
                    // Persistent selectable rendered-markdown view (keeps its text selection
                    // across frames); re-parses only when the source actually changes.
                    // Directory of the open markdown file, for resolving relative image paths.
                    let base_dir = self.browse.open_path.as_ref().and_then(|p| {
                        self.repo_root
                            .as_ref()
                            .map(|r| r.join(p))
                            .and_then(|abs| abs.parent().map(std::path::Path::to_path_buf))
                    });
                    let mv = self
                        .browse
                        .md_view
                        .get_or_insert_with(|| {
                            cx.new(|cx| mdview::MarkdownView::new(&text, base_dir.clone(), cx))
                        })
                        .clone();
                    mv.update(cx, |v, cx| v.set_text(&text, base_dir.clone(), cx));
                    let preview_pane = div()
                        .id("md-preview-scroll")
                        .overflow_y_scroll()
                        .track_scroll(&self.browse.md_preview_scroll)
                        .child(mv);
                    let right = div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .w(px(preview_w))
                        .h_full()
                        .min_h_0()
                        .child(self.with_scrollbars(
                            preview_pane,
                            self.browse.md_preview_scroll.clone(),
                            preview_w,
                            SbView::MdPreview,
                            window,
                            cx,
                        ));
                    div()
                        .id("md-split")
                        .flex_1()
                        .flex()
                        .flex_row()
                        .min_h_0()
                        .child(left)
                        .child(
                            div()
                                .id("md-divider")
                                .w(px(theme::FRAME_GAP))
                                .flex_none()
                                .h_full()
                                .cursor_col_resize()
                                .bg(theme::get().divider)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                                        this.start_divider_drag(
                                            Divider::MdSplit,
                                            e.position,
                                            window,
                                        );
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(right)
                        .into_any_element()
                } else {
                    editor
                }
            };
            // Right-click in the editor pane: git commands (Commit / Rollback / Push)
            // plus the buffer sort ops — Sort Lines when the selection spans ≥2 lines,
            // Sort Object Keys when the click lands in a JSON/JS/TS object. Availability
            // is computed HERE (menu-open), not in the render arm, so an open menu never
            // re-parses the buffer per frame.
            let menu_path = self.browse.open_path.clone();
            let editor_area = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(editor_area)
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                        if let Some(p) = menu_path.clone() {
                            // Caret under the pointer first (kept if clicking inside the
                            // selection), so the sort ops target what was clicked.
                            this.browse
                                .editor
                                .update(cx, |ed, cx| ed.caret_to(e.position, cx));
                            let (lines, keys) = {
                                let ed = this.browse.editor.read(cx);
                                let sel = ed.selection();
                                let keys =
                                    highlight::sort_object_keys(ed.text(), ed.lang, sel.clone())
                                        .is_some();
                                // Sort Lines also shows inside an object — there it
                                // delegates to the key sort (issue #43).
                                let lines =
                                    keys || editor::sort_lines_in(ed.text(), sel.clone()).is_some();
                                (lines, keys)
                            };
                            this.open_menu(e.position, MenuTarget::EditorGit(p, lines, keys), cx);
                        }
                    }),
                );
            let mut r = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                // Without min_w_0 the wide editor content (long code lines) lets this column
                // grow past the island, and the island's overflow_hidden then clips its right
                // edge — which is exactly where the tab-bar's trailing controls live, so the
                // `▾` overflow button vanished once tabs/content got wide.
                .min_w_0()
                .child(self.render_tab_bar(ui, fs, cx));
            // Install banner only applies to text/code files, not image previews.
            if image.is_none() {
                // Unresolved-conflict banner first — resolving beats installing a grammar.
                if let Some(p) = self.open_file_conflict() {
                    r = r.child(self.render_conflict_file_banner(p, ui, fs, cx));
                }
                if let Some(pack) = self.pending_pack() {
                    r = r.child(self.render_install_banner(pack, ui, fs, cx));
                }
                if self.find.open {
                    r = r.child(self.render_find_bar(ui, cx));
                }
            }
            r.child(editor_area)
        };
        right.into_any_element()
    }

    pub(crate) fn render_browse(
        &mut self,
        ui: &'static str,
        fs: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // Row-hover suppression during a tree resize now lives in `tree_row` (it reads
        // `dragging(Divider::Tree)`), so the cursor sweeping over rows mid-drag doesn't flicker.
        // Show the repo root as the top row; everything else nests one level under it,
        // and collapsing the root hides the whole tree.
        let root_name = self
            .repo_root
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or_else(|| "/".to_string(), |s| s.to_string_lossy().into_owned());
        let visible = self.browse_visible_rows();
        let scratch_group = scratch_group_path();
        // O(1) status lookup per row (was `self.files.iter().find()` — O(changed) per row,
        // i.e. O(rows×changed) for the whole tree on every frame).
        let status_by_path: std::collections::HashMap<&PathBuf, FileStatus> =
            self.files.iter().map(|f| (&f.path, f.status)).collect();
        let rows: Vec<gpui::AnyElement> = visible
            .into_iter()
            .map(|r| {
                let is_root = r.path.as_os_str().is_empty();
                let name: SharedString = if is_root {
                    root_name.clone().into()
                } else if r.path == scratch_group {
                    "Scratches".into()
                } else {
                    r.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into()
                };
                let expanded = self.browse.expanded.contains(&r.path);
                let selected = self.browse.selected_path.as_ref() == Some(&r.path)
                    || self.browse.multi_selected.contains(&r.path);
                let (p_act, p_ctx) = (r.path.clone(), r.path.clone());
                let is_dir = r.is_dir;
                // Color changed files by their git status (modified/added/…), matching the
                // commit view; unchanged files use the normal text color.
                let name_color = (!is_dir)
                    .then(|| status_by_path.get(&r.path))
                    .flatten()
                    .map_or(theme::get().text, |&s| status_color(s));
                ui::tree::item(
                    cx,
                    self.dragging(Divider::Tree),
                    &r.path,
                    is_dir,
                    expanded,
                    r.depth,
                    selected,
                    name,
                    name_color,
                    None,
                    None,
                    move |this, e, window, cx| {
                        // Cmd-click on a FILE toggles it in the multi-selection (for
                        // "Compare Selected" — issue #42) without opening it. The
                        // previously highlighted file seeds the set, so cmd-clicking a
                        // second file selects the natural pair.
                        if !is_dir && e.modifiers.platform {
                            if let Some(i) =
                                this.browse.multi_selected.iter().position(|p| p == &p_act)
                            {
                                this.browse.multi_selected.remove(i);
                            } else {
                                if this.browse.multi_selected.is_empty() {
                                    if let Some(prev) =
                                        this.browse.selected_path.clone().filter(|p| {
                                            p != &p_act
                                                && (this.browse.all_files.contains(p)
                                                    || this.browse.scratches.contains(p))
                                        })
                                    {
                                        this.browse.multi_selected.push(prev);
                                    }
                                }
                                this.browse.multi_selected.push(p_act.clone());
                            }
                            this.browse.selected_path = Some(p_act.clone());
                            cx.notify();
                            return;
                        }
                        // Folders toggle expansion. Files (VS Code-style): single click opens in
                        // the temporary *preview* tab (reused by the next single-click), double
                        // click opens a permanent tab.
                        this.browse.multi_selected.clear();
                        this.browse.selected_path = Some(p_act.clone());
                        // Focus the app root so the "Kyde"-context Backspace (delete) binding
                        // is live on the selected row. (open_file/preview_file re-focus the
                        // editor below, which is what we want there.)
                        window.focus(&this.focus_handle);
                        if is_dir {
                            this.toggle_dir(p_act.clone(), cx);
                        } else if e.click_count >= 2 {
                            this.open_file(p_act.clone(), cx);
                        } else {
                            this.preview_file(p_act.clone(), cx);
                        }
                        cx.notify();
                    },
                    |_this, _cx| {},
                    move |this, pos, cx| {
                        this.open_menu(pos, MenuTarget::BrowseFile(p_ctx.clone(), is_dir), cx);
                    },
                )
            })
            .collect();
        let t = theme::get();
        // Header strip with a `−` minimize button at the top-right.
        // Collapse button — absolutely positioned in the tree's top-right corner so it floats
        // over the rows instead of consuming a whole header line.
        let tree_minimize = div()
            .id("tree-minimize")
            .absolute()
            .top(px(4.0))
            // Clear the 12px scrollbar gutter on the right so the bar never sits under it.
            .right(px(8.0))
            .flex()
            .items_center()
            .justify_center()
            .size(px(22.0))
            .rounded_md()
            .text_size(px(16.0))
            .text_color(t.line_number)
            .bg(t.panel_bg)
            .hover(|s| s.bg(t.bg_light).text_color(t.text))
            .cursor_pointer()
            .child("−")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.browse.tree_collapsed = true;
                    cx.notify();
                }),
            );
        let tree = div()
            .flex()
            .flex_col()
            .relative()
            .w(px(self.browse.tree_width))
            .flex_none()
            .h_full()
            .py_1()
            .bg(t.panel_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .text_color(t.text)
            .font_family(ui)
            // A touch larger than the editor/code size for readability.
            .text_size(px(theme::get().ui_font_size + 3.0))
            .child({
                let rows_pane = div()
                    .id("browse-tree")
                    .overflow_y_scroll()
                    .track_scroll(&self.browse.tree_scroll)
                    .flex()
                    .flex_col()
                    .children(rows);
                self.with_scrollbars(
                    rows_pane,
                    self.browse.tree_scroll.clone(),
                    self.browse.tree_width,
                    SbView::Tree,
                    window,
                    cx,
                )
            })
            .child(tree_minimize);
        // Collapsed: a thin strip with an expand button where the tree was.
        let collapsed_strip = div()
            .flex_none()
            .h_full()
            .w(px(30.0))
            .py_1()
            .bg(t.panel_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .id("tree-expand")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.0))
                    .rounded_md()
                    .text_size(px(15.0))
                    .text_color(t.line_number)
                    .hover(|s| s.bg(t.bg_light).text_color(t.text))
                    .cursor_pointer()
                    .child("»")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.browse.tree_collapsed = false;
                            cx.notify();
                        }),
                    ),
            );

        // The frame-colored gap between the two islands doubles as the resize handle.
        // No hover/active tint — just the resize cursor.
        let divider = div()
            .id("browse-divider")
            .w(px(theme::FRAME_GAP))
            .h_full()
            .flex_none()
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                    this.start_divider_drag(Divider::Tree, e.position, window);
                    cx.notify();
                }),
            );

        // No tabs open → show useful shortcuts instead of the empty editor.
        let right = self.render_editor_pane(ui, fs, window, cx);

        let editor_island = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            // `relative` so the floated tab-overflow button anchors to the island's right
            // edge; `overflow_hidden` keeps it (and the tab strip) clipped to the island.
            .bg(theme::get().main_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .overflow_hidden()
            .child(right);

        let mut row = div().flex().flex_row().flex_1();
        row = if self.browse.tree_collapsed {
            row.child(collapsed_strip)
                .child(div().w(px(theme::FRAME_GAP)).flex_none().h_full())
        } else {
            row.child(tree).child(divider)
        };
        row.child(editor_island).into_any_element()
    }

    fn render_font_view(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let t = theme::get();
        let base = div()
            .id("font-view")
            .overflow_y_scroll()
            .flex_1()
            .flex()
            .flex_col()
            .bg(t.main_bg);
        if !self.plugins.is_installed("font") {
            return base
                .items_center()
                .justify_center()
                .gap_3()
                .font_family(theme::font::UI_FAMILY)
                .child(
                    div()
                        .text_color(t.text)
                        .child("Install Font Preview support to view this file."),
                )
                .child(
                    btn_primary("install-font", "Install Font Preview").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.install_pack("font", cx)),
                    ),
                );
        }
        let Some((_, family)) = self.font_preview.clone() else {
            return base
                .items_center()
                .justify_center()
                .font_family(theme::font::UI_FAMILY)
                .child(
                    div()
                        .text_color(t.line_number)
                        .child("Could not read this font file."),
                );
        };
        let quote = "Me fail English? That's unpossible.";
        let fam = family.clone();
        let line = move |sz: f32| {
            div()
                .font_family(fam.clone())
                .text_size(px(sz))
                .text_color(t.text)
                .child(quote)
        };
        base.p_6()
            .gap_3()
            .child(
                div()
                    .font_family(theme::font::UI_FAMILY)
                    .text_color(t.line_number)
                    .text_size(px(12.0))
                    .child(family.clone()),
            )
            .child(line(13.0))
            .child(line(18.0))
            .child(line(24.0))
            .child(line(32.0))
            .child(line(48.0))
            .child(
                div()
                    .font_family(family)
                    .text_size(px(20.0))
                    .text_color(t.secondary_text)
                    .child("ABCDEFG abcdefg 0123456789 !?&@#"),
            )
    }

    /// Rendered Markdown preview (right pane of the side-by-side markdown view). Live —
    /// re-renders from the editor's current text each frame.
    // Superseded by the selectable `mdview::MarkdownView`, but kept (another change adds image
    // rendering here); left in place rather than deleted.
    #[allow(dead_code)]
    fn render_markdown_preview(&self, text: &str) -> gpui::Stateful<gpui::Div> {
        let t = theme::get();
        let mk_font = |bold: bool, italic: bool, code: bool| gpui::Font {
            family: if code {
                theme::font::FAMILY
            } else {
                theme::font::UI_FAMILY
            }
            .into(),
            features: Default::default(),
            fallbacks: None,
            weight: if bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if italic {
                gpui::FontStyle::Italic
            } else {
                gpui::FontStyle::Normal
            },
        };
        let styled = move |spans: &[markdown::Span], color: gpui::Hsla| -> gpui::StyledText {
            let mut s = String::new();
            let mut runs = Vec::new();
            for sp in spans {
                let c: gpui::Hsla = if sp.code {
                    gpui::rgb(0xC9CDD6).into()
                } else {
                    color
                };
                runs.push(gpui::TextRun {
                    len: sp.text.len(),
                    font: mk_font(sp.bold, sp.italic, sp.code),
                    color: c,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
                s.push_str(&sp.text);
            }
            gpui::StyledText::new(SharedString::from(s)).with_runs(runs)
        };
        let blocks = markdown::parse(text).into_iter().map(|b| match b {
            markdown::Block::Heading(level, spans) => {
                let size = match level {
                    1 => 26.0,
                    2 => 21.0,
                    3 => 18.0,
                    _ => 15.0,
                };
                div()
                    .pt_2()
                    .text_size(px(size))
                    .child(styled(&spans, t.text.into()))
                    .into_any_element()
            }
            markdown::Block::Paragraph(spans) => div()
                .text_size(px(14.0))
                .child(styled(&spans, t.text.into()))
                .into_any_element(),
            markdown::Block::Code(code) => div()
                .font_family(theme::font::FAMILY)
                .text_size(px(13.0))
                .text_color(t.text)
                .bg(t.bg_mid)
                .rounded_md()
                .p_3()
                .child(SharedString::from(code))
                .into_any_element(),
            markdown::Block::ListItem(depth, spans) => div()
                .flex()
                .flex_row()
                .pl(px(depth as f32 * 14.0))
                .text_size(px(14.0))
                .child(div().pr_2().text_color(t.line_number).child("•"))
                .child(styled(&spans, t.text.into()))
                .into_any_element(),
            markdown::Block::Quote(spans) => div()
                .border_l_2()
                .border_color(t.divider)
                .pl_3()
                .text_size(px(14.0))
                .child(styled(&spans, t.secondary_text.into()))
                .into_any_element(),
            markdown::Block::Rule => div().h(px(1.0)).bg(t.divider).my_2().into_any_element(),
            markdown::Block::Image { src, .. } => {
                // Remote (http/https) → loaded via gpui's asset loader, which needs an
                // HttpClient wired at startup — only present under the `remote-images`
                // feature; without it the fetch fails and nothing renders (no crash).
                // Local → resolved relative to the open file's directory, then the repo
                // root, and read straight off disk (no deps, always works).
                let source: gpui::ImageSource =
                    if src.starts_with("http://") || src.starts_with("https://") {
                        src.clone().into()
                    } else {
                        let rel = src.trim_start_matches("./");
                        let joined = match self.browse.open_path.as_ref().and_then(|p| p.parent()) {
                            Some(dir) => dir.join(rel),
                            None => PathBuf::from(rel),
                        };
                        self.repo_root
                            .as_ref()
                            .map(|r| r.join(&joined))
                            .unwrap_or(joined)
                            .into()
                    };
                div()
                    .py_1()
                    .child(img(source).max_w_full().max_h(px(480.0)))
                    .into_any_element()
            }
        });
        div()
            .id("md-preview")
            .overflow_y_scroll()
            .track_scroll(&self.browse.md_preview_scroll)
            .flex()
            .flex_col()
            .gap_2()
            .p_5()
            .bg(t.main_bg)
            .font_family(theme::font::UI_FAMILY)
            .children(blocks)
    }

    /// Expand/collapse a directory in the Browse tree.
    pub(crate) fn toggle_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if !self.browse.expanded.remove(&dir) {
            self.browse.expanded.insert(dir);
        }
        cx.notify();
    }

    /// "Select Opened File in Tree" (IntelliJ-style): switch to Browse, expand
    /// every ancestor of the active file, select its row, and scroll it into
    /// view. Falls back to the highlighted row if no file is open.
    pub(crate) fn reveal_in_tree(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self
            .browse
            .open_path
            .clone()
            .or_else(|| self.browse.selected_path.clone())
        else {
            return;
        };
        self.mode = Mode::Browse;
        // Expand every ancestor dir (incl. the root `""`) so the row is visible.
        for anc in target.ancestors().skip(1) {
            self.browse.expanded.insert(anc.to_path_buf());
        }
        self.browse.selected_path = Some(target.clone());
        // Find the target's index in the same flattened order render_browse uses
        // (root row, then tree rows, then scratches) and scroll it into view.
        let mut idx = if target.as_os_str().is_empty() {
            Some(0)
        } else {
            None
        };
        if idx.is_none() {
            let mut i = 1usize;
            for r in self.browse.tree.visible(&self.browse.expanded) {
                if r.path == target {
                    idx = Some(i);
                    break;
                }
                i += 1;
            }
            if idx.is_none() {
                for s in &self.browse.scratches {
                    if *s == target {
                        idx = Some(i);
                        break;
                    }
                    i += 1;
                }
            }
        }
        if let Some(i) = idx {
            self.browse.tree_scroll.scroll_to_item(i);
        }
        cx.notify();
    }

    /// Open `rel` as a permanent tab (double-click, finder, "Open", restore, …). If it was
    /// the preview tab, it's promoted (the preview slot clears).
    pub(crate) fn open_file(&mut self, rel: PathBuf, cx: &mut Context<Self>) {
        self.open_file_inner(rel, false, cx);
    }

    /// Open `rel` as the VS Code-style *preview* (temporary) tab: a single-click in the tree.
    /// Reuses the one preview slot — a subsequent single-click on a different file replaces it
    /// in place rather than opening a new tab. Clicking a file that's already open as a
    /// permanent tab just activates it (no demotion).
    pub(crate) fn preview_file(&mut self, rel: PathBuf, cx: &mut Context<Self>) {
        self.open_file_inner(rel, true, cx);
    }

    fn open_file_inner(&mut self, rel: PathBuf, preview: bool, cx: &mut Context<Self>) {
        // Images preview via `img()` and font files preview in their own typeface (see
        // render_browse) — don't load their binary bytes into the text editor.
        if !is_image(&rel) && !is_font_file(&rel) {
            // Scratch files live outside the repo (absolute paths) — read them straight
            // from disk. Repo-relative files go through the repo's working tree when this is
            // a git repo, else straight from disk under the project root (non-git Browse).
            let content = if rel.is_absolute() {
                std::fs::read_to_string(&rel).unwrap_or_default()
            } else if let Some(repo) = self.repo() {
                repo.working_content(&rel).ok().unwrap_or_default()
            } else if let Some(root) = self.repo_root.as_ref() {
                std::fs::read_to_string(root.join(&rel)).unwrap_or_default()
            } else {
                String::new()
            };
            let lang = self.effective_lang(&rel);
            let errs = self.errors_enabled_for(lang);
            self.browse.editor.update(cx, |e, cx| {
                e.line_numbers = true;
                // Flag first so `set_content`'s recompute already parses (or skips)
                // errors for the new file's opt-in.
                e.set_error_highlight(errs, cx);
                e.set_content(content, lang, cx);
            });
            // Point the editor at whichever scroll container it renders in, so caret-follow
            // and drag auto-scroll move the right one: the Markdown split uses
            // `md_editor_scroll`, plain Browse uses `file_scroll` (mirrors the `md` gate in
            // render_browse).
            let md = matches!(highlight::Lang::from_path(&rel), highlight::Lang::Markdown)
                && self.plugins.is_installed("markdown");
            let h = if md {
                self.browse.md_editor_scroll.clone()
            } else {
                self.browse.editor_scroll.clone()
            };
            self.browse.editor.update(cx, |e, _| e.set_scroll_handle(h));
        }
        self.browse.selected_path = Some(rel.clone());
        if self.browse.open_tabs.contains(&rel) {
            // Already open. A permanent open promotes it out of the preview slot; a preview
            // open of an already-permanent tab leaves its permanence alone (just activates).
            if !preview && self.browse.preview_tab.as_ref() == Some(&rel) {
                self.browse.preview_tab = None;
            }
        } else if preview {
            // Reuse the single preview slot: replace its path in place if one exists, else
            // append. The replaced file's tab vanishes — exactly one temporary tab at a time.
            match self
                .browse
                .preview_tab
                .take()
                .and_then(|prev| self.browse.open_tabs.iter().position(|t| t == &prev))
            {
                Some(i) => self.browse.open_tabs[i] = rel.clone(),
                None => self.browse.open_tabs.push(rel.clone()),
            }
            self.browse.preview_tab = Some(rel.clone());
        } else {
            self.browse.open_tabs.push(rel.clone());
        }
        self.browse.open_path = Some(rel);
        // Scroll the (possibly off-screen) active tab into view on next paint.
        if let Some(i) = self
            .browse
            .open_path
            .as_ref()
            .and_then(|p| self.browse.open_tabs.iter().position(|t| t == p))
        {
            self.browse.tab_scroll.scroll_to_item(i);
        }
        self.load_font_preview(cx);
    }

    /// If the open file is a font and the "font" plugin is installed, parse its family name
    /// and register it with the text system so the preview pane can render it. Otherwise
    /// clears the cached preview. Cheap + idempotent (skips re-registering the same path).
    pub(crate) fn load_font_preview(&mut self, cx: &mut Context<Self>) {
        let Some(rel) = self.browse.open_path.clone().filter(|p| is_font_file(p)) else {
            self.font_preview = None;
            return;
        };
        if !self.plugins.is_installed("font") {
            self.font_preview = None;
            return;
        }
        if self.font_preview.as_ref().is_some_and(|(p, _)| *p == rel) {
            return; // already loaded
        }
        let abs = self
            .repo_root
            .as_ref()
            .map_or_else(|| rel.clone(), |r| r.join(&rel));
        let Ok(bytes) = std::fs::read(&abs) else {
            self.font_preview = None;
            return;
        };
        let Some(family) = font_family_name(&bytes) else {
            self.font_preview = None;
            return;
        };
        // Register the face so `.font_family(family)` resolves to it (idempotent in gpui).
        let _ = cx
            .text_system()
            .add_fonts(vec![std::borrow::Cow::Owned(bytes)]);
        self.font_preview = Some((rel, SharedString::from(family)));
    }

    // ── sort ops (issues #43 / #41) ───────────────────────────────
    /// Sort the Browse editor's selected lines alphabetically — but inside a
    /// JSON/JS/TS object "sort lines" MEANS "sort this object's keys" (issue
    /// #43): a textual line sort there would move entries without their commas
    /// and break the syntax, so it delegates to the key sort. Undo-recorded via
    /// `replace_range_text`; the sorted block stays selected. No-op outside
    /// Browse, with no file open, or (textual path) under two selected lines.
    pub(crate) fn sort_selected_lines(&mut self, cx: &mut Context<Self>) {
        if self.mode != Mode::Browse || self.browse.open_path.is_none() {
            return;
        }
        if self.object_sort(cx) {
            return;
        }
        let Some((r, sorted)) = ({
            let ed = self.browse.editor.read(cx);
            editor::sort_lines_in(ed.text(), ed.selection())
        }) else {
            return;
        };
        if self.browse.editor.read(cx).text()[r.clone()] == sorted {
            return; // already sorted — don't dirty the buffer
        }
        self.browse.editor.update(cx, |e, cx| {
            let end = r.start + sorted.len();
            e.replace_range_text(r.clone(), &sorted, cx);
            e.select_range(r.start..end, cx);
        });
    }

    /// Sort the keys of the JSON/JS/TS object at the Browse editor's caret,
    /// recursively (see `kyde_syntax::sort_object_keys`). Same guards + undo/
    /// selection behavior as [`sort_selected_lines`](Self::sort_selected_lines).
    pub(crate) fn sort_object_keys_at_caret(&mut self, cx: &mut Context<Self>) {
        if self.mode != Mode::Browse || self.browse.open_path.is_none() {
            return;
        }
        self.object_sort(cx);
    }

    /// Key-sort the object at the caret if there is one. Returns whether an
    /// object was FOUND — found-but-already-sorted still counts, so
    /// `sort_selected_lines` never falls back to a syntax-breaking textual sort
    /// inside JSON/JS.
    fn object_sort(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((r, text)) = ({
            let ed = self.browse.editor.read(cx);
            highlight::sort_object_keys(ed.text(), ed.lang, ed.selection())
        }) else {
            return false;
        };
        if self.browse.editor.read(cx).text()[r.clone()] != text {
            self.browse.editor.update(cx, |e, cx| {
                let end = r.start + text.len();
                e.replace_range_text(r.clone(), &text, cx);
                e.select_range(r.start..end, cx);
            });
        }
        true
    }

    pub(crate) fn act_sort_lines(
        &mut self,
        _: &SortLines,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sort_selected_lines(cx);
    }

    pub(crate) fn act_sort_keys(
        &mut self,
        _: &SortKeys,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sort_object_keys_at_caret(cx);
    }
}
