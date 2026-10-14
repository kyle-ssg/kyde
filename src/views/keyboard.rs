//! Keyboard navigation for the app's lists, trees and popups (issue #81). Each list's
//! elements carry the `ui::nav` key contexts and register [`nav_keys`] handlers for their
//! [`NavTarget`]; the focus path decides which list a key reaches. Semantics everywhere:
//! ↑/↓ move, ←/→ fold or hop panes, ⏎ activates, space toggles / previews, ⇧F10 opens the
//! row's context menu. Crate-root child module.

use crate::*;
use kyde_tree::{nav, Nav, NavKey};
use ui::nav::{
    ListConfirm, ListDismiss, ListDown, ListLeft, ListMenu, ListRight, ListToggle, ListUp,
};

/// Which list a navigation key is for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavTarget {
    /// The Browse file tree.
    BrowseTree,
    /// The Commit tab's changed-files tree (and its search box).
    CommitTree,
    /// The Push tab's file list.
    PushList,
    /// The History commit list (and its search box).
    HistoryCommits,
    /// The History changed-files tree (and its search box).
    HistoryFiles,
    /// The History branch dropdown (its search box keeps focus).
    HistoryRevs,
    /// The History compare-mode dropdown.
    HistoryCompareModes,
    /// The branch popup (its search box keeps focus).
    BranchPopup,
    /// The worktree popup.
    WorktreePopup,
    /// The Projects landing list (its search box keeps focus).
    Projects,
}

/// One navigation key.
#[derive(Clone, Copy)]
enum Key {
    Arrow(NavKey),
    Confirm,
    Toggle,
    Menu,
    Dismiss,
}

/// Register `target`'s handlers for every navigation action on `el`.
pub(crate) fn nav_keys<E: InteractiveElement>(
    el: E,
    target: NavTarget,
    cx: &mut Context<Kyde>,
) -> E {
    let on = move |key: Key| {
        move |this: &mut Kyde, window: &mut Window, cx: &mut Context<Kyde>| {
            this.list_key(target, key, window, cx);
        }
    };
    let (up, down, left, right) = (
        on(Key::Arrow(NavKey::Up)),
        on(Key::Arrow(NavKey::Down)),
        on(Key::Arrow(NavKey::Left)),
        on(Key::Arrow(NavKey::Right)),
    );
    let (confirm, toggle, menu, dismiss) = (
        on(Key::Confirm),
        on(Key::Toggle),
        on(Key::Menu),
        on(Key::Dismiss),
    );
    el.on_action(cx.listener(move |this, _: &ListUp, w, cx| up(this, w, cx)))
        .on_action(cx.listener(move |this, _: &ListDown, w, cx| down(this, w, cx)))
        .on_action(cx.listener(move |this, _: &ListLeft, w, cx| left(this, w, cx)))
        .on_action(cx.listener(move |this, _: &ListRight, w, cx| right(this, w, cx)))
        .on_action(cx.listener(move |this, _: &ListConfirm, w, cx| confirm(this, w, cx)))
        .on_action(cx.listener(move |this, _: &ListToggle, w, cx| toggle(this, w, cx)))
        .on_action(cx.listener(move |this, _: &ListMenu, w, cx| menu(this, w, cx)))
        .on_action(cx.listener(move |this, _: &ListDismiss, w, cx| dismiss(this, w, cx)))
}

/// Where a keyboard-opened context menu hangs: under row `i` of the list `scroll` tracks,
/// else a spot near the window's top-left.
fn row_anchor(scroll: &ScrollHandle, i: usize) -> Point<Pixels> {
    scroll.bounds_for_item(i).map_or_else(
        || gpui::point(px(160.0), px(120.0)),
        |b| gpui::point(b.left() + px(24.0), b.bottom()),
    )
}

/// The row `cursor` sits on, if it's visible.
fn row_of<'a>(
    rows: &'a [tree::Row],
    cursor: Option<&std::path::Path>,
) -> Option<(usize, &'a tree::Row)> {
    let c = cursor?;
    rows.iter().enumerate().find(|(_, r)| r.path == c)
}

