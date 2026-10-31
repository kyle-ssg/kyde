//! Right-click context menus: the entry model (built once, when the menu OPENS — never per
//! frame), the panel renderer, and keyboard navigation (issue #81): ↑/↓ move the highlight,
//! → or ⏎ open a "▸" submenu, ← backs out of it, ⏎ / space run the highlighted item, ⎋
//! closes. Mouse hover drives the same highlight, so there's only ever one lit row.
//! Crate-root child module.

use crate::*;
use std::rc::Rc;

/// What a menu item does when picked (click or keyboard).
pub(crate) type MenuRun = Rc<dyn Fn(&mut Kyde, &mut Window, &mut Context<Kyde>)>;

/// One row of a context menu.
#[derive(Clone)]
pub(crate) enum MenuEntry {
    /// A pickable row. `file` swaps the label-derived icon for that file's type badge, and
    /// `dim` mutes the text (both used by the tab list).
    Item {
        label: SharedString,
        file: Option<PathBuf>,
        dim: bool,
        run: MenuRun,
    },
    /// A greyed, unpickable note ("Nothing to revert to").
    Note(SharedString),
    /// A hairline between groups.
    Divider,
    /// A "<label> ▸" row with its own flyout of entries. `icon_label` picks the icon.
    Submenu {
        kind: Submenu,
        label: SharedString,
        icon_label: SharedString,
        items: Vec<MenuEntry>,
    },
}

impl MenuEntry {
    /// A plain item, its icon picked from the label (`menu_icon`).
    pub(crate) fn item(
        label: impl Into<SharedString>,
        run: impl Fn(&mut Kyde, &mut Window, &mut Context<Kyde>) + 'static,
    ) -> Self {
        Self::Item {
            label: label.into(),
            file: None,
            dim: false,
            run: Rc::new(run),
        }
    }

    /// Rows the highlight can land on (items + submenu headers; not notes or dividers).
    fn selectable(&self) -> bool {
        matches!(self, Self::Item { .. } | Self::Submenu { .. })
    }
}

/// The next selectable row from `from`, stepping `down` or up and wrapping round. From no
/// highlight, down lands on the first selectable row and up on the last.
fn step_highlight(entries: &[MenuEntry], from: Option<usize>, down: bool) -> Option<usize> {
    let n = entries.len();
    if n == 0 {
        return None;
    }
    let mut i = from.unwrap_or(if down { n - 1 } else { 0 });
    for _ in 0..n {
        i = if down { (i + 1) % n } else { (i + n - 1) % n };
        if entries[i].selectable() {
            return Some(i);
        }
    }
    None
}

impl MenuTarget {
    /// The modal window a menu on this target renders in (`None` = the main window).
    pub(crate) fn host(&self) -> Option<ModalKind> {
        match self {
            MenuTarget::RollbackFile(_) => Some(ModalKind::Rollback),
            MenuTarget::PushFile(_) => Some(ModalKind::Push),
            MenuTarget::LhRow | MenuTarget::LhPath(..) => Some(ModalKind::LocalHistory),
            _ => None,
        }
    }
}

impl Kyde {
    // ── opening / closing ─────────────────────────────────────────

    /// Open a context menu at `at` — its entries are built here, once, so availability
    /// checks (has changes? ahead of upstream?) never run per frame.
    pub(crate) fn open_menu(
        &mut self,
        at: Point<Pixels>,
        target: MenuTarget,
        cx: &mut Context<Self>,
    ) {
        let entries = self.menu_entries(&target);
        self.context_menu = Some(ContextMenu {
            at,
            target,
            submenu: None,
            entries,
            hl: None,
            sub_hl: None,
        });
        // Take keyboard focus on the next frame of the hosting window (see
        // `focus_menu_if_pending`) — the panel isn't in the element tree yet.
        self.menu_focus_pending = true;
        cx.notify();
    }

