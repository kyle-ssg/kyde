//! Find across agent tabs: an agent tab's find (⌘F: this tab) can widen to its worktree's
//! tabs or, ⌘⇧F, every worktree's (`terminal::FindScope`, picked on the bar). Each tab searches
//! its own buffer;
//! stepping past its first / last match goes on into the next tab in the sidebar's order that
//! has one, showing that tab and selecting its worktree. The bar's count takes in every tab's
//! matches, counted here off the UI thread.

use super::*;
use crate::terminal::{FindScope, Searcher, TerminalView};

/// How long a query must sit still before the other tabs are counted for it.
const RECOUNT_AFTER: Duration = Duration::from_millis(150);

/// One tab a find searches: its worktree, id and terminal.
struct FindTab {
    wt: PathBuf,
    id: String,
    view: Entity<TerminalView>,
}

impl Kyde {
    /// What a scoped find's tooltip shows as the key for every worktree: Find in Files' —
    /// none where that's the terminal's own find key (Ctrl+Shift+F off macOS).
    pub(super) fn agents_find_all_key(&self) -> Option<String> {
        let own = format!("{TERMINAL_MOD}-f");
        self.keymap
            .key_for("find_in_files")
            .map(|k| keymap::platform_key(&k))
            .filter(|k| *k != own)
            .map(|k| pretty_key(&k))
    }

    /// The tabs a find from worktree `wt` on `scope` searches, in order: the sidebar's
    /// worktrees top to bottom (folded ones too), each one's tabs left to right.
    fn agents_find_tabs(&self, wt: &Path, scope: FindScope) -> Vec<FindTab> {
        let wts = match scope {
            // Never asked: a find on just its own tab stays in it.
            FindScope::Tab => Vec::new(),
            FindScope::Worktree => vec![wt.to_path_buf()],
            FindScope::All => {
                let cfg = &self.agents.cfg;
                let mut out = Vec::new();
                for (_, mut idxs) in cfg.sections() {
                    idxs.sort_by_key(|&i| cfg.spaces[i].scratch);
                    out.extend(idxs.into_iter().flat_map(|i| cfg.spaces[i].rows()));
                }
                // Tabs in a worktree the sidebar doesn't list (yet) are searched too, last.
                let mut rest: Vec<PathBuf> = self
                    .agents
                    .tabs
                    .keys()
                    .filter(|w| !out.contains(w))
                    .cloned()
                    .collect();
                rest.sort();
                out.extend(rest);
                out
            }
        };
        let mut seen = std::collections::HashSet::new();
        let mut tabs = Vec::new();
        for w in wts {
            if self.agents.hiding.contains(&w) || !seen.insert(w.clone()) {
                continue;
            }
            for t in self.agents.tabs.get(&w).map_or(&[][..], |g| &g.tabs) {
                tabs.push(FindTab {
                    wt: w.clone(),
                    id: t.id.clone(),
                    view: t.view.clone(),
                });
            }
        }
        tabs
    }

    /// Tab `v`'s scoped find changed: once the query settles, count its matches in every tab
    /// of the scope (off the UI thread), then tell `v` how many lie before and after it.
    pub(super) fn agents_find_recount(
        &mut self,
        wt: &Path,
        v: &Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) {
        let Some((query, scope)) = v.read(cx).find_query() else {
            return;
        };
        self.agents.find.gen = self.agents.find.gen.wrapping_add(1);
        let gen = self.agents.find.gen;
        let (ids, terms): (Vec<String>, Vec<Searcher>) = self
            .agents_find_tabs(wt, scope)
            .into_iter()
            .map(|t| (t.id, t.view.read(cx).searcher()))
            .unzip();
        let v = v.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RECOUNT_AFTER).await;
            // Typed on during the wait: that newer count runs instead.
            if this
                .update(cx, |this, _| this.agents.find.gen)
                .unwrap_or(gen)
                != gen
            {
                return;
            }
            let q = query.clone();
            let counts = cx
                .background_executor()
                .spawn(async move { terminal::count_matches(&terms, &q) })
                .await;
            this.update(cx, |this, cx| {
                if this.agents.find.gen == gen {
                    this.agents.find.query = query;
                    this.agents.find.counts = ids.into_iter().zip(counts).collect();
                    this.agents_find_tell(&v, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Tell tab `v` how many of the last count's matches lie in the tabs before and after it.
    fn agents_find_tell(&self, v: &Entity<TerminalView>, cx: &mut Context<Self>) {
        let (Some(wt), Some((query, scope))) = (self.agents_wt_of(v), v.read(cx).find_query())
        else {
            return;
        };
        if query != self.agents.find.query {
            return;
        }
        let tabs = self.agents_find_tabs(&wt, scope);
        let Some(at) = tabs.iter().position(|t| t.view == *v) else {
            return;
        };
        let sum = |ts: &[FindTab]| -> usize {
            ts.iter()
                .map(|t| self.agents.find.counts.get(&t.id).copied().unwrap_or(0))
                .sum()
        };
        let (before, after) = (sum(&tabs[..at]), sum(&tabs[at + 1..]));
        v.update(cx, |v, cx| v.set_find_elsewhere(before, after, cx));
    }

    /// Tab `v`'s scoped find stepped past its oldest (`older`) / newest match: on into the
    /// next tab in order with a match — up the sidebar for older, down for newer, wrapping —
    /// which is shown, its find on the match nearest `v`'s side. With none, `v`'s find wraps.
    pub(super) fn agents_find_out(
        &mut self,
        wt: &Path,
        v: &Entity<TerminalView>,
        older: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((query, scope)) = v.read(cx).find_query() else {
            return;
        };
        let tabs = self.agents_find_tabs(wt, scope);
        let n = tabs.len();
        let counted = self.agents.find.query == query;
        if let Some(at) = tabs.iter().position(|t| t.view == *v) {
            for k in 1..n {
                let t = &tabs[if older {
                    (at + n - k) % n
                } else {
                    (at + k) % n
                }];
                if counted && self.agents.find.counts.get(&t.id) == Some(&0) {
                    continue;
                }
                if t.view
                    .update(cx, |tv, cx| tv.find_land(&query, scope, older, cx))
                {
                    v.update(cx, TerminalView::end_find);
                    self.agents_go_to_tab(t.wt.clone(), &t.id, cx);
                    self.agents_find_tell(&t.view, cx);
                    return;
                }
                // Its matches went since the count (cleared).
                if counted {
                    self.agents.find.counts.insert(t.id.clone(), 0);
                }
            }
        }
        v.update(cx, |v, cx| v.find_wrap(older, cx));
    }
}