impl Kyde {
    fn list_key(
        &mut self,
        target: NavTarget,
        key: Key,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // An open delete confirmation owns ⏎ even while a tree has focus.
        if self.delete_target.is_some() && matches!(key, Key::Confirm) {
            self.do_delete(cx);
            return;
        }
        match target {
            NavTarget::BrowseTree => self.browse_key(key, window, cx),
            NavTarget::CommitTree => self.commit_tree_key(key, window, cx),
            NavTarget::PushList => self.push_list_key(key, cx),
            NavTarget::HistoryCommits => self.history_commits_key(key, window, cx),
            NavTarget::HistoryFiles => self.history_files_key(key, window, cx),
            NavTarget::HistoryRevs => self.history_revs_key(key, cx),
            NavTarget::HistoryCompareModes => self.history_compare_key(key, cx),
            NavTarget::BranchPopup => self.branch_popup_key(key, window, cx),
            NavTarget::WorktreePopup => self.worktree_popup_key(key, window, cx),
            NavTarget::Projects => self.projects_key(key, cx),
        }
        cx.notify();
    }

    /// Browse tree: arrows walk + fold; ⏎ opens a file (into the editor) or folds a folder;
    /// space previews a file without leaving the tree.
    fn browse_key(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.browse_visible_rows();
        let cursor = self.browse.selected_path.clone();
        let at = row_of(&rows, cursor.as_deref()).map(|(i, r)| (i, r.path.clone(), r.is_dir));
        match key {
            Key::Arrow(k) => match nav(&rows, cursor.as_deref(), &self.browse.expanded, k) {
                Nav::Select(p) => {
                    self.browse.multi_selected.clear();
                    if let Some(i) = rows.iter().position(|r| r.path == p) {
                        self.browse.tree_scroll.scroll_to_item(i);
                    }
                    self.browse.selected_path = Some(p);
                }
                Nav::Expand(p) | Nav::Collapse(p) => self.toggle_dir(p, cx),
                Nav::Stay => {}
            },
            Key::Confirm | Key::Toggle => match at {
                Some((_, p, true)) => self.toggle_dir(p, cx),
                Some((_, p, false)) if matches!(key, Key::Confirm) => {
                    self.open_file(p, cx);
                    self.focus_editor(window, cx);
                }
                Some((_, p, false)) => self.preview_file(p, cx),
                None => {}
            },
            Key::Menu => {
                if let Some((i, p, is_dir)) = at {
                    let pos = row_anchor(&self.browse.tree_scroll, i);
                    self.open_menu_by_key(pos, MenuTarget::BrowseFile(p, is_dir), cx);
                }
            }
            Key::Dismiss => {}
        }
    }

    /// Commit tree: arrows walk + fold (landing on a file shows its diff); space ticks the
    /// row in/out of the commit; ⏎ folds a folder or jumps into the file's working pane.
    /// From the search box, ⏎ moves into the tree.
    fn commit_tree_key(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.commit_visible_rows(cx);
        let cursor = self.commit.dir_cursor.clone().or_else(|| {
            self.selected
                .and_then(|i| self.files.get(i))
                .map(|f| f.path.clone())
        });
        let at = row_of(&rows, cursor.as_deref()).map(|(i, r)| (i, r.path.clone(), r.is_dir));
        let in_tree = self.commit.tree_focus.is_focused(window);
        match key {
            Key::Arrow(k) => match nav(&rows, cursor.as_deref(), &self.commit.expanded, k) {
                Nav::Select(p) => {
                    if let Some(i) = rows.iter().position(|r| r.path == p) {
                        self.commit.tree_scroll.scroll_to_item(i);
                    }
                    self.commit_cursor_to(p, cx);
                }
                Nav::Expand(p) | Nav::Collapse(p) => self.toggle_commit_dir(p, cx),
                Nav::Stay => {}
            },
            Key::Confirm if !in_tree => {
                if at.is_none() {
                    if let Some(first) = rows.iter().find(|r| !r.is_dir) {
                        self.commit_cursor_to(first.path.clone(), cx);
                    }
                }
                window.focus(&self.commit.tree_focus);
            }
            Key::Confirm => match at {
                Some((_, p, true)) => self.toggle_commit_dir(p, cx),
                Some((_, _, false)) => {
                    let pane = self.diff.right.read(cx).focus_handle.clone();
                    window.focus(&pane);
                }
                None => {}
            },
            Key::Toggle => {
                if let Some((_, p, is_dir)) = at {
                    self.toggle_commit_check(p, is_dir, cx);
                }
            }
            Key::Menu => {
                if let Some((i, p, is_dir)) = at {
                    let pos = row_anchor(&self.commit.tree_scroll, i);
                    self.open_menu_by_key(pos, MenuTarget::CommitPath(p, is_dir), cx);
                }
            }
            Key::Dismiss => {}
        }
    }