    /// Open a menu from the keyboard: the first item starts highlighted, so ⏎ acts at once.
    pub(crate) fn open_menu_by_key(
        &mut self,
        at: Point<Pixels>,
        target: MenuTarget,
        cx: &mut Context<Self>,
    ) {
        self.open_menu(at, target, cx);
        if let Some(m) = self.context_menu.as_mut() {
            m.hl = step_highlight(&m.entries, None, true);
        }
    }

    /// Hand keyboard focus to a just-opened menu, from the render pass of the window it lives
    /// in (`host` = that window's modal kind, `None` for the main window). Remembers what had
    /// focus so closing the menu gives it back.
    pub(crate) fn focus_menu_if_pending(
        &mut self,
        host: Option<ModalKind>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let here = self
            .context_menu
            .as_ref()
            .is_some_and(|m| m.target.host() == host);
        if self.menu_focus_pending && here {
            self.menu_focus_pending = false;
            self.menu_return_focus = window.focused(cx).filter(|f| *f != self.menu_focus);
            window.focus(&self.menu_focus);
        }
    }

    pub(crate) fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.context_menu = None;
        cx.notify();
    }

    /// Close the menu and, if it still holds focus (the picked action didn't move it, e.g.
    /// into a rename box), hand focus back to wherever it came from.
    fn close_menu_restoring(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.context_menu = None;
        if self.menu_focus.is_focused(window) {
            if let Some(f) = self.menu_return_focus.take() {
                window.focus(&f);
            }
        }
        cx.notify();
    }

    /// Run a picked item, then close — the order a click takes (the row's handler, then the
    /// backdrop's dismiss).
    fn menu_run(&mut self, run: &MenuRun, window: &mut Window, cx: &mut Context<Self>) {
        run(self, window, cx);
        self.close_menu_restoring(window, cx);
    }

    /// Open a flyout submenu (`New` / `Git`) — used on hover, so it switches between them
    /// but never re-closes on a repeated hover.
    pub(crate) fn open_menu_submenu(&mut self, kind: Submenu, cx: &mut Context<Self>) {
        if let Some(m) = self.context_menu.as_mut() {
            if m.submenu != Some(kind) {
                m.submenu = Some(kind);
                m.sub_hl = None;
                cx.notify();
            }
        }
    }

    /// Toggle a flyout submenu (`New` / `Git`) — used on click.
    pub(crate) fn toggle_menu_submenu(&mut self, kind: Submenu, cx: &mut Context<Self>) {
        if let Some(m) = self.context_menu.as_mut() {
            m.submenu = if m.submenu == Some(kind) {
                None
            } else {
                Some(kind)
            };
            m.sub_hl = None;
        }
        cx.notify();
    }

    // ── keyboard ──────────────────────────────────────────────────

    pub(crate) fn menu_down(&mut self, _: &MenuDown, _w: &mut Window, cx: &mut Context<Self>) {
        self.menu_step(true, cx);
    }

    pub(crate) fn menu_up(&mut self, _: &MenuUp, _w: &mut Window, cx: &mut Context<Self>) {
        self.menu_step(false, cx);
    }

    /// ↑/↓ — within the open submenu once the keyboard is in it, else the top level (moving
    /// off a submenu header folds its flyout).
    fn menu_step(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(m) = self.context_menu.as_mut() else {
            return;
        };
        let ContextMenu {
            entries,
            hl,
            submenu,
            sub_hl,
            ..
        } = m;
        if sub_hl.is_some() {
            if let Some(items) = submenu_items(entries, *submenu) {
                *sub_hl = step_highlight(items, *sub_hl, down);
            }
        } else {
            let next = step_highlight(entries, *hl, down);
            if next != *hl {
                *submenu = None;
            }
            *hl = next;
        }
        cx.notify();
    }

    /// → — open the highlighted submenu and step into it.
    pub(crate) fn menu_right(&mut self, _: &MenuRight, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.context_menu.as_mut() else {
            return;
        };
        if m.sub_hl.is_some() {
            return;
        }
        if let Some(MenuEntry::Submenu { kind, items, .. }) = m.hl.and_then(|i| m.entries.get(i)) {
            m.submenu = Some(*kind);
            m.sub_hl = step_highlight(items, None, true);
            cx.notify();
        }
    }

    /// ← — back out of a submenu to its header.
    pub(crate) fn menu_left(&mut self, _: &MenuLeft, _w: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = self.context_menu.as_mut() {
            if m.sub_hl.take().is_some() {
                m.submenu = None;
                cx.notify();
            }
        }
    }

    /// ⏎ / space — run the highlighted item, or step into the highlighted submenu.
    pub(crate) fn menu_confirm(
        &mut self,
        _: &MenuConfirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(m) = self.context_menu.as_ref() else {
            return;
        };
        let picked = if m.sub_hl.is_some() {
            submenu_items(&m.entries, m.submenu).and_then(|items| items.get(m.sub_hl?))
        } else {
            m.hl.and_then(|i| m.entries.get(i))
        };
        match picked {
            Some(MenuEntry::Item { run, .. }) => {
                let run = run.clone();
                self.menu_run(&run, window, cx);
            }
            Some(MenuEntry::Submenu { .. }) => self.menu_right(&MenuRight, window, cx),
            _ => {}
        }
    }

    /// ⎋ — out of the submenu first, then close the menu.
    pub(crate) fn menu_dismiss(
        &mut self,
        _: &MenuDismiss,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .context_menu
            .as_ref()
            .is_some_and(|m| m.sub_hl.is_some())
        {
            self.menu_left(&MenuLeft, window, cx);
        } else {
            self.close_menu_restoring(window, cx);
        }
    }

    /// Mouse hover lights a row (and folds a flyout that isn't the hovered row's own).
    fn menu_hover(&mut self, row: usize, sub: Option<usize>, cx: &mut Context<Self>) {
        let Some(m) = self.context_menu.as_mut() else {
            return;
        };
        match sub {
            Some(j) => {
                m.hl = Some(row);
                m.sub_hl = Some(j);
            }
            None if m.hl != Some(row) || m.sub_hl.is_some() => {
                m.hl = Some(row);
                m.sub_hl = None;
                if !matches!(m.entries.get(row), Some(MenuEntry::Submenu { kind, .. }) if m.submenu == Some(*kind))
                {
                    m.submenu = None;
                }
            }
            None => return,
        }
        cx.notify();
    }

    // ── entries ───────────────────────────────────────────────────

    /// The rows for a menu on `target`, with every availability check done now.
    fn menu_entries(&self, target: &MenuTarget) -> Vec<MenuEntry> {
        let fetch = || MenuEntry::item("Fetch", |this, _w, cx| this.do_fetch(cx));
        let pull = || MenuEntry::item("Pull", |this, _w, cx| this.do_pull(cx));
        let push = || MenuEntry::item("Push", |this, _w, cx| this.open_push_modal(cx));
        let ahead = self.sync.ahead.unwrap_or(0) > 0;
        let mut out = Vec::new();
        match target {
            MenuTarget::EditorGit(p, can_sort_lines, can_sort_keys) => {
                // Buffer sort ops first (they act on the click point / selection), then the
                // git commands.
                if *can_sort_lines {
                    out.push(MenuEntry::item("Sort Lines", |this, _w, cx| {
                        this.sort_selected_lines(cx);
                    }));
                }
                if *can_sort_keys {
                    out.push(MenuEntry::item("Sort Object Keys", |this, _w, cx| {
                        this.sort_object_keys_at_caret(cx);
                    }));
                }
                // Git commands only when the project is a git repo (issue #66).
                if self.is_git {
                    let pc = p.clone();
                    out.push(MenuEntry::item("Commit", move |this, _w, cx| {
                        this.menu_commit_path(pc.clone(), cx);
                    }));
                    if self.has_changes_under(p) {
                        let pr = p.clone();
                        out.push(MenuEntry::item("Rollback", move |this, _w, cx| {
                            this.open_rollback_path(pr.clone(), cx);
                        }));
                    }
                }
                // Local History for the edited file (issue #7) — hidden when disabled.
                if self.lh.store.is_some() {
                    let pl = p.clone();
                    out.push(MenuEntry::item("Local History", move |this, _w, cx| {
                        this.open_local_history(pl.clone(), cx);
                    }));
                }
                // Git remote ops, WebStorm-style (Fetch/Pull always, Push when ahead).
                if self.is_git {
                    out.extend([fetch(), pull()]);
                    if ahead {
                        out.push(push());
                    }
                }
            }
            MenuTarget::BrowseFile(p, is_dir) => {
                let is_dir = *is_dir;
                // New File / Paste land inside this folder, or in a file's parent folder.
                let dir = if is_dir {
                    p.clone()
                } else {
                    p.parent()
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or_default()
                };
                // New ▸ — File / Scratch File / Directory. A scratch file's language is
                // picked in the finder overlay; it lives outside the tree, so it isn't
                // rooted at the clicked folder.
                let (nd_file, nd_dir) = (dir.clone(), dir.clone());
                out.push(MenuEntry::Submenu {
                    kind: Submenu::New,
                    label: "New".into(),
                    icon_label: "New File…".into(),
                    items: vec![
                        MenuEntry::item("File", move |this, window, cx| {
                            this.start_new_file(nd_file.clone(), window, cx);
                        }),
                        MenuEntry::item("Scratch File", |this, window, cx| {
                            this.open_finder(FinderMode::Scratch, window, cx);
                        }),
                        MenuEntry::item("Directory", move |this, window, cx| {
                            this.start_new_folder(nd_dir.clone(), window, cx);
                        }),
                    ],
                });
                out.push(MenuEntry::Divider);

                // Cut / Copy / Paste (issue #67). Copy/Cut act on the whole cmd-selection
                // when the clicked row is part of it, else just this row. Paste pulls from
                // kyde's clipboard, then Finder file copies, then image data.
                fn clip_paths(this: &Kyde, row: &std::path::Path) -> Vec<PathBuf> {
                    if this.browse.multi_selected.len() > 1
                        && this.browse.multi_selected.iter().any(|s| s == row)
                    {
                        this.browse.multi_selected.clone()
                    } else {
                        vec![row.to_path_buf()]
                    }
                }
                let (pcut, pcopy) = (p.clone(), p.clone());
                out.push(MenuEntry::item("Cut", move |this, _w, cx| {
                    let paths = clip_paths(this, &pcut);
                    this.clip_paths(paths, true, cx);
                }));
                out.push(MenuEntry::item("Copy", move |this, _w, cx| {
                    let paths = clip_paths(this, &pcopy);
                    this.clip_paths(paths, false, cx);
                }));
                out.push(MenuEntry::item("Paste", move |this, _w, cx| {
                    this.paste_into(dir.clone(), cx);
                }));
                // Exactly two cmd-selected files → side-by-side compare (issue #42).
                if let [a, b] = self.browse.multi_selected.as_slice() {
                    let (a, b) = (a.clone(), b.clone());
                    out.push(MenuEntry::item("Compare Selected", move |this, _w, cx| {
                        this.open_compare(a.clone(), b.clone(), cx);
                    }));
                }
                out.push(MenuEntry::Divider);

                // Rename / Delete — a folder rename carries its whole subtree.
                let (pn, pd) = (p.clone(), p.clone());
                out.push(MenuEntry::item("Rename…", move |this, window, cx| {
                    this.start_rename(pn.clone(), window, cx);
                }));
                out.push(MenuEntry::item("Delete…", move |this, _w, cx| {
                    this.open_delete(pd.clone(), cx);
                }));

                // Local History + Git group (each conditional) — its own divided section.
                if self.lh.store.is_some() || self.is_git {
                    out.push(MenuEntry::Divider);
                }
                // Local History (issue #7) — file OR folder scope; hidden when disabled.
                if self.lh.store.is_some() {
                    let pl = p.clone();
                    out.push(MenuEntry::item("Local History", move |this, _w, cx| {
                        this.open_local_history(pl.clone(), cx);
                    }));
                }
                // Git ▸ — commit / rollback / history / fetch / pull / push, grouped. The
                // whole submenu is hidden for a plain (non-git) folder (issue #66).
                if self.is_git {
                    let mut git = Vec::new();
                    if self.has_changes_under(p) {
                        let (pc, pr) = (p.clone(), p.clone());
                        git.push(MenuEntry::item("Commit", move |this, _w, cx| {
                            this.menu_commit_path(pc.clone(), cx);
                        }));
                        git.push(MenuEntry::item("Rollback", move |this, _w, cx| {
                            this.open_rollback_path(pr.clone(), cx);
                        }));
                    }
                    let ph = p.clone();
                    git.push(MenuEntry::item("Git History", move |this, _w, cx| {
                        this.enter_history_for(ph.clone(), cx);
                    }));
                    git.extend([fetch(), pull()]);
                    if ahead {
                        git.push(push());
                    }
                    out.push(MenuEntry::Submenu {
                        kind: Submenu::Git,
                        label: "Git".into(),
                        icon_label: "Commit".into(),
                        items: git,
                    });
                }

                // Reveal in Finder (the file manager) — its own trailing group.
                let pv = p.clone();
                out.push(MenuEntry::Divider);
                out.push(MenuEntry::item(
                    super::REVEAL_IN_FILES,
                    move |this, _w, cx| {
                        this.reveal_in_os(&pv, cx);
                    },
                ));
            }
            MenuTarget::Tab(idx) => {
                let idx = *idx;
                out.push(MenuEntry::item("Close", move |this, _w, cx| {
                    this.close_tab(idx, cx);
                }));
                out.push(MenuEntry::item("Close Others", move |this, _w, cx| {
                    this.close_other_tabs(idx, cx);
                }));
                out.push(MenuEntry::item(
                    "Close Tabs to the Right",
                    move |this, _w, cx| {
                        this.close_tabs_right(idx, cx);
                    },
                ));
                let tab = self.browse.open_tabs.get(idx).cloned();
                // Compare this tab's file against the ACTIVE tab's (issue #42) — only offered
                // on a non-active tab, where the pair is meaningful. Active tab on the left
                // (the "current" baseline), the clicked tab on the right.
                if let (Some(tab), Some(active)) = (tab.clone(), self.browse.open_path.clone()) {
                    if tab != active {
                        out.push(MenuEntry::item(
                            "Compare with Current Tab",
                            move |this, _w, cx| {
                                this.open_compare(active.clone(), tab.clone(), cx);
                            },
                        ));
                    }
                }
                if let Some(tab) = tab {
                    if self.lh.store.is_some() {
                        let tl = tab.clone();
                        out.push(MenuEntry::item("Local History", move |this, _w, cx| {
                            this.open_local_history(tl.clone(), cx);
                        }));
                    }
                    out.push(MenuEntry::item(
                        super::REVEAL_IN_FILES,
                        move |this, _w, cx| {
                            this.reveal_in_os(&tab, cx);
                        },
                    ));
                }
            }
            MenuTarget::CommitPath(path, _is_dir) => {
                // Folders + files both get Rollback. (Staging is implicit — checking a file in
                // the tree includes it in the commit; no separate Stage item.)
                let pr = path.clone();
                out.push(MenuEntry::item("Rollback", move |this, _w, cx| {
                    this.open_rollback_path(pr.clone(), cx);
                }));
                if ahead {
                    out.push(push());
                }
            }
            MenuTarget::RollbackFile(idx) => {
                let idx = *idx;
                out.push(MenuEntry::item("View Diff", move |this, _w, cx| {
                    this.menu_show_diff(idx, cx);
                }));
            }
            MenuTarget::PushFile(idx) => {
                let idx = *idx;
                out.push(MenuEntry::item("View Diff", move |this, _w, cx| {
                    this.push_show_diff(idx, cx);
                }));
            }
            // Local History menus (timeline row / changed-files row) — the feature module
            // owns its items.
            t @ (MenuTarget::LhRow | MenuTarget::LhPath(..)) => out = self.lh_menu_entries(t),
            MenuTarget::TabList => {
                if self.browse.open_tabs.is_empty() {
                    out.push(MenuEntry::Note("No open tabs".into()));
                }
                for (i, p) in self.browse.open_tabs.iter().enumerate() {
                    let name: SharedString = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into();
                    out.push(MenuEntry::Item {
                        label: name,
                        file: Some(p.clone()),
                        dim: self.browse.open_path.as_ref() != Some(p),
                        run: Rc::new(move |this, _w, cx| {
                            if let Some(p) = this.browse.open_tabs.get(i).cloned() {
                                this.open_file(p, cx);
                            }
                        }),
                    });
                }
            }
            // Same compare options as the header dropdown, applied to the right-clicked
            // commit (`HistoryCompare`) or file row (`HistoryFile` — the file stays selected).
            MenuTarget::HistoryCompare(idx) | MenuTarget::HistoryFile(idx) => {
                let (idx, on_file) = (*idx, matches!(target, MenuTarget::HistoryFile(_)));
                for mode in CompareMode::ALL {
                    let label = if mode == self.history.compare {
                        format!("✓ {}", mode.label())
                    } else {
                        mode.label().to_string()
                    };
                    out.push(MenuEntry::item(label, move |this, _w, cx| {
                        if on_file {
                            this.history_compare_file(idx, mode, cx);
                        } else {
                            this.history_compare_commit(idx, mode, cx);
                        }
                    }));
                }
            }
            // The menu bar's items, from the title strip's menu button (off macOS).
            MenuTarget::AppMenu => out = chrome::app_menu_entries(crate::app_menus(&self.recents)),
            // A branch row in the branch popup: merge it into the current branch, or check it
            // out. Checked out in another worktree → Checkout jumps there (say so); hidden
            // inside a linked worktree with no jump target (the worktree is pinned to its
            // branch — merge is still allowed).
            MenuTarget::Branch(name) => {
                let cur = self.current_branch.clone().unwrap_or_else(|| "HEAD".into());
                let elsewhere = self
                    .other_worktree_for_branch(name)
                    .and_then(|w| w.path.file_name())
                    .map(|s| s.to_string_lossy().into_owned());
                if elsewhere.is_some() || !self.in_linked_worktree() {
                    let label = match elsewhere {
                        Some(wt) => format!("Checkout  (jumps to “{wt}”)"),
                        None => "Checkout".to_string(),
                    };
                    let nm = name.clone();
                    out.push(MenuEntry::item(label, move |this, window, cx| {
                        this.context_menu = None;
                        this.checkout_branch(nm.clone(), window, cx);
                    }));
                }
                let nm = name.clone();
                out.push(MenuEntry::item(
                    format!("Merge “{name}” into “{cur}”"),
                    move |this, _w, cx| this.menu_merge_branch(nm.clone(), cx),
                ));
            }
        }
        out
    }

    // ── rendering ─────────────────────────────────────────────────

    /// Right-click menu, positioned at the cursor. Transparent backdrop closes it.
    pub(crate) fn render_context_menu(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(menu) = &self.context_menu else {
            return div().into_any_element();
        };
        let t = theme::get();
        let panel_chrome = |d: gpui::Div| {
            d.flex()
                .flex_col()
                .py_1()
                .bg(t.bg_mid)
                .border_1()
                .border_color(t.divider)
                .rounded_md()
                .shadow_lg()
        };
        let rows: Vec<gpui::AnyElement> = menu
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let lit = menu.hl == Some(i);
                match entry {
                    MenuEntry::Submenu {
                        kind,
                        label,
                        icon_label,
                        items,
                    } => {
                        let (kind, open) = (*kind, menu.submenu == Some(*kind));
                        let header = menu_row(
                            SharedString::from(format!("menu-sub-{label}")),
                            lit || open,
                            icon_slot(menu_icon(icon_label)),
                            label.clone(),
                            t.text,
                        )
                        .child(
                            div().flex_none().flex().items_center().child(
                                svg()
                                    .path("icons/chevron-right.svg")
                                    .size(px(14.0))
                                    .text_color(t.line_number),
                            ),
                        )
                        // Hover opens it (stays until you pick an item / dismiss); click
                        // toggles.
                        .on_hover(cx.listener(move |this, hovered: &bool, _w, cx| {
                            if *hovered {
                                this.menu_hover(i, None, cx);
                                this.open_menu_submenu(kind, cx);
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                cx.stop_propagation();
                                this.toggle_menu_submenu(kind, cx);
                            }),
                        );
                        let mut wrap = div().relative().child(header);
                        if open {
                            let flyout = items.iter().enumerate().map(|(j, e)| {
                                let lit = menu.sub_hl == Some(j);
                                self.menu_entry(e, (i, Some(j)), lit, cx)
                            });
                            wrap = wrap.child(
                                panel_chrome(div())
                                    .absolute()
                                    .left_full()
                                    .top_0()
                                    .ml(px(4.0))
                                    .min_w(px(150.0))
                                    .children(flyout),
                            );
                        }
                        wrap.into_any_element()
                    }
                    e => self.menu_entry(e, (i, None), lit, cx),
                }
            })
            .collect();

        let panel = panel_chrome(div())
            .track_focus(&self.menu_focus)
            .key_context("ContextMenu")
            .on_action(cx.listener(Self::menu_up))
            .on_action(cx.listener(Self::menu_down))
            .on_action(cx.listener(Self::menu_left))
            .on_action(cx.listener(Self::menu_right))
            .on_action(cx.listener(Self::menu_confirm))
            .on_action(cx.listener(Self::menu_dismiss))
            .cursor(gpui::CursorStyle::Arrow)
            .min_w(px(160.0))
            .font_family(theme::font::UI_FAMILY)
            .text_size(px(t.ui_font_size))
            .children(rows);

        // Backdrop: any click (incl. right elsewhere) dismisses the menu. `occlude()` makes it
        // absorb ALL mouse events (incl. hover/move), so hovering the menu — or anywhere over
        // the backdrop — doesn't bleed through to the rows painted behind it.
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, w, cx| this.close_menu_restoring(w, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _e, w, cx| this.close_menu_restoring(w, cx)),
            )
            // `anchored` positions the panel at the cursor but flips/snaps it back inside the
            // window when it would overflow an edge (so a menu near the bottom/right opens
            // upward/leftward instead of being clipped). `deferred` paints it above the rest.
            .child({
                // Branch actions fly out LEFT of the branch popup: `at` is the popup's left
                // edge, so anchor the menu's top-RIGHT corner there (every other menu hangs
                // from its top-left at the cursor).
                let mut anchored = gpui::anchored().position(menu.at);
                if matches!(menu.target, MenuTarget::Branch(_)) {
                    anchored = anchored.anchor(gpui::Corner::TopRight);
                }
                gpui::deferred(anchored.snap_to_window_with_margin(px(8.0)).child(panel))
                    .with_priority(1)
            })
            .into_any_element()
    }

    /// One non-submenu row. `at` = (top-level index, index within its flyout).
    fn menu_entry(
        &self,
        entry: &MenuEntry,
        at: (usize, Option<usize>),
        lit: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        match entry {
            MenuEntry::Item {
                label,
                file,
                dim,
                run,
            } => {
                let icon = match file {
                    Some(p) => div()
                        .flex_none()
                        .child(badge_inner(file_badge(p), 0.0))
                        .into_any_element(),
                    None => icon_slot(menu_icon(label)).into_any_element(),
                };
                let id = match at {
                    (i, None) => SharedString::from(format!("menu-row-{i}")),
                    (i, Some(j)) => SharedString::from(format!("menu-row-{i}-{j}")),
                };
                let run = run.clone();
                menu_row(
                    id,
                    lit,
                    icon,
                    label.clone(),
                    if *dim { t.line_number } else { t.text },
                )
                .on_hover(cx.listener(move |this, hovered: &bool, _w, cx| {
                    if *hovered {
                        this.menu_hover(at.0, at.1, cx);
                    }
                }))
                // The click bubbles on to the backdrop, which closes the menu after.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, window, cx| run(this, window, cx)),
                )
                .into_any_element()
            }
            MenuEntry::Note(label) => div()
                .px_3()
                .py_1()
                .text_color(t.line_number)
                .child(label.clone())
                .into_any_element(),
            MenuEntry::Divider => ui::menu_divider().into_any_element(),
            // Nested submenus don't occur; a header renders as its label.
            MenuEntry::Submenu { label, .. } => {
                div().px_3().py_1().child(label.clone()).into_any_element()
            }
        }
    }
}

/// The entries of the `kind` submenu, if the menu has one.
fn submenu_items(entries: &[MenuEntry], kind: Option<Submenu>) -> Option<&[MenuEntry]> {
    let kind = kind?;
    entries.iter().find_map(|e| match e {
        MenuEntry::Submenu { kind: k, items, .. } if *k == kind => Some(items.as_slice()),
        _ => None,
    })
}

/// The fixed-width leading icon column (empty when the row has no icon, so labels align).
fn icon_slot(icon: Option<&'static str>) -> gpui::Div {
    let slot = div().flex_none().size(px(16.0)).flex().items_center();
    match icon {
        Some(path) => slot.child(
            svg()
                .path(path)
                .size(px(15.0))
                .text_color(theme::get().secondary_text),
        ),
        None => slot,
    }
}

/// A menu row's shell: icon + label, lit when highlighted (by keyboard or hover).
fn menu_row(
    id: SharedString,
    lit: bool,
    icon: impl IntoElement,
    label: SharedString,
    color: kyde_color::Color,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_3()
        .py_1()
        // Default arrow cursor over menu rows (don't inherit the editor's I-beam).
        .cursor(gpui::CursorStyle::Arrow)
        .text_color(color)
        .when(lit, |d| d.bg(t.selected_bg))
        .child(icon)
        .child(div().flex_1().min_w_0().truncate().child(label))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ↑/↓ skip notes + dividers and wrap at both ends.
    #[test]
    fn highlight_skips_unpickable_rows_and_wraps() {
        let noop = || MenuEntry::item("x", |_, _, _| {});
        let entries = vec![
            noop(),
            MenuEntry::Divider,
            MenuEntry::Note("n".into()),
            noop(),
        ];
        assert_eq!(step_highlight(&entries, None, true), Some(0));
        assert_eq!(step_highlight(&entries, None, false), Some(3));
        assert_eq!(step_highlight(&entries, Some(0), true), Some(3));
        assert_eq!(step_highlight(&entries, Some(3), true), Some(0));
        assert_eq!(step_highlight(&entries, Some(0), false), Some(3));
        assert_eq!(step_highlight(&[MenuEntry::Divider], None, true), None);
    }
}