    /// Put the Commit tree's cursor on `p`: a folder just moves the cursor; a file selects
    /// it (loading its diff).
    pub(crate) fn commit_cursor_to(&mut self, p: PathBuf, cx: &mut Context<Self>) {
        match self.files.iter().position(|f| f.path == p) {
            Some(i) => self.select_with(i, Some(cx)),
            None => self.commit.dir_cursor = Some(p),
        }
    }

    /// Push tab tree: arrows walk + fold (landing on a file shows its diff); ⏎ / space fold
    /// a folder.
    fn push_list_key(&mut self, key: Key, cx: &mut Context<Self>) {
        let rows = self.push_visible_rows();
        let cursor = self.sync.push_dir_cursor.clone().or_else(|| {
            self.sync
                .push_selected
                .and_then(|i| self.sync.push_files.get(i))
                .map(|f| f.path.clone())
        });
        match key {
            Key::Arrow(k) => match nav(&rows, cursor.as_deref(), &self.sync.push_expanded, k) {
                Nav::Select(p) => {
                    if let Some(i) = rows.iter().position(|r| r.path == p) {
                        self.sync.push_scroll.scroll_to_item(i);
                    }
                    match self.sync.push_files.iter().position(|f| f.path == p) {
                        Some(i) => self.select_push_file(i, cx),
                        None => self.sync.push_dir_cursor = Some(p),
                    }
                }
                Nav::Expand(p) | Nav::Collapse(p) => self.toggle_push_dir(p, cx),
                Nav::Stay => {}
            },
            Key::Confirm | Key::Toggle => {
                if let Some((_, r)) = row_of(&rows, cursor.as_deref()).filter(|(_, r)| r.is_dir) {
                    let p = r.path.clone();
                    self.toggle_push_dir(p, cx);
                }
            }
            _ => {}
        }
    }

    /// History commit list: ↑/↓ walk the (filtered) commits; → / ⏎ step into the files
    /// pane. From the search box, ⏎ moves into the list.
    fn history_commits_key(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.history_visible_commits(cx);
        let pos = self
            .history
            .selected
            .and_then(|s| shown.iter().position(|&i| i == s));
        let in_list = self.history.list_focus.is_focused(window);
        let next = match (key, pos) {
            (Key::Arrow(NavKey::Down), None) => Some(0),
            (Key::Arrow(NavKey::Down), Some(p)) => Some(ui::picker::nav_down(p, shown.len())),
            (Key::Arrow(NavKey::Up), None) => shown.len().checked_sub(1),
            (Key::Arrow(NavKey::Up), Some(p)) => Some(ui::picker::nav_up(p)),
            (Key::Confirm, None) if !in_list => Some(0),
            _ => None,
        };
        if let Some(&i) = next.and_then(|p| shown.get(p)) {
            if self.history.selected != Some(i) {
                self.select_history_commit(i, cx);
            }
            if let Some(p) = next {
                self.history.scroll.scroll_to_item(p);
            }
        }
        match key {
            Key::Confirm if !in_list => window.focus(&self.history.list_focus),
            Key::Arrow(NavKey::Right) | Key::Confirm => {
                if self.history.file_selected.is_none() && !self.history.files.is_empty() {
                    self.select_history_file(0, cx);
                }
                window.focus(&self.history.files_focus);
            }
            Key::Menu => {
                if let (Some(i), Some(p)) = (self.history.selected, pos) {
                    let at = row_anchor(&self.history.scroll, p);
                    self.open_menu_by_key(at, MenuTarget::HistoryCompare(i), cx);
                }
            }
            _ => {}
        }
    }

    /// History files tree: arrows walk + fold; ← on a top-level row hops back to the
    /// commit list. From the search box, ⏎ moves into the tree.
    fn history_files_key(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.history_visible_files(cx);
        let cursor = self.history.files_dir_cursor.clone().or_else(|| {
            self.history
                .file_selected
                .and_then(|i| self.history.files.get(i))
                .map(|f| f.path.clone())
        });
        let at = row_of(&rows, cursor.as_deref()).map(|(i, r)| (i, r.path.clone(), r.is_dir));
        let in_tree = self.history.files_focus.is_focused(window);
        match key {
            Key::Arrow(k) => match nav(&rows, cursor.as_deref(), &self.history.files_expanded, k) {
                Nav::Select(p) => {
                    if let Some(i) = rows.iter().position(|r| r.path == p) {
                        self.history.files_scroll.scroll_to_item(i);
                    }
                    match self.history.files.iter().position(|f| f.path == p) {
                        Some(i) => {
                            self.history.files_dir_cursor = None;
                            self.select_history_file(i, cx);
                        }
                        None => self.history.files_dir_cursor = Some(p),
                    }
                }
                Nav::Expand(p) | Nav::Collapse(p) => {
                    if !self.history.files_expanded.remove(&p) {
                        self.history.files_expanded.insert(p);
                    }
                }
                Nav::Stay if matches!(k, NavKey::Left) => window.focus(&self.history.list_focus),
                Nav::Stay => {}
            },
            Key::Confirm if !in_tree => window.focus(&self.history.files_focus),
            Key::Confirm | Key::Toggle => {
                if let Some((_, p, true)) = at {
                    if !self.history.files_expanded.remove(&p) {
                        self.history.files_expanded.insert(p);
                    }
                }
            }
            Key::Menu => {
                if let Some((i, p, false)) = at {
                    if let Some(fi) = self.history.files.iter().position(|f| f.path == p) {
                        let pos = row_anchor(&self.history.files_scroll, i);
                        self.open_menu_by_key(pos, MenuTarget::HistoryFile(fi), cx);
                    }
                }
            }
            Key::Dismiss => {}
        }
    }

    /// History branch dropdown (the search box keeps focus): ↑/↓ highlight, ⏎ logs that
    /// branch (the first match when nothing's highlighted), ⎋ closes.
    fn history_revs_key(&mut self, key: Key, cx: &mut Context<Self>) {
        let items = self.history_rev_items(cx);
        let n = items.len();
        match key {
            Key::Arrow(NavKey::Down) => {
                self.history.rev_hl = Some(
                    self.history
                        .rev_hl
                        .map_or(0, |i| ui::picker::nav_down(i, n)),
                );
            }
            Key::Arrow(NavKey::Up) => {
                self.history.rev_hl = Some(
                    self.history
                        .rev_hl
                        .map_or(n.saturating_sub(1), ui::picker::nav_up),
                );
            }
            Key::Confirm => {
                if let Some(rev) = items.into_iter().nth(self.history.rev_hl.unwrap_or(0)) {
                    self.set_history_rev(rev, cx);
                }
            }
            Key::Dismiss => self.history.branch_open = false,
            _ => {}
        }
    }

    /// History compare-mode dropdown: ↑/↓ highlight, ⏎ / space apply, ⎋ closes.
    fn history_compare_key(&mut self, key: Key, cx: &mut Context<Self>) {
        let n = CompareMode::ALL.len();
        let cur = CompareMode::ALL
            .iter()
            .position(|m| *m == self.history.compare);
        let at = self.history.compare_hl.or(cur);
        match key {
            Key::Arrow(NavKey::Down) => {
                self.history.compare_hl = Some(at.map_or(0, |i| ui::picker::nav_down(i, n)));
            }
            Key::Arrow(NavKey::Up) => {
                self.history.compare_hl = Some(at.map_or(n - 1, ui::picker::nav_up));
            }
            Key::Confirm | Key::Toggle => {
                if let Some(&mode) = at.and_then(|i| CompareMode::ALL.get(i)) {
                    self.set_history_compare(mode, cx);
                }
            }
            Key::Dismiss => self.history.compare_open = false,
            _ => {}
        }
    }

    /// Branch popup (the search box keeps focus): ↑/↓ over New Branch, Pull and the branch
    /// tree; ⏎ runs the row — a folder folds, a branch opens its Checkout/Merge menu with
    /// the keyboard in it. ⏎ with no highlight acts on the first match (or creates the
    /// typed branch when nothing matches).
    fn branch_popup_key(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.branch_popup_rows(cx);
        let searching = !self.branch.query.read(cx).text().trim().is_empty();
        let first_leaf = rows
            .iter()
            .position(|r| matches!(r.node, BranchNode::Leaf { .. }))
            .map(|i| i + 2);
        let n = rows.len() + 2;
        match key {
            Key::Arrow(NavKey::Down) => {
                self.branch.hl = Some(match self.branch.hl {
                    None if searching => first_leaf.unwrap_or(0),
                    None => 0,
                    Some(i) => ui::picker::nav_down(i, n),
                });
            }
            Key::Arrow(NavKey::Up) => {
                self.branch.hl = Some(self.branch.hl.map_or(n - 1, ui::picker::nav_up));
            }
            Key::Confirm | Key::Menu => {
                let hl = match self.branch.hl {
                    Some(i) => i,
                    None if searching => first_leaf.unwrap_or(0),
                    None => return,
                };
                match hl {
                    0 => self.open_new_branch(cx),
                    1 => self.do_pull(cx),
                    i => match rows.into_iter().nth(i - 2).map(|r| r.node) {
                        Some(BranchNode::Folder { key, .. }) => self.toggle_branch_node(key, cx),
                        Some(BranchNode::Leaf { full })
                            if self.current_branch.as_deref() != Some(full.as_str()) =>
                        {
                            // The list's children: New, Pull, a hairline, then the rows.
                            let y = self
                                .branch
                                .scroll
                                .bounds_for_item(i + 1)
                                .map_or(px(200.0), |b| b.top());
                            self.open_branch_menu(full, y, true, window, cx);
                        }
                        _ => {}
                    },
                }
                return;
            }
            _ => return,
        }
        if let Some(i) = self.branch.hl {
            self.branch
                .scroll
                .scroll_to_item(if i < 2 { i } else { i + 1 });
        }
    }

    /// Worktree popup: ↑/↓ highlight, ⏎ switches, ⎋ closes.
    fn worktree_popup_key(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.worktree.list.len();
        match key {
            Key::Arrow(NavKey::Down) => {
                self.worktree.hl = Some(self.worktree.hl.map_or(0, |i| ui::picker::nav_down(i, n)));
            }
            Key::Arrow(NavKey::Up) => {
                self.worktree.hl = Some(
                    self.worktree
                        .hl
                        .map_or(n.saturating_sub(1), ui::picker::nav_up),
                );
            }
            Key::Confirm => {
                if let Some(w) = self.worktree.hl.and_then(|i| self.worktree.list.get(i)) {
                    let path = w.path.clone();
                    self.worktree.popup_open = false;
                    window.focus(&self.focus_handle);
                    self.switch_worktree(path, cx);
                }
            }
            Key::Dismiss => {
                self.worktree.popup_open = false;
                window.focus(&self.focus_handle);
            }
            _ => {}
        }
    }

    /// Projects landing (the search box keeps focus): ↑/↓ highlight, ⏎ opens (the first
    /// match when nothing is highlighted).
    fn projects_key(&mut self, key: Key, cx: &mut Context<Self>) {
        let query = self.project_search.read(cx).text().to_lowercase();
        let n = self.project_matches(&query).len();
        match key {
            Key::Arrow(NavKey::Down) => {
                self.projects_hl = Some(self.projects_hl.map_or(0, |i| ui::picker::nav_down(i, n)));
            }
            Key::Arrow(NavKey::Up) => {
                self.projects_hl = Some(
                    self.projects_hl
                        .map_or(n.saturating_sub(1), ui::picker::nav_up),
                );
            }
            Key::Confirm => {
                let pick = self
                    .project_matches(&query)
                    .get(self.projects_hl.unwrap_or(0))
                    .map(|p| (*p).clone());
                if let Some(p) = pick {
                    self.open_project(p, cx);
                }
            }
            _ => {}
        }
    }
}
