//! Agents view — a native, herdr-style workspace for AI coding agents. **Spaces** (starting
//! folders) → **worktrees** (the space's own folder, plus linked git worktrees made from it)
//! → **tabs** (PTY terminals that start with the space's start command, `claude` by default).
//! Status dots show which agents are working and which finished while you looked elsewhere;
//! a 1 s ticker drives the dots and the idle-tab cleanup. Pure model: `crate::agents`;
//! persisted layout: `kyde_config::agents`. Crate-root child module.

use crate::agents::AgentState;
use crate::*;
use kyde_config::agents::CleanupMode;
use kyde_config::agents::Space;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

mod ci;
#[cfg(feature = "terminal")]
mod context;

/// How often the status/cleanup ticker runs, and how many ticks between cleanup sweeps.
#[cfg(feature = "terminal")]
const TICK: Duration = Duration::from_secs(1);
#[cfg(feature = "terminal")]
const SWEEP_EVERY_TICKS: u32 = 60;
/// AI "is it done?" checks per cleanup pass (each is a few-second `claude -p` call).
#[cfg(feature = "terminal")]
const MAX_AI_CHECKS: usize = 3;
/// How often each space's merged PRs are re-fetched.
#[cfg(feature = "terminal")]
const MERGED_EVERY: Duration = Duration::from_mins(10);
#[cfg(feature = "terminal")]
const GIT_EVERY_TICKS: u32 = 5;
/// How often running agent hosts are checked for ones to adopt (started by the CLI).
#[cfg(all(feature = "terminal", unix))]
const HOST_SCAN_TICKS: u32 = 3;
/// How often a worktree showing the background agents at work in it repaints (their "last
/// reported" ages are in minutes).
#[cfg(feature = "terminal")]
const SUB_AGE_TICKS: u32 = 15;
/// How often the open Issues / Pull Requests lists re-fetch (PRs merge, issues close), and
/// how old they may be when the tab is shown again.
#[cfg(feature = "terminal")]
const GH_EVERY_TICKS: u32 = 2 * 60;
const GH_STALE: Duration = Duration::from_mins(1);
/// Agent titles/context: re-read every 5 minutes…
#[cfg(feature = "terminal")]
const CONTEXT_EVERY_TICKS: u32 = 5 * 60;
/// …and once this long after a tab's first prompt (time for Claude Code to title it).
#[cfg(feature = "terminal")]
const TITLE_AFTER_PROMPT: Duration = Duration::from_secs(15);
/// An agent tab this new, with no hook report yet, counts as alive: its agent is starting.
#[cfg(feature = "terminal")]
const FEEDBACK_STARTING: Duration = Duration::from_mins(1);
/// One tab for the cleanup's background probe: worktree, spawn time, shell pid, last output,
/// and the AI question to ask about it (if any).
#[cfg(feature = "terminal")]
type Probe = (
    PathBuf,
    std::time::SystemTime,
    u32,
    Option<Instant>,
    Option<String>,
);

/// One worktree's agent tabs (by spawn time) and the Claude Code session each was matched to.
type SessionMatches = (PathBuf, Vec<(std::time::SystemTime, Option<String>)>);

/// How often a space's own checkout is fetched + fast-forwarded to its remote mainline.
const MAIN_SYNC_EVERY: Duration = Duration::from_mins(5);
// Sidebar row geometry, shared so the tree lines meet the dots exactly.
/// Height of a row's title line (dot, chevron, actions centre on it).
const TITLE_H: f32 = 18.0;
/// Row padding: left (space rows) and top/bottom.
const ROW_PAD_X: f32 = 4.0;
const ROW_PAD_Y: f32 = 1.0;
/// Fold-chevron column, status dot, and the gap between title-line items.
const CHEVRON_W: f32 = 14.0;
const DOT: f32 = 10.0;
const GAP: f32 = 8.0;
/// Space between two tabs of a worktree's strip (px) — the drop line sits in the middle.
#[cfg(feature = "terminal")]
const AGENT_TAB_GAP: f32 = 4.0;
/// A worktree row's tree line sits under its space's dot centre…
const WT_LINE_X: f32 = ROW_PAD_X + CHEVRON_W + GAP + DOT / 2.0;
/// …and its own dot starts a stub's length to the right.
const WT_DOT_X: f32 = WT_LINE_X + 16.0;
/// How far a group's projects sit in from its header.
const GROUP_INDENT: f32 = 12.0;

impl Kyde {
    /// Enter the Agents view: refresh the branch labels off-thread and focus the visible tab.
    pub(crate) fn enter_agents(&mut self, cx: &mut Context<Self>) {
        self.mode = Mode::Agents;
        self.diff_view_open = false;
        self.agents.focus_pending = true;
        self.agents_sync_hooks(cx);
        self.agents_autostart(cx);
        self.agents_refresh_branches(cx);
        self.agents_refresh_contexts(cx);
        self.agents_sync_mains(cx);
        cx.notify();
    }

    /// Keep each space's own checkout up to date with its remote mainline (`agents::sync_main`:
    /// fetch + fast-forward, only on the default branch and clean) — at most every few
    /// minutes, in the background; refreshes the rows after.
    fn agents_sync_mains(&mut self, cx: &mut Context<Self>) {
        if self.agents.main_syncing
            || self
                .agents
                .main_synced_at
                .is_some_and(|t| t.elapsed() < MAIN_SYNC_EVERY)
        {
            return;
        }
        self.agents.main_syncing = true;
        let spaces: Vec<PathBuf> = self
            .agents
            .cfg
            .spaces
            .iter()
            .filter(|s| !s.scratch)
            .map(|s| s.path.clone())
            .collect();
        cx.spawn(async move |this, cx| {
            let res: Vec<(PathBuf, Option<crate::agents::MainSync>)> = cx
                .background_executor()
                .spawn(async move {
                    spaces
                        .into_iter()
                        .map(|s| {
                            let r = crate::agents::sync_main(&s);
                            (s, r)
                        })
                        .collect()
                })
                .await;
            this.update(cx, |this, cx| {
                this.agents.main_syncing = false;
                this.agents.main_synced_at = Some(Instant::now());
                for (space, r) in res {
                    match r {
                        Some(r) => this.agents.main_sync.insert(space, r),
                        None => this.agents.main_sync.remove(&space),
                    };
                }
                this.agents_refresh_branches(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// On launch, resume every worktree's Claude sessions in the background — the tabs open
    /// at the last quit, then each other worktree's latest session. The selection and focus
    /// stay where they are.
    fn agents_restore_saved(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "terminal")]
        self.agents_load_done(cx);
        // Agents still running in their hosts come back first (exactly as they are); only
        // worktrees left without one resume a session.
        #[cfg(all(feature = "terminal", unix))]
        if crate::agents::hosted() {
            self.agents.hosts_scanning = true;
            cx.spawn(async move |this, cx| {
                let live = cx
                    .background_executor()
                    .spawn(async { crate::agent_host::live_hosts() })
                    .await;
                this.update(cx, |this, cx| {
                    this.agents.hosts_scanning = false;
                    this.agents_reattach(live, true, cx);
                    this.agents_resume_rest(cx);
                })
                .ok();
            })
            .detach();
            return;
        }
        self.agents_resume_rest(cx);
    }

    /// Reopen tabs on running agent hosts: at launch (`launch`) the saved tabs in their
    /// order, then any host nobody saved (the CLI started it, or Kyde crashed) whose folder is
    /// a known worktree — the latter also every few seconds after.
    #[cfg(all(feature = "terminal", unix))]
    fn agents_reattach(
        &mut self,
        live: Vec<crate::agent_host::HostInfo>,
        launch: bool,
        cx: &mut Context<Self>,
    ) {
        let open: std::collections::HashSet<String> = self
            .agents
            .tabs
            .values()
            .flat_map(|g| g.tabs.iter().map(|t| t.id.clone()))
            .collect();
        let mut live: std::collections::HashMap<String, crate::agent_host::HostInfo> = live
            .into_iter()
            .filter(|h| !open.contains(&h.id) && !self.agents.closed_hosts.contains(&h.id))
            .map(|h| (h.id.clone(), h))
            .collect();
        if live.is_empty() {
            return;
        }
        let selected = self.agents.selected.clone();
        if launch {
            let saved: Vec<(PathBuf, Vec<kyde_config::agents::SavedTab>)> = self
                .agents
                .cfg
                .tabs
                .iter()
                .filter(|(wt, _)| self.agents.cfg.space_of(wt).is_some())
                .map(|(wt, t)| (wt.clone(), t.clone()))
                .collect();
            for (wt, tabs) in saved {
                for t in &tabs {
                    if let Some(id) = t.id.as_deref().filter(|id| live.remove(*id).is_some()) {
                        self.agents_attach_tab(wt.clone(), t, id, cx);
                    }
                }
            }
        }
        let known: Vec<PathBuf> = self
            .agents
            .cfg
            .spaces
            .iter()
            .flat_map(|s| std::iter::once(&s.path).chain(&s.worktrees))
            .cloned()
            .collect();
        let mut rest: Vec<crate::agent_host::HostInfo> = live.into_values().collect();
        rest.sort_by_key(|h| h.started);
        for h in rest {
            if let Some(wt) = known.iter().find(|w| **w == h.cwd) {
                let saved = kyde_config::agents::SavedTab {
                    agent: h.agent,
                    ..Default::default()
                };
                self.agents_attach_tab(wt.clone(), &saved, &h.id, cx);
            }
        }
        // Reattaching doesn't move you: the selection, and its tab, stay put.
        self.agents.selected = selected;
        cx.notify();
    }

    /// Adopt agent hosts started outside Kyde (`ky agent new`) — a quick background scan.
    #[cfg(all(feature = "terminal", unix))]
    fn agents_scan_hosts(&mut self, cx: &mut Context<Self>) {
        if self.agents.hosts_scanning || !self.agents.hosts_restored || !crate::agents::hosted() {
            return;
        }
        self.agents.hosts_scanning = true;
        cx.spawn(async move |this, cx| {
            let live = cx
                .background_executor()
                .spawn(async { crate::agent_host::live_hosts() })
                .await;
            this.update(cx, |this, cx| {
                this.agents.hosts_scanning = false;
                // Ones that wound down since a close needn't be remembered any more.
                this.agents
                    .closed_hosts
                    .retain(|id| live.iter().any(|h| &h.id == id));
                this.agents_reattach(live, false, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Launch, after reattaching: resume the Claude sessions of worktrees that have no agent
    /// running, then the autostart.
    fn agents_resume_rest(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "terminal")]
        {
            // Only real conversations come back (a known Claude session, resumed); saved tabs
            // without one were never matched to any work — reopening them would just start
            // fresh, pointless agents — so they're dropped.
            let saved: Vec<(PathBuf, Vec<String>)> = self
                .agents
                .cfg
                .tabs
                .iter()
                .filter(|(wt, _)| {
                    self.agents.cfg.space_of(wt).is_some() && !self.agents.restored.contains(*wt)
                })
                .map(|(wt, tabs)| {
                    let sessions = tabs
                        .iter()
                        .filter(|t| t.agent)
                        .filter_map(|t| t.session.clone())
                        .collect();
                    (wt.clone(), sessions)
                })
                .collect();
            let mut resuming: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
            for (wt, sessions) in saved {
                if sessions.is_empty() {
                    self.agents.cfg.tabs.remove(&wt);
                    continue;
                }
                resuming.insert(wt.clone());
                self.agents_resume_when_idle(wt, sessions, cx);
            }
            // Every other worktree with a Claude session resumes its latest one too — all in
            // the background, so clicking a row just shows it.
            let latest: Vec<(PathBuf, String)> = self
                .agents
                .cfg
                .spaces
                .iter()
                .flat_map(|s| std::iter::once(&s.path).chain(&s.worktrees))
                .filter(|wt| !resuming.contains(*wt) && !self.agents.restored.contains(*wt))
                .filter_map(|wt| {
                    let id = self.agents.contexts.get(wt)?.session.clone()?;
                    Some((wt.clone(), id))
                })
                .collect();
            for (wt, id) in latest {
                self.agents_resume_when_idle(wt, vec![id], cx);
            }
            self.agents_sync_hooks(cx);
            self.agents_save(cx);
        }
        self.agents.hosts_restored = true;
        if self.mode == Mode::Agents {
            self.agents_autostart(cx);
        }
    }

    /// Resume `sessions` in worktree `wt` (a project's own checkout catches up with main
    /// first) — unless something opened tabs there meanwhile (e.g. the selected one's
    /// autostart), so a session never runs twice.
    #[cfg(feature = "terminal")]
    fn agents_resume_when_idle(
        &mut self,
        wt: PathBuf,
        sessions: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.agents_after_main_sync(wt.clone(), cx, move |this, cx| {
            if this
                .agents
                .tabs
                .get(&wt)
                .is_some_and(|g| !g.tabs.is_empty())
            {
                return;
            }
            for id in sessions {
                this.agents_resume_tab(wt.clone(), id, cx);
            }
        });
    }

    /// Once per launch, start the selected worktree's agent if it has no tab — so opening the
    /// view (or finishing setup) lands on a running agent, not an empty pane.
    fn agents_autostart(&mut self, cx: &mut Context<Self>) {
        // Wait for the session list, so the agent resumes its last session (the context
        // refresh calls back here once it lands).
        if self.agents.autostarted
            || !self.agents.cfg.setup_done
            || !self.agents.contexts_loaded
            || !self.agents.hosts_restored
        {
            return;
        }
        self.agents.autostarted = true;
        #[cfg(feature = "terminal")]
        if let Some(wt) = self
            .agents
            .selected
            .clone()
            .filter(|w| self.agents.cfg.space_of(w).is_some())
        {
            self.agents_open_tabs(wt, cx);
        }
        #[cfg(not(feature = "terminal"))]
        let _ = cx;
    }

    /// Install (or, when switched off, remove) the Claude Code hook that reports each agent's
    /// state to Kyde — once per launch, in the background; only writes when it changes.
    pub(crate) fn agents_sync_hooks(&mut self, cx: &mut Context<Self>) {
        // A screenshot run never touches Claude Code's settings.
        if self.agents.hooks_synced || std::env::var_os("KYDE_SHOT").is_some() {
            return;
        }
        self.agents.hooks_synced = true;
        let on = self.agents.cfg.claude_hooks;
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    let _ = std::fs::create_dir_all(kyde_config::agents::agents_dir());
                    kyde_config::claude_hooks::apply(
                        &kyde_config::claude_hooks::settings_path(),
                        on,
                    )
                })
                .await;
            if let Err(e) = r {
                this.update(cx, |this, cx| {
                    this.agents.note = Some(format!("Couldn't update Claude Code's hooks: {e}"));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Persist the layout once typing/clicking settles (300 ms), off the UI thread.
    pub(crate) fn agents_save(&mut self, cx: &mut Context<Self>) {
        self.agents_snapshot_tabs();
        self.agents.save_gen = self.agents.save_gen.wrapping_add(1);
        let generation = self.agents.save_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let cfg = this
                .update(cx, |this, _| {
                    (this.agents.save_gen == generation).then(|| this.agents.cfg.clone())
                })
                .ok()
                .flatten();
            if let Some(cfg) = cfg {
                cx.background_executor()
                    .spawn(async move { cfg.save() })
                    .await;
            }
        })
        .detach();
    }

    /// Re-read every worktree's branch (and which spaces are git repos) on a background
    /// thread — git never runs on the UI thread.
    fn agents_refresh_branches(&mut self, cx: &mut Context<Self>) {
        if self.agents.git_refreshing {
            return;
        }
        self.agents.git_refreshing = true;
        let spaces: Vec<PathBuf> = self
            .agents
            .cfg
            .spaces
            .iter()
            .filter(|s| !s.scratch)
            .map(|s| s.path.clone())
            .collect();
        cx.spawn(async move |this, cx| {
            // 1. One `git worktree list` per space: every worktree + its branch, so the rows
            //    and their names are on screen almost at once.
            let (branches, git_spaces, listed) = cx
                .background_executor()
                .spawn(async move {
                    let mut branches = std::collections::HashMap::new();
                    let mut git_spaces = std::collections::HashSet::new();
                    let mut listed = std::collections::HashMap::new();
                    for space in spaces {
                        let Some((own, others)) = crate::agents::repo_worktrees(&space) else {
                            continue;
                        };
                        if let Some(b) = own {
                            branches.insert(space.clone(), b);
                        }
                        let mut paths = Vec::with_capacity(others.len());
                        for (p, b) in others {
                            if let Some(b) = b {
                                branches.insert(p.clone(), b);
                            }
                            paths.push(p);
                        }
                        listed.insert(space.clone(), paths);
                        git_spaces.insert(space);
                    }
                    (branches, git_spaces, listed)
                })
                .await;
            let Ok(all) = this.update(cx, |this, cx| {
                this.agents.branches = branches;
                this.agents.git_spaces = git_spaces;
                if this.agents_adopt_worktrees(listed) {
                    this.agents_save(cx);
                }
                cx.notify();
                this.agents
                    .cfg
                    .spaces
                    .iter()
                    .filter(|s| !s.scratch)
                    .flat_map(|s| std::iter::once(&s.path).chain(&s.worktrees))
                    .cloned()
                    .collect::<Vec<_>>()
            }) else {
                return;
            };
            // 2. Changed-files + unpushed counts for every worktree, in parallel.
            let read = cx
                .background_executor()
                .spawn(async move { crate::agents::per_worktree(all, crate::agents::read_git) })
                .await;
            this.update(cx, |this, cx| {
                this.agents.git.clear();
                for (wt, r) in read {
                    if let Some((branch, g)) = r {
                        if let Some(b) = branch {
                            this.agents.branches.insert(wt.clone(), b);
                        }
                        this.agents.git.insert(wt, g);
                    }
                }
                this.agents.git_refreshing = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Put the live tabs into the saved layout: worktrees opened this launch save what's open
    /// now (none → dropped); ones not reopened yet keep what they had; gone ones are pruned.
    fn agents_snapshot_tabs(&mut self) {
        #[cfg(feature = "terminal")]
        {
            let (cfg, live, restored) = (
                &mut self.agents.cfg,
                &self.agents.tabs,
                &self.agents.restored,
            );
            let known: std::collections::HashSet<PathBuf> = cfg
                .spaces
                .iter()
                .flat_map(|s| std::iter::once(&s.path).chain(&s.worktrees))
                .cloned()
                .collect();
            cfg.tabs
                .retain(|wt, _| known.contains(wt) && !restored.contains(wt));
            for (wt, g) in live {
                cfg.tabs.insert(
                    wt.clone(),
                    g.tabs
                        .iter()
                        .map(|t| kyde_config::agents::SavedTab {
                            agent: t.agent,
                            session: t.session.clone(),
                            id: Some(t.id.clone()),
                            name: t.name.clone(),
                        })
                        .collect(),
                );
            }
        }
    }

    /// Re-read every worktree's agent context (Claude Code's own title + prompts) in the
    /// background. Runs once a tab's first prompt has had time to be titled, then every few
    /// minutes — not with the 5 s git refresh: titles change on the task's timescale.
    pub(crate) fn agents_refresh_contexts(&mut self, cx: &mut Context<Self>) {
        let all: Vec<PathBuf> = self
            .agents
            .cfg
            .spaces
            .iter()
            .flat_map(|s| std::iter::once(&s.path).chain(&s.worktrees))
            .cloned()
            .collect();
        let wanted = self.agents_sessions_wanted();
        cx.spawn(async move |this, cx| {
            let (read, found) = cx
                .background_executor()
                .spawn(async move {
                    let read = crate::agents::per_worktree(all, crate::agents::read_agent_context);
                    // Which Claude Code session each new agent tab started (for resume).
                    let found: Vec<SessionMatches> = wanted
                        .into_iter()
                        .map(|(wt, spawns, claimed)| {
                            let since = spawns
                                .iter()
                                .min()
                                .copied()
                                .unwrap_or(std::time::UNIX_EPOCH);
                            let cands = crate::agents::sessions_since(&wt, since);
                            let ids = crate::agents::assign_sessions(&spawns, &cands, &claimed);
                            (wt, spawns.into_iter().zip(ids).collect())
                        })
                        .collect();
                    (read, found)
                })
                .await;
            this.update(cx, |this, cx| {
                this.agents.contexts = read
                    .into_iter()
                    .filter_map(|(wt, c)| Some((wt, c?)))
                    .collect();
                // The first read unblocks the launch auto-start (now it can resume).
                if !this.agents.contexts_loaded {
                    this.agents.contexts_loaded = true;
                    // Reattach / resume, then the autostart.
                    this.agents_restore_saved(cx);
                }
                if this.agents_take_sessions(found) {
                    this.agents_save(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Agent tabs that don't know their Claude Code session yet, per worktree: their spawn
    /// times (sorted) and the session ids that worktree's other tabs already hold.
    #[allow(clippy::type_complexity)]
    fn agents_sessions_wanted(
        &self,
    ) -> Vec<(
        PathBuf,
        Vec<std::time::SystemTime>,
        std::collections::HashSet<String>,
    )> {
        #[cfg(feature = "terminal")]
        {
            self.agents
                .tabs
                .iter()
                .filter_map(|(wt, g)| {
                    let mut spawns: Vec<_> = g
                        .tabs
                        .iter()
                        .filter(|t| t.agent && t.session.is_none())
                        .map(|t| t.spawned_at)
                        .collect();
                    if spawns.is_empty() {
                        return None;
                    }
                    spawns.sort();
                    let claimed = g.tabs.iter().filter_map(|t| t.session.clone()).collect();
                    Some((wt.clone(), spawns, claimed))
                })
                .collect()
        }
        #[cfg(not(feature = "terminal"))]
        Vec::new()
    }

    /// Record the sessions matched to tabs (by spawn time). Returns whether any was learned.
    fn agents_take_sessions(&mut self, found: Vec<SessionMatches>) -> bool {
        #[cfg_attr(not(feature = "terminal"), allow(unused_mut))]
        let mut learned = false;
        #[cfg(feature = "terminal")]
        for (wt, ids) in found {
            let Some(g) = self.agents.tabs.get_mut(&wt) else {
                continue;
            };
            for (spawn, id) in ids {
                let Some(id) = id else { continue };
                if let Some(t) = g
                    .tabs
                    .iter_mut()
                    .find(|t| t.spawned_at == spawn && t.session.is_none())
                {
                    t.session = Some(id);
                    learned = true;
                }
            }
        }
        #[cfg(not(feature = "terminal"))]
        let _ = found;
        learned
    }

    /// Make each space's worktree list what git reports (`listed`, keyed by space), keeping
    /// any worktree git no longer lists while it still has live tabs (its agent keeps
    /// running). Returns whether anything changed (→ persist).
    fn agents_adopt_worktrees(
        &mut self,
        mut listed: std::collections::HashMap<PathBuf, Vec<PathBuf>>,
    ) -> bool {
        let mut changed = false;
        for s in &mut self.agents.cfg.spaces {
            // `mut` only matters with live tabs to keep (terminal builds).
            #[cfg_attr(not(feature = "terminal"), allow(unused_mut))]
            let Some(mut list) = listed.remove(&s.path) else {
                continue;
            };
            // Ones being removed in the background stay hidden until git's done.
            list.retain(|w| !self.agents.hiding.contains(w));
            #[cfg(feature = "terminal")]
            for w in &s.worktrees {
                if !list.contains(w) && self.agents.tabs.contains_key(w) {
                    list.push(w.clone());
                }
            }
            // Your order (dragged) stays; git only adds and drops worktrees.
            let merged = crate::agents::keep_order(&s.worktrees, list);
            if merged != s.worktrees {
                s.worktrees = merged;
                changed = true;
            }
        }
        changed
    }

    /// "Add space" — pick a starting folder with the native folder picker.
    pub(crate) fn agents_add_space_prompt(&mut self, cx: &mut Context<Self>) {
        self.prompt_folder("Add Project", cx, Self::agents_add_space);
    }

    /// Add `path` as a space (no-op if it already is one) and open it — its own folder is
    /// the default worktree, so selecting it starts the first agent tab.
    pub(crate) fn agents_add_space(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.agents_ensure_space(&path, cx);
        self.agents_select(path, cx);
    }

    /// Make `path` a space unless it's one already — or one of a space's worktrees.
    pub(crate) fn agents_ensure_space(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.agents.cfg.space_of(path).is_some() {
            return;
        }
        let at = self.agents.cfg.new_space_index();
        self.agents.cfg.spaces.insert(
            at,
            Space {
                path: path.to_path_buf(),
                ..Space::default()
            },
        );
        self.agents_save(cx);
        self.agents_refresh_branches(cx);
    }

    /// Drop a space from the sidebar (its folder and worktrees stay on disk) and close its tabs.
    fn agents_remove_space(&mut self, space: &Path, cx: &mut Context<Self>) {
        let Some(i) = self.agents.cfg.spaces.iter().position(|s| s.path == space) else {
            return;
        };
        let removed = self.agents.cfg.spaces.remove(i);
        if self.agents.gh_scope.as_ref() == Some(&removed.path) {
            self.agents.gh_scope = None;
        }
        for wt in std::iter::once(&removed.path).chain(&removed.worktrees) {
            self.agents.cfg.pinned.remove(wt);
        }
        #[cfg(feature = "terminal")]
        for wt in std::iter::once(&removed.path).chain(&removed.worktrees) {
            if let Some(g) = self.agents.tabs.remove(wt) {
                for t in &g.tabs {
                    t.view.read(cx).kill_host();
                }
            }
        }
        if self
            .agents
            .selected
            .as_deref()
            .is_some_and(|s| s == removed.path || removed.worktrees.iter().any(|w| w == s))
        {
            self.agents.selected = self.agents.cfg.spaces.first().map(|s| s.path.clone());
        }
        self.agents_save(cx);
        cx.notify();
    }

    /// Show worktree `wt`; a worktree with no live tab starts its first agent tab.
    pub(crate) fn agents_select(&mut self, wt: PathBuf, cx: &mut Context<Self>) {
        self.agents_reveal(&wt);
        self.agents.selected = Some(wt.clone());
        if self.agents.cfg.selected.as_ref() != Some(&wt) {
            self.agents.cfg.selected = Some(wt.clone());
            self.agents_save(cx);
        }
        #[cfg(feature = "terminal")]
        self.agents_open_tabs(wt.clone(), cx);
        // The code views follow: its folder is the open project.
        #[cfg(feature = "terminal")]
        self.agents_sync_project(wt, cx);
        self.agents.focus_pending = true;
        cx.notify();
    }

    /// Create a linked worktree for `space` (`~/.kyde/worktrees/<space>/<name>` on a new
    /// `worktree/<name>` branch off the freshly fetched remote mainline) in the background,
    /// then open it.
    fn agents_new_worktree(&mut self, space: PathBuf, cx: &mut Context<Self>) {
        let name = crate::agents::worktree_name(crate::agents::random_seed());
        let branch = format!("worktree/{name}");
        self.agents_create_worktree(space, name, WtSource::NewBranch(branch), None, cx);
    }

    /// A new Scratch row: a fresh folder under `~/.kyde/scratch` (made — and trusted in Claude
    /// Code, so its agent doesn't open on the trust question — in the background), then
    /// opened, which starts its agent.
    pub(crate) fn agents_new_scratch(&mut self, cx: &mut Context<Self>) {
        let root = kyde_config::agents::scratch_dir();
        let path = root.join(crate::agents::worktree_name(crate::agents::random_seed()));
        // Tests and screenshot runs never touch Claude Code's config.
        let trust = !cfg!(test) && std::env::var_os("KYDE_SHOT").is_none();
        cx.spawn(async move |this, cx| {
            let dir = path.clone();
            let made = cx
                .background_executor()
                .spawn(async move {
                    std::fs::create_dir_all(&dir)?;
                    if trust {
                        let root = root.canonicalize().unwrap_or(root);
                        let path = kyde_config::claude_trust::config_path();
                        // Untrusted, its agent just asks first — not worth a note.
                        let _ = kyde_config::claude_trust::apply(&path, &root);
                    }
                    Ok::<(), std::io::Error>(())
                })
                .await;
            this.update(cx, |this, cx| {
                if let Err(e) = made {
                    this.agents.note = Some(format!("Couldn't make a scratch folder: {e}"));
                    cx.notify();
                    return;
                }
                this.agents.cfg.ensure_scratch();
                if let Some(s) = this.agents.cfg.spaces.iter_mut().find(|s| s.scratch) {
                    s.worktrees.push(path.clone());
                }
                this.agents_save(cx);
                this.agents_select(path, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Create worktree `name` for `space` (`~/.kyde/worktrees/<space>/<name>`) from `source`
    /// in the background, then open it — starting its agent with `prompt` when given (an
    /// issue to work on, a PR to review).
    fn agents_create_worktree(
        &mut self,
        space: PathBuf,
        name: String,
        source: WtSource,
        start: Option<WtStart>,
        cx: &mut Context<Self>,
    ) {
        let path = kyde_config::agents::worktree_path(&space, &name);
        // Already on its way (a second click).
        if self.agents.creating.iter().any(|c| c.path == path) {
            self.agents.side_tab = SideTab::Code;
            self.agents.selected = Some(path);
            cx.notify();
            return;
        }
        self.agents_reveal(&space);
        let doing = start
            .as_ref()
            .map_or_else(|| format!("Creating worktree {name}"), |s| s.doing.clone());
        self.agents.creating.push(Creating {
            space: space.clone(),
            name,
            path: path.clone(),
            doing,
        });
        // Straight to it: its setup screen shows until the agent starts.
        self.agents.side_tab = SideTab::Code;
        self.agents.selected = Some(path.clone());
        self.agents.note = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (repo_dir, dest, src) = (space.clone(), path.clone(), source.clone());
            let res = cx
                .background_executor()
                .spawn(async move {
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    let repo = Repo::discover(&repo_dir).map_err(|e| e.to_string())?;
                    match src {
                        // Fork from a freshly fetched remote mainline when there is one (an
                        // offline fetch just forks from the last-fetched one), else from HEAD.
                        WtSource::NewBranch(br) => {
                            let base = repo.remote_default();
                            if let Some(b) = &base {
                                let _ = repo.fetch_remote_branch(b);
                            }
                            repo.add_worktree(&dest, &br, base.as_deref())
                        }
                        // Checked out once, straight at the PR's head (fetched first), then
                        // `gh pr checkout` gives it the PR's branch — no files to change by then.
                        // If that fails the half-made worktree goes again.
                        WtSource::Pr(n) => {
                            let at = repo.fetch_pr_head(n).ok();
                            repo.add_worktree_detached(&dest, at.as_deref())
                                .and_then(|()| {
                                    kyde_git::github::pr_checkout(&dest, n).inspect_err(|_| {
                                        let _ = repo.remove_worktree(&dest);
                                    })
                                })
                        }
                    }
                    .map_err(|e| e.to_string())
                })
                .await;
            this.update(cx, |this, cx| {
                this.agents.creating.retain(|c| c.path != path);
                match res {
                    Ok(()) => {
                        // A refresh while git was at it may have listed it already.
                        if let Some(s) = this.agents.cfg.spaces.iter_mut().find(|s| s.path == space)
                        {
                            if !s.worktrees.contains(&path) {
                                s.worktrees.push(path.clone());
                            }
                        }
                        if let WtSource::NewBranch(b) = source {
                            this.agents.branches.insert(path.clone(), b);
                        }
                        if let Some(s) = &start {
                            this.agents
                                .cfg
                                .origins
                                .insert(path.clone(), s.origin.clone());
                        }
                        this.agents_save(cx);
                        #[cfg(feature = "terminal")]
                        match start {
                            Some(s) => {
                                // Made to act on review comments: on every one queued by now.
                                let prompt = this.agents_feedback_take(&path).unwrap_or(s.prompt);
                                this.agents_new_tab_as(path.clone(), Some(prompt), Some(s.tab), cx);
                            }
                            // Made for you to work in: its agent starts if you're still here.
                            None if this.agents.selected.as_ref() == Some(&path) => {
                                this.agents_new_tab(path.clone(), true, None, cx);
                            }
                            None => {}
                        }
                        #[cfg(not(feature = "terminal"))]
                        let _ = start;
                        // Only if you're still looking at its setup screen.
                        if this.agents.selected.as_ref() == Some(&path) {
                            this.agents_select(path, cx);
                        }
                        this.agents_refresh_branches(cx);
                    }
                    Err(e) => {
                        this.agents.note = Some(format!(
                            "Couldn't create worktree: {}",
                            crate::agents::git_reason(&e)
                        ));
                        // Review comments waiting for it have nowhere to go.
                        #[cfg(feature = "terminal")]
                        this.agents.feedback.remove(&path);
                        if this.agents.selected.as_ref() == Some(&path) {
                            this.agents.selected = Some(space.clone());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Switch the sidebar list; Issues / Pull Requests load their project's list if needed.
    pub(crate) fn agents_show_tab(&mut self, tab: SideTab, cx: &mut Context<Self>) {
        self.agents.side_tab = tab;
        if tab == SideTab::Code {
            self.agents.gh_scope = None;
        }
        let missing: Vec<PathBuf> = self
            .agents
            .cfg
            .spaces
            .iter()
            .map(|s| s.path.clone())
            .filter(|p| match tab {
                SideTab::Code => false,
                SideTab::Issues => !self.agents.issues.contains_key(p),
                SideTab::Prs => !self.agents.prs.contains_key(p),
                SideTab::Ci => !self.agents.ci.runs.contains_key(p),
            })
            .collect();
        let stale = tab != SideTab::Code
            && self
                .agents
                .gh_fetched
                .get(&tab)
                .is_none_or(|at| at.elapsed() > GH_STALE);
        if stale {
            self.agents_refresh_gh(tab, true, cx);
        }
        for space in missing {
            self.agents_fetch_gh(space, tab, false, cx);
        }
        cx.notify();
    }

    /// A project row's jump: `tab` narrowed to `space` alone, unfolded, listing everyone's
    /// (this project's PRs — not only the ones waiting on you; every run, not only failures).
    fn agents_jump_gh(&mut self, space: PathBuf, tab: SideTab, cx: &mut Context<Self>) {
        use kyde_git::github::{IssueFilter, PrFilter};
        let widened = match tab {
            SideTab::Issues => {
                std::mem::replace(&mut self.agents.issue_filter, IssueFilter::All)
                    != IssueFilter::All
            }
            SideTab::Prs => {
                std::mem::replace(&mut self.agents.pr_filter, PrFilter::All) != PrFilter::All
            }
            // CI filters as it renders — nothing to refetch.
            SideTab::Ci => {
                self.agents.ci.filter = None;
                false
            }
            SideTab::Code => false,
        };
        // A new filter makes what's loaded stale: `agents_show_tab` refetches it.
        if widened {
            self.agents.gh_fetched.remove(&tab);
        }
        self.agents_reveal(&space);
        self.agents_save(cx);
        self.agents.gh_scope = Some(space);
        self.agents_show_tab(tab, cx);
    }

    /// Whether the Issues / Pull Requests / CI tabs show `space` (all, unless narrowed).
    fn agents_in_scope(&self, space: &Path) -> bool {
        self.agents.gh_scope.as_deref().is_none_or(|s| s == space)
    }

    /// The Issues / Pull Requests search text.
    fn agents_gh_query(&self, cx: &gpui::App) -> String {
        self.agents.gh_query.read(cx).text().trim().to_string()
    }

    /// The search changed: re-query once typing pauses (400 ms).
    pub(crate) fn agents_query_changed(&mut self, cx: &mut Context<Self>) {
        // The CI list is filtered as it renders — nothing to re-query.
        if self.agents.side_tab == SideTab::Ci {
            cx.notify();
            return;
        }
        self.agents.gh_query_gen = self.agents.gh_query_gen.wrapping_add(1);
        let generation = self.agents.gh_query_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            this.update(cx, |this, cx| {
                if this.agents.gh_query_gen == generation {
                    let tab = this.agents.side_tab;
                    this.agents_refresh_gh(tab, false, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The filter row under the header on the Issues / Pull Requests tabs: whose (pills) and a
    /// GitHub search box.
    fn render_agents_gh_filters(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use kyde_git::github::{IssueFilter, PrFilter};
        let t = theme::get();
        let issues = self.agents.side_tab == SideTab::Issues;
        let (options, selected): (&[&'static str], usize) = if self.agents.side_tab == SideTab::Ci {
            (
                &ci::CI_FILTER_LABELS,
                ci::filter_index(self.agents.ci.filter),
            )
        } else if issues {
            (
                &["Mine", "All", "Unassigned"],
                match self.agents.issue_filter {
                    IssueFilter::Mine => 0,
                    IssueFilter::All => 1,
                    IssueFilter::Unassigned => 2,
                },
            )
        } else {
            (
                &["Review", "Mine", "All"],
                match self.agents.pr_filter {
                    PrFilter::ReviewRequested => 0,
                    PrFilter::Mine => 1,
                    PrFilter::All => 2,
                },
            )
        };
        let filter = ui::select(
            cx,
            "gh-filter",
            118.0,
            options,
            Some(selected),
            &self.agents.gh_filter_sel,
            |k: &mut Kyde| &mut k.agents.gh_filter_sel,
            |_this, _cx| {},
            move |this, i, cx| {
                if this.agents.side_tab == SideTab::Ci {
                    this.agents.ci.filter = ci::filter_at(i);
                    cx.notify();
                    return;
                }
                if this.agents.side_tab == SideTab::Issues {
                    this.agents.issue_filter =
                        [IssueFilter::Mine, IssueFilter::All, IssueFilter::Unassigned][i.min(2)];
                } else {
                    this.agents.pr_filter =
                        [PrFilter::ReviewRequested, PrFilter::Mine, PrFilter::All][i.min(2)];
                }
                let tab = this.agents.side_tab;
                this.agents_refresh_gh(tab, false, cx);
            },
        );
        // Narrowed to one project (a project row's jump): its name, click to show them all.
        let scope = self.agents.gh_scope.as_ref().map(|p| {
            let name = self
                .agents
                .cfg
                .spaces
                .iter()
                .find(|s| &s.path == p)
                .map_or_else(|| leaf_name(p), |s| self.agents_space_name(s));
            div()
                .id("agents-gh-scope")
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .flex_shrink()
                .min_w_0()
                .max_w(px(120.0))
                .pl_2()
                .pr_1p5()
                .py_1p5()
                .rounded_md()
                .border_1()
                .border_color(t.divider)
                .bg(t.bg_light)
                .text_color(t.text)
                .cursor_pointer()
                .hover(|d| d.bg(t.bg_mid))
                .tooltip(ui::tip("Show every project"))
                .child(div().min_w_0().truncate().child(SharedString::from(name)))
                .child(
                    svg()
                        .flex_none()
                        .path("icons/x.svg")
                        .size(px(11.0))
                        .text_color(t.secondary_text),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        this.agents.gh_scope = None;
                        cx.notify();
                    }),
                )
        });
        // One row: the project it's narrowed to, whose (dropdown) + a GitHub search box.
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .px_2()
            .pb_2()
            .flex_none()
            .text_size(px(t.ui_font_size - 1.0))
            .children(scope)
            .child(div().flex_none().child(filter))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(t.divider)
                    .bg(t.main_bg)
                    .overflow_hidden()
                    .child(self.agents.gh_query.clone()),
            )
            .into_any_element()
    }

    /// Reload every project's list for `tab` (the header's refresh button) — `quiet`ly (the
    /// current list stays up until the new one lands) for the periodic refresh.
    fn agents_refresh_gh(&mut self, tab: SideTab, quiet: bool, cx: &mut Context<Self>) {
        self.agents.gh_fetched.insert(tab, Instant::now());
        if tab == SideTab::Ci {
            self.agents_reload_run(!quiet, cx);
        }
        let all: Vec<PathBuf> = self
            .agents
            .cfg
            .spaces
            .iter()
            .map(|s| s.path.clone())
            .collect();
        for space in all {
            self.agents_fetch_gh(space, tab, quiet, cx);
        }
    }

    /// (Re)load `space`'s assigned issues or open PRs with `gh`, in the background. `quiet`:
    /// keep showing the current list meanwhile, and keep it if the fetch fails.
    fn agents_fetch_gh(
        &mut self,
        space: PathBuf,
        tab: SideTab,
        quiet: bool,
        cx: &mut Context<Self>,
    ) {
        let reason = |e: kyde_git::GitError| crate::agents::git_reason(&e.to_string()).to_string();
        match tab {
            SideTab::Code => {}
            SideTab::Ci => self.agents_fetch_runs(space, quiet, cx),
            SideTab::Issues => {
                let (filter, query) = (self.agents.issue_filter, self.agents_gh_query(cx));
                let shown = matches!(self.agents.issues.get(&space), Some(GhList::Ready(_)));
                if !(quiet && shown) {
                    self.agents.issues.insert(space.clone(), GhList::Loading);
                }
                cx.spawn(async move |this, cx| {
                    let dir = space.clone();
                    let r = cx
                        .background_executor()
                        .spawn(async move { kyde_git::github::issues(&dir, filter, &query) })
                        .await;
                    this.update(cx, |this, cx| {
                        if quiet && r.is_err() && shown {
                            return;
                        }
                        let list = r.map_or_else(|e| GhList::Failed(reason(e)), GhList::Ready);
                        this.agents.issues.insert(space, list);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            SideTab::Prs => {
                let (filter, query) = (self.agents.pr_filter, self.agents_gh_query(cx));
                let shown = matches!(self.agents.prs.get(&space), Some(GhList::Ready(_)));
                if !(quiet && shown) {
                    self.agents.prs.insert(space.clone(), GhList::Loading);
                }
                cx.spawn(async move |this, cx| {
                    let dir = space.clone();
                    let r = cx
                        .background_executor()
                        .spawn(async move { kyde_git::github::prs(&dir, filter, &query) })
                        .await;
                    this.update(cx, |this, cx| {
                        if quiet && r.is_err() && shown {
                            return;
                        }
                        let list = r.map_or_else(|e| GhList::Failed(reason(e)), GhList::Ready);
                        this.agents.prs.insert(space, list);
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// The worktree of `space` (incl. its own folder) whose branch or folder matches.
    fn agents_find_worktree(
        &self,
        space: &Path,
        hit: impl Fn(&str, &str) -> bool,
    ) -> Option<PathBuf> {
        let s = self.agents.cfg.spaces.iter().find(|s| s.path == space)?;
        std::iter::once(&s.path)
            .chain(&s.worktrees)
            .find(|w| {
                let branch = self.agents.branches.get(*w).map_or("", String::as_str);
                hit(branch, &leaf_name(w))
            })
            .cloned()
    }

    /// The worktree already working on issue `n` (branch `…/<n>-…` or folder `<n>-…`).
    fn agents_issue_worktree(&self, space: &Path, n: u64) -> Option<PathBuf> {
        let tag = format!("{n}-");
        self.agents_find_worktree(space, |branch, folder| {
            folder.starts_with(&tag)
                || branch
                    .rsplit('/')
                    .next()
                    .is_some_and(|tail| tail.starts_with(&tag))
        })
    }

    /// The worktree already on PR `pr`'s branch (or the `pr-<n>` folder).
    fn agents_pr_worktree(
        &self,
        space: &Path,
        pr: &kyde_git::github::PullRequest,
    ) -> Option<PathBuf> {
        let folder = format!("pr-{}", pr.number);
        self.agents_find_worktree(space, |branch, f| branch == pr.head || f == folder)
    }

    /// One click on an issue: open its worktree if there is one, else make it (an
    /// `is`-skill-style `<type>/<n>-<slug>` branch off the latest main) and start the agent on
    /// the issue.
    fn agents_start_issue(
        &mut self,
        space: PathBuf,
        issue: kyde_git::github::Issue,
        cx: &mut Context<Self>,
    ) {
        if let Some(wt) = self.agents_issue_worktree(&space, issue.number) {
            self.agents.side_tab = SideTab::Code;
            self.agents_select(wt, cx);
            return;
        }
        let (folder, branch) =
            crate::agents::issue_branch(issue.number, &issue.title, &issue.labels);
        let n = issue.number.to_string();
        let base = self.agents_base_name(&space);
        let prompt = kyde_config::agents::fill(
            self.agents.cfg.issue_prompt(),
            &[
                ("number", &n),
                ("title", &issue.title),
                ("url", &issue.url),
                ("branch", &branch),
                ("base", &base),
                ("author", ""),
            ],
        );
        let start = WtStart {
            prompt,
            tab: clip(&format!("#{n} {}", issue.title), 36),
            origin: format!("#{n} {}", issue.title),
            doing: format!("Setting up #{n}: {}", issue.title),
        };
        self.agents_create_worktree(space, folder, WtSource::NewBranch(branch), Some(start), cx);
    }

    /// One click on a PR: open its worktree if there is one, else check it out into a
    /// `pr-<n>` worktree and start the agent on a review.
    fn agents_start_review(
        &mut self,
        space: PathBuf,
        pr: kyde_git::github::PullRequest,
        cx: &mut Context<Self>,
    ) {
        match self.agents_review_state(&space, &pr) {
            // An agent is on it: just go there.
            ReviewState::InProgress(wt) => {
                self.agents.side_tab = SideTab::Code;
                self.agents_select(wt, cx);
                return;
            }
            // Reviewed before: pull the PR's latest commits in, then a fresh pass.
            ReviewState::Done(wt) => {
                self.agents_rereview(wt, pr, cx);
                return;
            }
            ReviewState::None => {}
        }
        let base = self.agents_base_name(&space);
        let prompt = self.agents_review_prompt(self.agents.cfg.review_prompt(), &pr, &base);
        let n = pr.number;
        let start = WtStart {
            prompt,
            tab: format!("Review #{n}"),
            origin: format!("Review #{n}: {}", pr.title),
            doing: format!("Setting up a review of #{n}: {}", pr.title),
        };
        self.agents_create_worktree(space, format!("pr-{n}"), WtSource::Pr(n), Some(start), cx);
    }

    /// A review prompt template filled in for `pr`.
    fn agents_review_prompt(
        &self,
        template: &str,
        pr: &kyde_git::github::PullRequest,
        base: &str,
    ) -> String {
        kyde_config::agents::fill(
            template,
            &[
                ("number", &pr.number.to_string()),
                ("title", &pr.title),
                ("url", &pr.url),
                ("branch", &pr.head),
                ("base", base),
                ("author", &pr.author),
            ],
        )
    }

    /// The project's mainline branch name for prompts ("main"), from what git last read.
    fn agents_base_name(&self, space: &Path) -> String {
        match self.agents.main_sync.get(space) {
            Some(crate::agents::MainSync::Synced(r)) => {
                r.rsplit('/').next().unwrap_or("main").to_string()
            }
            _ => "main".to_string(),
        }
    }

    /// Where a PR's review stands: no worktree yet, an agent working in one, or one with no
    /// live agent (reviewed before).
    fn agents_review_state(&self, space: &Path, pr: &kyde_git::github::PullRequest) -> ReviewState {
        match self.agents_pr_worktree(space, pr) {
            None => ReviewState::None,
            Some(wt) if self.agents_has_tabs(&wt) => ReviewState::InProgress(wt),
            Some(wt) => ReviewState::Done(wt),
        }
    }

    /// Whether worktree `wt` has a live tab.
    fn agents_has_tabs(&self, wt: &Path) -> bool {
        #[cfg(feature = "terminal")]
        {
            self.agents.tabs.get(wt).is_some_and(|g| !g.tabs.is_empty())
        }
        #[cfg(not(feature = "terminal"))]
        {
            let _ = wt;
            false
        }
    }

    /// Re-review: bring the review worktree up to the PR's latest commits (`gh pr checkout`),
    /// then start a fresh agent on what changed, and go there.
    fn agents_rereview(
        &mut self,
        wt: PathBuf,
        pr: kyde_git::github::PullRequest,
        cx: &mut Context<Self>,
    ) {
        self.agents.side_tab = SideTab::Code;
        self.agents.note = Some(format!("Updating #{} for a re-review…", pr.number));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let dir = wt.clone();
            let n = pr.number;
            let res = cx
                .background_executor()
                .spawn(async move { kyde_git::github::pr_checkout(&dir, n) })
                .await;
            this.update(cx, |this, cx| {
                this.agents.note = res.err().map(|e| {
                    format!(
                        "Couldn't update #{n}: {} — re-reviewing what's there",
                        crate::agents::git_reason(&e.to_string())
                    )
                });
                let space = this
                    .agents
                    .cfg
                    .space_of(&wt)
                    .map_or_else(|| wt.clone(), |s| s.path.clone());
                let base = this.agents_base_name(&space);
                let template = this.agents.cfg.rereview_prompt().to_string();
                let prompt = this.agents_review_prompt(&template, &pr, &base);
                #[cfg(feature = "terminal")]
                this.agents_new_tab_as(
                    wt.clone(),
                    Some(prompt),
                    Some(format!("Re-review #{n}")),
                    cx,
                );
                #[cfg(not(feature = "terminal"))]
                let _ = prompt;
                this.agents_select(wt, cx);
                this.agents_refresh_branches(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The worktree row's ×: ask first, in a native window that says what would be lost.
    fn agents_confirm_remove(&mut self, space: PathBuf, wt: PathBuf, cx: &mut Context<Self>) {
        self.agents.removing = Some((space, wt));
        self.open_modal_window(
            ModalKind::RemoveWorktree,
            "Remove Worktree",
            480.0,
            250.0,
            cx,
        );
    }

    /// Body of the "Remove worktree?" window: the worktree, and exactly what removing it
    /// costs — uncommitted changes are lost (in red), commits stay on the kept branch.
    pub(crate) fn render_remove_worktree_body(
        &mut self,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let Some((space, wt)) = self.agents.removing.clone() else {
            return div().into_any_element();
        };
        let g = self.agents.git.get(&wt).copied().unwrap_or_default();
        let branch = self.agents.branches.get(&wt).cloned();
        let heading = format!("Remove “{}”?", worktree_title(Some(&space), &wt));
        let path = wt.display().to_string();
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let mut lines: Vec<(String, bool)> = Vec::new();
        if g.changed > 0 {
            lines.push((
                format!(
                    "{} uncommitted change{} will be deleted.",
                    g.changed,
                    plural(g.changed)
                ),
                true,
            ));
        }
        if g.ahead > 0 {
            lines.push((
                format!(
                    "{} commit{} not pushed yet — they stay on the branch.",
                    g.ahead,
                    plural(g.ahead)
                ),
                false,
            ));
        }
        if g.changed == 0 {
            lines.push(("Nothing uncommitted — no work is lost.".into(), false));
        }
        if let Some(b) = &branch {
            lines.push((format!("The branch {b} is kept."), false));
        }
        let cancel = btn_secondary("rm-wt-cancel", "Cancel").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| {
                this.agents.removing = None;
                this.close_modal_window(ModalKind::RemoveWorktree, cx);
            }),
        );
        let confirm = btn_primary(
            "rm-wt-confirm",
            if g.changed > 0 {
                "Remove & Discard Changes"
            } else {
                "Remove Worktree"
            },
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| this.agents_remove_confirmed(cx)),
        );
        ui::confirm_body(heading, path)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .pt_2()
                    .children(lines.into_iter().map(|(line, danger)| {
                        div()
                            .text_color(if danger {
                                t.status_conflict
                            } else {
                                t.secondary_text
                            })
                            .child(SharedString::from(line))
                    })),
            )
            .child(ui::modal_footer().child(cancel).child(confirm))
            .into_any_element()
    }

    /// The "Remove worktree?" window's confirm (its primary button, or Enter): close it and
    /// remove the worktree it asked about.
    pub(crate) fn agents_remove_confirmed(&mut self, cx: &mut Context<Self>) {
        self.close_modal_window(ModalKind::RemoveWorktree, cx);
        if let Some((space, wt)) = self.agents.removing.take() {
            self.agents_remove_worktree(space, wt, cx);
        }
    }

    /// Remove a linked worktree the user confirmed: `git worktree remove --force` (its
    /// uncommitted changes were shown in the confirmation), then close its tabs and drop it
    /// from the space. The branch is kept. A folder already gone from disk is just forgotten.
    fn agents_remove_worktree(&mut self, space: PathBuf, wt: PathBuf, cx: &mut Context<Self>) {
        self.agents_remove_in_background(vec![(space, wt)], cx);
    }

    /// Remove worktrees without making you wait: they leave the sidebar (and their tabs
    /// close) at once, while git deletes the checkouts in the background — slow on a big
    /// repo. Any that git refuses come back, with a note saying why.
    fn agents_remove_in_background(
        &mut self,
        list: Vec<(PathBuf, PathBuf)>,
        cx: &mut Context<Self>,
    ) {
        if list.is_empty() {
            return;
        }
        for (_, wt) in &list {
            self.agents.hiding.insert(wt.clone());
            self.agents_forget_worktree(wt, cx);
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    list.into_iter()
                        .map(|(space, wt)| {
                            let r = if wt.exists() {
                                Repo::discover(&space)
                                    .and_then(|r| r.remove_worktree_force(&wt))
                                    .map_err(|e| e.to_string())
                            } else {
                                Ok(())
                            };
                            (space, wt, r)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                let mut kept = Vec::new();
                for (space, wt, r) in results {
                    this.agents.hiding.remove(&wt);
                    if let Err(e) = r {
                        kept.push(format!(
                            "{} ({})",
                            worktree_title(Some(&space), &wt),
                            crate::agents::git_reason(&e)
                        ));
                    }
                }
                if !kept.is_empty() {
                    this.agents.note = Some(format!("Couldn't remove {}", kept.join(", ")));
                }
                // Re-list: anything git kept shows up again.
                this.agents_refresh_branches(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Drop a (removed) worktree from its space, closing its tabs. A scratch row's folder
    /// goes too, unless its agent left files there.
    fn agents_forget_worktree(&mut self, wt: &Path, cx: &mut Context<Self>) {
        let mut owner = None;
        for s in &mut self.agents.cfg.spaces {
            if let Some(i) = s.worktrees.iter().position(|w| w == wt) {
                s.worktrees.remove(i);
                owner = if s.scratch {
                    // Scratch's own folder isn't a row: its neighbour is shown instead.
                    s.worktrees.get(i.saturating_sub(1)).cloned()
                } else {
                    Some(s.path.clone())
                };
                if s.scratch {
                    let dir = wt.to_path_buf();
                    cx.background_executor()
                        .spawn(async move { crate::agents::remove_scratch_dir(&dir) })
                        .detach();
                }
            }
        }
        #[cfg(feature = "terminal")]
        {
            if let Some(g) = self.agents.tabs.remove(wt) {
                for t in &g.tabs {
                    t.view.read(cx).kill_host();
                }
            }
            // Its stopped background agents' tabs go too.
            let gone: Vec<String> = self.agents_done_in(wt).map(|d| d.sub.id.clone()).collect();
            self.agents_dismiss_subs(&gone, cx);
        }
        if self.agents.selected.as_deref() == Some(wt) {
            self.agents.selected.clone_from(&owner);
        }
        self.agents.cfg.pinned.remove(wt);
        // Its code goes too: open, its space's tab moves to the row taking its place.
        #[cfg(feature = "terminal")]
        if self.open_projects.iter().any(|p| p == wt) {
            let shown = self.repo_root.as_deref() == Some(wt);
            let agents = self.mode == Mode::Agents;
            self.close_project(wt.to_path_buf(), cx);
            if let Some(o) = owner.filter(|_| shown) {
                self.open_project(o, cx);
            }
            if agents {
                self.mode = Mode::Agents;
            }
        }
        self.agents_save(cx);
        cx.notify();
    }

    /// A scratch row's ×: close its tabs (asking first while an agent there isn't done) — the
    /// row (and its folder, if empty) goes with the last one.
    fn agents_close_scratch(&mut self, wt: &Path, cx: &mut Context<Self>) {
        #[cfg(feature = "terminal")]
        if let Some(g) = self.agents.tabs.get(wt).filter(|g| !g.tabs.is_empty()) {
            let ids = g.tabs.iter().map(|t| t.id.clone()).collect();
            self.agents_ask_close(ids, cx);
            return;
        }
        // Closing its last tab forgot it already; a row without tabs goes now.
        if self.agents.cfg.is_scratch(wt) {
            self.agents_forget_worktree(wt, cx);
        }
    }

    /// The window is Agents-only: the IDE chrome (rail, toolbars, status bar, bottom terminal,
    /// project landing) is hidden and the Agents view fills it. Terminal builds only.
    pub(crate) fn agents_only(&self) -> bool {
        cfg!(feature = "terminal") && self.agents.cfg.agents_only
    }

    /// Turn Agents-only mode on/off (sidebar button, Settings → Agents). Persisted.
    pub(crate) fn agents_set_only(&mut self, on: bool, cx: &mut Context<Self>) {
        self.agents.cfg.agents_only = on;
        if on {
            self.term.panel.maximized = false;
            self.enter_agents(cx);
        }
        self.agents_save(cx);
        cx.notify();
    }

    /// Pressed on a draggable row: a drag may start once the mouse moves.
    fn agents_drag_press(&mut self, item: DragItem, e: &gpui::MouseDownEvent) {
        if e.click_count == 1 {
            self.agents.drag = Some(SideDrag {
                item,
                from: e.position,
                active: false,
                over: None,
            });
        }
    }

    /// The mouse moved over the list: a pressed row becomes a drag past a few pixels; a
    /// released button (missed by the list) ends it.
    fn agents_drag_move(&mut self, e: &gpui::MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(d) = &mut self.agents.drag else {
            return;
        };
        if e.pressed_button != Some(MouseButton::Left) {
            self.agents.drag = None;
            cx.notify();
            return;
        }
        if !d.active && (e.position - d.from).magnitude() > 5.0 {
            d.active = true;
            cx.notify();
        }
    }

    /// The mouse is over a row of project `space` (worktree `wt`, or its header): that's
    /// where a dragged project — or one of this project's worktrees — would go. A dragged
    /// group would go where this project's group is.
    fn agents_drag_hover(&mut self, space: &Path, wt: Option<&Path>, cx: &mut Context<Self>) {
        let Some(item) = self
            .agents
            .drag
            .as_ref()
            .filter(|d| d.active)
            .map(|d| &d.item)
        else {
            return;
        };
        let scratch = self
            .agents
            .cfg
            .spaces
            .iter()
            .any(|s| s.scratch && s.path == space);
        let over = match item {
            // A project can't go below Scratch, which stays at the bottom.
            DragItem::Space(_) if scratch => None,
            DragItem::Space(_) => Some(DropOver::Path(space.to_path_buf())),
            DragItem::Group(_) => {
                let cfg = &self.agents.cfg;
                cfg.spaces
                    .iter()
                    .find(|s| s.path == space)
                    .and_then(|s| cfg.group_of(s))
                    .map(|g| DropOver::Group(g.id))
            }
            DragItem::Worktree { space: s, .. } if s == space => {
                Some(DropOver::Path(wt.unwrap_or(space).to_path_buf()))
            }
            DragItem::Worktree { .. } => None,
        };
        self.agents_drag_over(over, cx);
    }

    /// The mouse is over group `id`'s header: a dragged project would go into the group, a
    /// dragged group where it is.
    fn agents_drag_hover_group(&mut self, id: u32, cx: &mut Context<Self>) {
        let Some(item) = self
            .agents
            .drag
            .as_ref()
            .filter(|d| d.active)
            .map(|d| &d.item)
        else {
            return;
        };
        let over = match item {
            DragItem::Space(_) | DragItem::Group(_) => Some(DropOver::Group(id)),
            DragItem::Worktree { .. } => None,
        };
        self.agents_drag_over(over, cx);
    }

    /// Where the drag in progress would drop (repaints when that changes).
    fn agents_drag_over(&mut self, over: Option<DropOver>, cx: &mut Context<Self>) {
        if let Some(d) = self.agents.drag.as_mut() {
            if d.over != over {
                d.over = over;
                cx.notify();
            }
        }
    }

    /// Released: move the dragged row to where the line was.
    fn agents_drag_drop(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.agents.drag.take() else {
            return;
        };
        if let (true, Some(onto)) = (d.active, d.over) {
            let cfg = &mut self.agents.cfg;
            let moved = match (d.item, onto) {
                (DragItem::Space(p), DropOver::Path(onto)) => cfg.move_space(&p, &onto),
                (DragItem::Space(p), DropOver::Group(g)) => cfg.move_space_to_group(&p, Some(g)),
                (DragItem::Group(g), DropOver::Group(onto)) => cfg.move_group(g, onto),
                (DragItem::Worktree { space, wt }, DropOver::Path(onto)) => {
                    self.agents_move_worktree(&space, &wt, &onto)
                }
                _ => false,
            };
            if moved {
                self.agents_save(cx);
            }
        }
        cx.notify();
    }

    /// Where the drop line shows for the drag in progress (`None`: no move).
    fn agents_drop_line(&self) -> Option<DropLine> {
        let d = self.agents.drag.as_ref().filter(|d| d.active)?;
        let cfg = &self.agents.cfg;
        match (&d.item, d.over.as_ref()?) {
            (DragItem::Space(p), DropOver::Path(onto)) => {
                let order = cfg.space_order();
                let at = |q: &Path| order.iter().position(|&i| cfg.spaces[i].path == q);
                drop_side(
                    at(p)?,
                    at(onto)?,
                    DropLine::BeforeSpace(onto.clone()),
                    DropLine::AfterSpace(onto.clone()),
                )
            }
            (DragItem::Space(_), DropOver::Group(g)) => Some(DropLine::IntoGroup(*g)),
            (DragItem::Group(g), DropOver::Group(onto)) => {
                let at = |id: u32| cfg.groups.iter().position(|x| x.id == id);
                drop_side(
                    at(*g)?,
                    at(*onto)?,
                    DropLine::BeforeGroup(*onto),
                    DropLine::AfterGroup(*onto),
                )
            }
            (DragItem::Worktree { space, wt }, DropOver::Path(onto)) => {
                let rows = cfg.space_of(space)?.rows();
                let at = |q: &Path| rows.iter().position(|w| w == q);
                drop_side(
                    at(wt)?,
                    at(onto)?,
                    DropLine::BeforeRow(onto.clone()),
                    DropLine::AfterRow(onto.clone()),
                )
            }
            _ => None,
        }
    }

    /// Move linked worktree `wt` of project `space` to where `onto` is — the project's own
    /// checkout (always first) or its header meaning the top. `false`: nothing moved.
    fn agents_move_worktree(&mut self, space: &Path, wt: &Path, onto: &Path) -> bool {
        let Some(s) = self.agents.cfg.spaces.iter_mut().find(|s| s.path == space) else {
            return false;
        };
        let mut rows = s.rows();
        let (Some(f), Some(t)) = (
            rows.iter().position(|w| w == wt),
            rows.iter().position(|w| w == onto),
        ) else {
            return false;
        };
        if f == t {
            return false;
        }
        let w = rows.remove(f);
        rows.insert(t, w);
        s.set_rows(rows);
        true
    }

    /// Fold / unfold one space's worktrees in the sidebar.
    fn agents_toggle_space(&mut self, space: &Path, cx: &mut Context<Self>) {
        if let Some(s) = self.agents.cfg.spaces.iter_mut().find(|s| s.path == space) {
            s.collapsed = !s.collapsed;
            self.agents_save(cx);
            cx.notify();
        }
    }

    /// Fold / unfold a group's projects in the sidebar.
    fn agents_toggle_group(&mut self, id: u32, cx: &mut Context<Self>) {
        if let Some(g) = self.agents.cfg.groups.iter_mut().find(|g| g.id == id) {
            g.collapsed = !g.collapsed;
            self.agents_save(cx);
            cx.notify();
        }
    }

    /// Expand all / Collapse all (the sidebar header buttons) — groups and projects.
    fn agents_set_all_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        for s in &mut self.agents.cfg.spaces {
            s.collapsed = collapsed;
        }
        for g in &mut self.agents.cfg.groups {
            g.collapsed = collapsed;
        }
        self.agents_save(cx);
        cx.notify();
    }

    /// Unfold the space that owns `wt` (and its group), so a worktree being opened/created is
    /// visible.
    fn agents_reveal(&mut self, wt: &Path) {
        let cfg = &mut self.agents.cfg;
        let Some(s) = cfg
            .spaces
            .iter_mut()
            .find(|s| s.path == wt || s.worktrees.iter().any(|w| w == wt))
        else {
            return;
        };
        s.collapsed = false;
        if let Some(id) = s.group {
            for g in cfg.groups.iter_mut().filter(|g| g.id == id) {
                g.collapsed = false;
            }
        }
    }

    /// A new group, renamed in place at once; `with` (a project) moves into it.
    pub(crate) fn agents_new_group(
        &mut self,
        with: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.agents.cfg.add_group("New group");
        if let Some(p) = with {
            self.agents.cfg.move_space_to_group(&p, Some(id));
        }
        self.agents_save(cx);
        self.agents_start_rename(RenameTarget::Group(id), window, cx);
    }

    /// Remove group `id` from the sidebar — its projects stay, ungrouped.
    fn agents_remove_group(&mut self, id: u32, cx: &mut Context<Self>) {
        self.agents.cfg.remove_group(id);
        self.agents_save(cx);
        cx.notify();
    }

    /// Move project `space` into group `group` (`None`: out of its group).
    fn agents_move_to_group(&mut self, space: &Path, group: Option<u32>, cx: &mut Context<Self>) {
        if self.agents.cfg.move_space_to_group(space, group) {
            self.agents_save(cx);
            cx.notify();
        }
    }

    /// A group header's "+": pick a folder and add it as a project in that group (one that's
    /// a project already moves there).
    fn agents_add_space_to_group_prompt(&mut self, id: u32, cx: &mut Context<Self>) {
        self.prompt_folder("Add Project", cx, move |this, path, cx| {
            this.agents_ensure_space(&path, cx);
            if this.agents.cfg.move_space_to_group(&path, Some(id)) {
                this.agents_save(cx);
            }
            this.agents_select(path, cx);
        });
    }

    /// When worktree `wt`'s agents last did anything (output, or being looked at).
    fn agents_last_active(&self, wt: &Path) -> Option<Instant> {
        #[cfg(feature = "terminal")]
        {
            self.agents
                .tabs
                .get(wt)
                .and_then(|g| g.tabs.iter().map(|t| t.last_active).max())
        }
        #[cfg(not(feature = "terminal"))]
        {
            let _ = wt;
            None
        }
    }

    /// Rolled-up status of worktree `wt`'s tabs — and of any background agent at work in it
    /// (started from a tab anywhere).
    pub(crate) fn agents_state(&self, wt: &Path) -> AgentState {
        #[cfg(feature = "terminal")]
        {
            let tabs = self.agents.tabs.get(wt).into_iter().flat_map(|g| &g.tabs);
            let busy = self.agents_subs_in(wt).next().map(|_| AgentState::Working);
            AgentState::rollup(tabs.map(|t| t.shown).chain(busy))
        }
        #[cfg(not(feature = "terminal"))]
        {
            let _ = wt;
            AgentState::None
        }
    }
}

// ── live tabs (PTY-backed — terminal feature) ────────────────────────────────────────────
#[cfg(feature = "terminal")]
impl Kyde {
    /// The background agents at work in worktree `wt`, each with the worktree and tab that
    /// started it.
    pub(crate) fn agents_subs_in<'a>(
        &'a self,
        wt: &'a Path,
    ) -> impl Iterator<Item = (&'a Path, &'a AgentTab, &'a crate::agents::SubAgent)> + 'a {
        self.agents.tabs.iter().flat_map(move |(home, g)| {
            g.tabs.iter().flat_map(move |t| {
                t.subs
                    .iter()
                    .filter(move |s| s.worktree.as_deref() == Some(wt))
                    .map(move |s| (home.as_path(), t, s))
            })
        })
    }

    /// The stopped background agents whose work in worktree `wt` is kept until dismissed.
    pub(crate) fn agents_done_in<'a>(
        &'a self,
        wt: &'a Path,
    ) -> impl Iterator<Item = &'a crate::agents::DoneSub> + 'a {
        self.agents
            .done_subs
            .iter()
            .filter(move |d| d.sub.worktree.as_deref() == Some(wt))
    }

    /// Show tab `id` of worktree `wt` (the tab a background agent came from).
    fn agents_go_to_tab(&mut self, wt: PathBuf, id: &str, cx: &mut Context<Self>) {
        self.agents.sub_shown = None;
        if let Some(g) = self.agents.tabs.get_mut(&wt) {
            if let Some(i) = g.tabs.iter().position(|t| t.id == id) {
                g.active = i;
            }
        }
        self.agents_select(wt, cx);
    }

    /// Bring a worktree's tabs up as it's opened: the tabs it had at the last quit (agent tabs
    /// resume their Claude Code sessions), else one fresh agent tab. No-op if it has tabs.
    fn agents_open_tabs(&mut self, wt: PathBuf, cx: &mut Context<Self>) {
        if self
            .agents
            .tabs
            .get(&wt)
            .is_some_and(|g| !g.tabs.is_empty())
        {
            return;
        }
        // A project's own checkout first catches up with the latest main.
        self.agents_after_main_sync(wt.clone(), cx, move |this, cx| {
            this.agents_open_tabs_now(wt, cx);
        });
    }

    /// Run `then` once worktree `wt` is up to date with its remote mainline — straight away
    /// for a linked worktree; for a project's own checkout (the `main` rows), after a
    /// background fetch + fast-forward (`agents::sync_main`: only on the default branch with
    /// no uncommitted changes, never a merge).
    fn agents_after_main_sync(
        &mut self,
        wt: PathBuf,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        if !self.agents.cfg.spaces.iter().any(|s| s.path == wt) {
            then(self, cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            let dir = wt.clone();
            let res = cx
                .background_executor()
                .spawn(async move { crate::agents::sync_main(&dir) })
                .await;
            this.update(cx, |this, cx| {
                if let Some(r) = res {
                    this.agents.main_sync.insert(wt, r);
                }
                then(this, cx);
                this.agents_refresh_branches(cx);
            })
            .ok();
        })
        .detach();
    }

    /// `agents_open_tabs` once main is synced.
    fn agents_open_tabs_now(&mut self, wt: PathBuf, cx: &mut Context<Self>) {
        if self
            .agents
            .tabs
            .get(&wt)
            .is_some_and(|g| !g.tabs.is_empty())
        {
            return;
        }
        let saved = (!self.agents.restored.contains(&wt))
            .then(|| self.agents.cfg.tabs.get(&wt).cloned())
            .flatten()
            .filter(|v| !v.is_empty());
        match saved {
            Some(saved) => {
                for t in saved {
                    match (t.agent, t.session) {
                        (true, Some(id)) => self.agents_resume_tab(wt.clone(), id, cx),
                        (agent, _) => self.agents_new_tab(wt.clone(), agent, None, cx),
                    }
                }
            }
            // A background agent is at work here: no agent of its own beside it (two on the
            // same files) — the empty state says whose it is.
            None if self.agents_subs_in(&wt).next().is_some() => {}
            // Its latest Claude Code session, resumed — else, in a project's own checkout (or a
            // scratch row), a fresh agent. A linked worktree with neither waits for you to
            // start one: one an agent made (`git worktree add`) gets that agent's tab once it
            // edits there, and a blank agent beside it would read as the one at work.
            None => match self
                .agents
                .contexts
                .get(&wt)
                .and_then(|c| c.session.clone())
            {
                Some(id) => self.agents_resume_tab(wt, id, cx),
                None if self.agents.cfg.spaces.iter().any(|s| s.path == wt)
                    || self.agents.cfg.is_scratch(&wt) =>
                {
                    self.agents_new_tab(wt, true, None, cx);
                }
                None => {}
            },
        }
    }

    /// Open a tab in worktree `wt`. `agent` = type the start command into the fresh shell
    /// (herdr-style, so quitting the agent leaves you at a prompt in the worktree), seeded with
    /// a first `prompt` when given (an issue to work on, a PR to review); else a plain shell.
    pub(crate) fn agents_new_tab(
        &mut self,
        wt: PathBuf,
        agent: bool,
        prompt: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let line = agent.then(|| {
            let cmd = self.agents.cfg.default_command();
            match &prompt {
                Some(p) => crate::agents::seeded_line(cmd, p),
                None => cmd.to_string(),
            }
        });
        // A tab started on a task (an issue, a review) is in use from the start.
        let seeded = agent && prompt.is_some();
        // Every agent tab hears how to move to another worktree with its first prompt; a
        // blank one also hears what's already going on here.
        let brief = agent
            .then(|| self.agents_task_brief(&wt, prompt.is_none(), cx))
            .flatten();
        self.agents_spawn_tab(wt, agent, line, None, seeded, brief, cx);
    }

    /// Kyde's note for a new agent tab in `wt`: how to move to another worktree, plus the
    /// work already there when `blank` (a seeded tab's worktree was made for its task) — see
    /// `agents::task_brief`. `None` without the hook.
    fn agents_task_brief(&self, wt: &Path, blank: bool, cx: &gpui::App) -> Option<String> {
        // The hook is what hands it over; a scratch folder has no worktrees to move to.
        if !self.agents.cfg.claude_hooks || self.agents.cfg.is_scratch(wt) {
            return None;
        }
        let mode = self.agents.cfg.new_task;
        if !blank {
            return Some(crate::agents::task_brief(
                &crate::agents::Busy::default(),
                mode,
            ));
        }
        let git = self.agents.git.get(wt).copied().unwrap_or_default();
        let busy = crate::agents::Busy {
            branch: self.agents.branches.get(wt).cloned(),
            changed: git.changed,
            ahead: git.ahead,
            off_main: matches!(
                self.agents.main_sync.get(wt),
                Some(crate::agents::MainSync::OffBranch(_))
            ),
            others: self
                .agents
                .tabs
                .get(wt)
                .into_iter()
                .flat_map(|g| g.tabs.iter().filter(|t| t.agent && t.used))
                .map(|t| t.name.clone().unwrap_or_else(|| tab_auto_title(t, cx)))
                .chain(self.agents_subs_in(wt).map(|(_, t, s)| {
                    let from = t.name.clone().unwrap_or_else(|| tab_auto_title(t, cx));
                    format!("{} (a background agent of tab {from})", s.what)
                }))
                .collect(),
            last_task: self
                .agents
                .contexts
                .get(wt)
                .and_then(|c| c.title.clone().or_else(|| c.last_prompt.clone())),
        };
        Some(crate::agents::task_brief(&busy, mode))
    }

    /// A new agent tab started on `prompt`, named `name` in the tab strip.
    pub(crate) fn agents_new_tab_as(
        &mut self,
        wt: PathBuf,
        prompt: Option<String>,
        name: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.agents_new_tab(wt.clone(), true, prompt, cx);
        if let Some(t) = self
            .agents
            .tabs
            .get_mut(&wt)
            .and_then(|g| g.tabs.last_mut())
        {
            t.name = name;
            self.agents_save(cx);
        }
    }

    /// Reopen an agent tab on its Claude Code session (`<start command> --resume <id>`).
    fn agents_resume_tab(&mut self, wt: PathBuf, session: String, cx: &mut Context<Self>) {
        let line = format!(
            "{} --resume {}",
            self.agents.cfg.default_command(),
            crate::agents::shell_quote(&session)
        );
        self.agents_spawn_tab(wt, true, Some(line), Some(session), false, None, cx);
    }

    /// Spawn a tab's PTY in `wt`, typing `line` into its shell when given, with `brief` for
    /// its agent's first prompt.
    #[allow(clippy::too_many_arguments)]
    fn agents_spawn_tab(
        &mut self,
        wt: PathBuf,
        agent: bool,
        line: Option<String>,
        session: Option<String>,
        seeded: bool,
        brief: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let id = crate::agents::tab_id();
        // The Claude Code hook (see `kyde_config::claude_hooks`) reports this tab's state into
        // its state file; agents can find themselves by id.
        let _ = std::fs::create_dir_all(kyde_config::agents::agents_dir());
        if let Some(text) = brief {
            // Long before the first prompt is typed. If the agent moves out, Claude Code's
            // worktree mustn't make this checkout read as dirty.
            let (path, dir) = (crate::agents::brief_path(&id), wt.clone());
            cx.background_executor()
                .spawn(async move {
                    let _ = std::fs::write(path, text);
                    crate::agents::exclude_claude_worktrees(&dir);
                })
                .detach();
        }
        let env = crate::agents::tab_env(&id);
        // In an agent host, so it keeps running when Kyde quits — else (no host: Windows, a
        // failed start, tests) in a terminal of its own.
        let view = if let Some(v) =
            self.agents_spawn_host(&wt, &id, agent, line.clone(), env.clone(), cx)
        {
            v
        } else {
            let view = cx.new(|cx| {
                terminal::TerminalView::with_env(Some(wt.clone()), env.into_iter().collect(), cx)
            });
            if let Some(line) = line {
                view.read(cx).send_input(&format!("{line}\n"));
            }
            view
        };
        self.agents_add_tab(wt, view, id, agent, session, seeded, None, cx);
    }

    /// Start an agent host for a new tab and attach to it (`None` = no host here).
    #[allow(clippy::unnecessary_wraps)]
    fn agents_spawn_host(
        &mut self,
        wt: &Path,
        id: &str,
        agent: bool,
        line: Option<String>,
        env: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) -> Option<Entity<terminal::TerminalView>> {
        #[cfg(unix)]
        {
            if !crate::agents::hosted() {
                return None;
            }
            let spec = crate::agent_host::HostSpec {
                id: id.to_string(),
                cwd: wt.to_path_buf(),
                shell: terminal::default_shell(),
                env,
                line,
                cols: 120,
                rows: 32,
                agent,
            };
            let conn = crate::agent_host::spawn(&spec)
                .and_then(|()| terminal::TerminalView::connect_host(id));
            match conn {
                Ok(conn) => Some(cx.new(|cx| terminal::TerminalView::attach(conn, cx))),
                Err(e) => {
                    self.agents.note = Some(format!(
                        "Couldn't start the agent host ({e}) — this tab ends when Kyde quits"
                    ));
                    None
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (wt, id, agent, line, env, cx);
            None
        }
    }

    /// Reopen a tab on agent host `id`, still running from before (Kyde quit, or the CLI
    /// started it). Returns whether the host answered.
    #[cfg(unix)]
    fn agents_attach_tab(
        &mut self,
        wt: PathBuf,
        saved: &kyde_config::agents::SavedTab,
        id: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let Ok(conn) = terminal::TerminalView::connect_host(id) else {
            return false;
        };
        let view = cx.new(|cx| terminal::TerminalView::attach(conn, cx));
        self.agents_add_tab(
            wt,
            view,
            id.to_string(),
            saved.agent,
            saved.session.clone(),
            false,
            saved.name.clone(),
            cx,
        );
        // It's been at work: not "never used" (its replayed output isn't news, though).
        if let Some(t) = self
            .agents
            .tabs
            .values_mut()
            .flat_map(|g| g.tabs.iter_mut())
            .find(|t| t.id == id)
        {
            t.used = true;
        }
        true
    }

    /// Put a tab's terminal `view` into worktree `wt`'s strip and start watching it.
    #[allow(clippy::too_many_arguments)]
    fn agents_add_tab(
        &mut self,
        wt: PathBuf,
        view: Entity<terminal::TerminalView>,
        id: String,
        agent: bool,
        session: Option<String>,
        seeded: bool,
        name: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let spawned_at = std::time::SystemTime::now();
        // The agent's input box sits at the bottom of the pane, herdr-style.
        view.update(cx, |v, _| v.anchor_bottom = true);
        // Looked up per event: the tab moves when its agent moves to another worktree.
        cx.subscribe(&view, move |this, v, ev, cx| {
            let Some(key) = this.agents_wt_of(&v) else {
                return;
            };
            match ev {
                terminal::TerminalEvent::Output => this.agents_on_output(&key, &v, cx),
                terminal::TerminalEvent::TitleChanged => cx.notify(),
                terminal::TerminalEvent::Submitted => this.agents_on_submit(&key, &v, cx),
                terminal::TerminalEvent::OpenScrollback => this.agents_open_scrollback(&v, cx),
                terminal::TerminalEvent::OpenFile { path, line } => {
                    this.open_terminal_file(path.clone(), *line, cx);
                }
                terminal::TerminalEvent::CloseRequested => {
                    let Some(i) = this
                        .agents
                        .tabs
                        .get(&key)
                        .and_then(|g| g.tabs.iter().position(|t| t.view == v))
                    else {
                        return;
                    };
                    // Its shell exited: nothing left to lose. Its own menu's Close asks.
                    if v.read(cx).exited {
                        this.agents_close_tab(&key, i, cx);
                    } else {
                        let id = this.agents.tabs[&key].tabs[i].id.clone();
                        this.agents_ask_close(vec![id], cx);
                    }
                }
            }
        })
        .detach();
        self.agents.restored.insert(wt.clone());
        let group = self.agents.tabs.entry(wt).or_insert_with(|| AgentTabs {
            tabs: Vec::new(),
            active: 0,
        });
        group.tabs.push(AgentTab {
            view,
            id,
            name,
            hook: None,
            hook_seen: None,
            wrote: None,
            subs: Vec::new(),
            subs_seen: None,
            followed: std::collections::HashSet::new(),
            held: None,
            last_output: None,
            last_active: Instant::now(),
            unseen: false,
            agent,
            shown: AgentState::Idle,
            prompted: seeded,
            used: seeded || session.is_some(),
            spawned_at,
            session,
            verdict: None,
            ai: None,
        });
        group.active = group.tabs.len() - 1;
        self.agents.focus_pending = true;
        self.agents_ensure_ticker(cx);
        self.agents_save(cx);
        cx.notify();
    }

    /// Close tab `idx` of worktree `wt` (dropping the view shuts its PTY down).
    pub(crate) fn agents_close_tab(&mut self, wt: &Path, idx: usize, cx: &mut Context<Self>) {
        let Some(g) = self.agents.tabs.get_mut(wt) else {
            return;
        };
        let len = g.tabs.len();
        if idx >= len {
            return;
        }
        let gone = g.tabs.remove(idx);
        // Closing the tab ends its agent (quitting Kyde doesn't) — and its background agents,
        // whose work stays where they worked until dismissed.
        gone.view.read(cx).kill_host();
        let from = gone
            .name
            .clone()
            .unwrap_or_else(|| tab_auto_title(&gone, cx));
        let stopped: Vec<_> = gone
            .subs
            .iter()
            .map(|sub| crate::agents::DoneSub {
                sub: sub.clone(),
                tab: gone.id.clone(),
                home: wt.to_path_buf(),
                from: from.clone(),
                finished: false,
            })
            .collect();
        self.agents.closed_hosts.insert(gone.id.clone());
        let files = [
            crate::agents::state_path(&gone.id),
            crate::agents::brief_path(&gone.id),
            crate::agents::write_path(&gone.id),
        ];
        let subs = crate::agents::sub_dir(&gone.id);
        cx.background_executor()
            .spawn(async move {
                for f in files {
                    let _ = std::fs::remove_file(f);
                }
                let _ = std::fs::remove_dir_all(subs);
            })
            .detach();
        if let Some(a) = crate::term_panel::active_after_close(len, idx, g.active) {
            g.active = a;
        } else {
            self.agents.tabs.remove(wt);
            // A scratch row lives as long as its tabs.
            if self.agents.cfg.is_scratch(wt) {
                self.agents_forget_worktree(wt, cx);
            }
        }
        self.agents_keep_done(stopped, &[], cx);
        self.agents.focus_pending = true;
        self.agents_save(cx);
        cx.notify();
    }

    /// Close the tabs `ids` — asking first, in a native window, when any of them isn't seen as
    /// done (`tab_unfinished`). The tab ×, ⌘W, the tab menu and a scratch row's × all come
    /// through here; the cleanup doesn't (it only closes finished tabs).
    pub(crate) fn agents_ask_close(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let now = Instant::now();
        let unfinished = self
            .agents
            .tabs
            .values()
            .flat_map(|g| &g.tabs)
            .any(|t| ids.contains(&t.id) && tab_unfinished(t, now, cx));
        if !unfinished {
            self.agents_close_ids(&ids, cx);
            return;
        }
        let title = if ids.len() == 1 {
            "Close Tab"
        } else {
            "Close Tabs"
        };
        self.agents.closing = ids;
        self.open_modal_window(ModalKind::CloseTabs, title, 460.0, 220.0, cx);
    }

    /// Close the tabs `ids`, wherever each is now (a tab follows its agent between worktrees).
    fn agents_close_ids(&mut self, ids: &[String], cx: &mut Context<Self>) {
        for id in ids {
            let at = self.agents.tabs.iter().find_map(|(wt, g)| {
                let i = g.tabs.iter().position(|t| &t.id == id)?;
                Some((wt.clone(), i))
            });
            if let Some((wt, i)) = at {
                self.agents_close_tab(&wt, i, cx);
            }
        }
    }

    /// The "Close tab?" window's confirm (its primary button, or Enter): close it and the
    /// tabs it asked about.
    pub(crate) fn agents_close_confirmed(&mut self, cx: &mut Context<Self>) {
        self.close_modal_window(ModalKind::CloseTabs, cx);
        let ids = std::mem::take(&mut self.agents.closing);
        self.agents_close_ids(&ids, cx);
    }

    /// Body of the "Close tab?" window: the tabs it would close, and which of them aren't
    /// done — closing a tab ends its agent.
    pub(crate) fn render_close_tabs_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let now = Instant::now();
        let closing = &self.agents.closing;
        let tabs: Vec<&AgentTab> = self
            .agents
            .tabs
            .values()
            .flat_map(|g| &g.tabs)
            .filter(|t| closing.contains(&t.id))
            .collect();
        let name = |t: &AgentTab| t.name.clone().unwrap_or_else(|| tab_auto_title(t, cx));
        let busy: Vec<String> = tabs
            .iter()
            .filter(|t| tab_unfinished(t, now, cx))
            .map(|t| {
                let state = AgentState::of_tab(&tab_signals(t, cx), now);
                let what = match state {
                    AgentState::Blocked => "needs input",
                    AgentState::Working => "working",
                    _ => "stopped mid-task",
                };
                format!("{} — {what}", name(t))
            })
            .collect();
        let (heading, about, confirm) = match tabs.as_slice() {
            [] => return div().into_any_element(),
            [one] => (
                format!("Close “{}”?", name(one)),
                "Its agent isn't done. Closing the tab ends it.".to_string(),
                "Close Tab".to_string(),
            ),
            many => (
                format!("Close {} tabs?", many.len()),
                "Closing a tab ends its agent — and these aren't done:".to_string(),
                format!("Close {} Tabs", many.len()),
            ),
        };
        let cancel = btn_secondary("close-tabs-cancel", "Cancel").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| {
                this.agents.closing.clear();
                this.close_modal_window(ModalKind::CloseTabs, cx);
            }),
        );
        let confirm = btn_primary("close-tabs-confirm", confirm).on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| this.agents_close_confirmed(cx)),
        );
        ui::confirm_body(heading, about)
            .when(tabs.len() > 1, |d| {
                d.child(
                    div()
                        .id("close-tabs-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_color(t.secondary_text)
                        .children(
                            busy.into_iter()
                                .map(|line| div().truncate().child(SharedString::from(line))),
                        ),
                )
            })
            .child(ui::modal_footer().child(cancel).child(confirm))
            .into_any_element()
    }

    /// A tab's right-click menu: close it, the others, or the ones to its right (each asks
    /// first while a tab it would close isn't done), rename it, pin its worktree.
    pub(crate) fn agents_tab_menu(&self, wt: &Path, id: &str) -> Vec<MenuEntry> {
        let Some(g) = self.agents.tabs.get(wt) else {
            return Vec::new();
        };
        let Some(at) = g.tabs.iter().position(|t| t.id == id) else {
            return Vec::new();
        };
        let ids = |keep: &dyn Fn(usize) -> bool| -> Vec<String> {
            g.tabs
                .iter()
                .enumerate()
                .filter(|(i, _)| keep(*i))
                .map(|(_, t)| t.id.clone())
                .collect()
        };
        let (others, right) = (ids(&|i| i != at), ids(&|i| i > at));
        let (one, rename) = (id.to_string(), id.to_string());
        let mut out = vec![MenuEntry::item("Close", move |this, _w, cx| {
            this.agents_ask_close(vec![one.clone()], cx);
        })];
        if !others.is_empty() {
            out.push(MenuEntry::item("Close Others", move |this, _w, cx| {
                this.agents_ask_close(others.clone(), cx);
            }));
        }
        if !right.is_empty() {
            out.push(MenuEntry::item(
                "Close Tabs to the Right",
                move |this, _w, cx| this.agents_ask_close(right.clone(), cx),
            ));
        }
        out.push(MenuEntry::Divider);
        let wt = wt.to_path_buf();
        out.push(MenuEntry::item("Rename…", {
            let wt = wt.clone();
            move |this, window, cx| {
                let target = RenameTarget::Tab(wt.clone(), rename.clone());
                this.agents_start_rename(target, window, cx);
            }
        }));
        out.push(self.agents_pin_entry(&wt, true));
        out
    }

    /// Whether stopped background agent `d`'s work sits in a pinned worktree (the cleanup
    /// leaves it there).
    fn agents_sub_pinned(&self, d: &crate::agents::DoneSub) -> bool {
        let at = d.sub.worktree.as_ref().unwrap_or(&d.home);
        self.agents.cfg.pinned.contains(at)
    }

    /// Keep the work of background agents that `stopped` until it's dismissed (a newer read of
    /// one replaces the older; one that did nothing has nothing to keep), and drop any that are
    /// `live` again; saves when that changed.
    fn agents_keep_done(
        &mut self,
        mut stopped: Vec<crate::agents::DoneSub>,
        live: &[String],
        cx: &mut Context<Self>,
    ) {
        stopped.retain(|s| !s.sub.steps.is_empty());
        let before = self.agents.done_subs.len();
        self.agents
            .done_subs
            .retain(|d| !live.contains(&d.sub.id) && !stopped.iter().any(|s| s.sub.id == d.sub.id));
        if stopped.is_empty() && self.agents.done_subs.len() == before {
            return;
        }
        self.agents.done_subs.extend(stopped);
        self.agents_save_done(cx);
        cx.notify();
    }

    /// Dismiss background agent `id`'s finished work.
    pub(crate) fn agents_dismiss_sub(&mut self, id: &str, cx: &mut Context<Self>) {
        self.agents_dismiss_subs(&[id.to_string()], cx);
    }

    /// Dismiss the finished work of background agents `ids`; returns how many went.
    fn agents_dismiss_subs(&mut self, ids: &[String], cx: &mut Context<Self>) -> usize {
        let before = self.agents.done_subs.len();
        self.agents.done_subs.retain(|d| !ids.contains(&d.sub.id));
        if self
            .agents
            .sub_shown
            .as_ref()
            .is_some_and(|s| ids.contains(s))
        {
            self.agents.sub_shown = None;
        }
        let gone = before - self.agents.done_subs.len();
        if gone > 0 {
            self.agents_save_done(cx);
            cx.notify();
        }
        gone
    }

    /// The stopped background agent on screen right now, if the Agents view is showing one —
    /// as `render_agents_tabs` picks it: the one clicked, else the first when no tab is there.
    fn agents_visible_sub(&self) -> Option<&str> {
        if self.mode != Mode::Agents || self.diff_view_open {
            return None;
        }
        let wt = self.agents.selected.as_deref()?;
        let has_tab = self.agents.tabs.get(wt).is_some_and(|g| !g.tabs.is_empty());
        let ids: Vec<&str> = self
            .agents_subs_in(wt)
            .map(|(.., s)| s)
            .chain(self.agents_done_in(wt).map(|d| &d.sub))
            .map(|s| s.id.as_str())
            .collect();
        let picked = self.agents.sub_shown.as_deref();
        ids.iter()
            .find(|id| Some(**id) == picked)
            .or_else(|| ids.first().filter(|_| !has_tab))
            .copied()
    }

    /// Save the stopped background agents' work (`agents::done_path`), off the UI thread.
    fn agents_save_done(&self, cx: &mut Context<Self>) {
        let Ok(json) = serde_json::to_string(&self.agents.done_subs) else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                let path = crate::agents::done_path();
                let tmp = path.with_extension("json.tmp");
                let _ = std::fs::create_dir_all(kyde_config::agents::agents_dir());
                if std::fs::write(&tmp, json).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            })
            .detach();
    }

    /// Bring back the stopped background agents' work saved at the last quit, off the UI
    /// thread (any already found this launch stay). Ones that did nothing — Claude Code's own
    /// helpers, which older builds kept — are dropped, and the file rewritten without them.
    fn agents_load_done(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let saved: Vec<crate::agents::DoneSub> = cx
                .background_executor()
                .spawn(async {
                    std::fs::read_to_string(crate::agents::done_path())
                        .ok()
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default()
                })
                .await;
            this.update(cx, |this, cx| {
                let mut kept = saved;
                let before = kept.len();
                kept.retain(|s| !s.sub.steps.is_empty());
                let purged = kept.len() < before;
                let now = std::mem::take(&mut this.agents.done_subs);
                kept.retain(|s| !now.iter().any(|d| d.sub.id == s.sub.id));
                kept.extend(now);
                this.agents.done_subs = kept;
                if purged {
                    this.agents_save_done(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The (worktree, tab index) on screen right now, if the Agents view is showing one.
    fn agents_visible_tab(&self) -> Option<(&Path, usize)> {
        if self.mode != Mode::Agents || self.diff_view_open {
            return None;
        }
        let wt = self.agents.selected.as_deref()?;
        let g = self.agents.tabs.get(wt)?;
        (g.active < g.tabs.len()).then_some((wt, g.active))
    }

    /// A tab printed something: it's working, and — if you weren't looking — unseen.
    /// Repaints only when its dot changes (agents stream output many times a second).
    fn agents_on_output(
        &mut self,
        wt: &Path,
        v: &Entity<terminal::TerminalView>,
        cx: &mut Context<Self>,
    ) {
        let visible = self.agents_visible_tab().map(|(w, i)| (w.to_path_buf(), i));
        let Some(g) = self.agents.tabs.get_mut(wt) else {
            return;
        };
        let Some(i) = g.tabs.iter().position(|t| &t.view == v) else {
            return;
        };
        let on_screen = visible.is_some_and(|(w, vi)| w == wt && vi == i);
        let tab = &mut g.tabs[i];
        let now = Instant::now();
        tab.last_output = Some(now);
        tab.last_active = now;
        // Only output after you've prompted it is news: an auto-started agent's banner or a
        // resumed session's replay printing off screen isn't.
        tab.unseen |= !on_screen && tab.prompted;
        let state = AgentState::of_tab(&tab_signals(tab, cx), now);
        if state != tab.shown {
            tab.shown = state;
            cx.notify();
        }
    }

    /// A prompt was submitted in an agent tab. The tab's first one — and any while its
    /// worktree still has no title — schedules one context read a little later, once Claude
    /// Code has had time to title the session; after that the 5-minute cadence takes over.
    fn agents_on_submit(
        &mut self,
        wt: &Path,
        v: &Entity<terminal::TerminalView>,
        cx: &mut Context<Self>,
    ) {
        let untitled = self
            .agents
            .contexts
            .get(wt)
            .and_then(|c| c.title.as_ref())
            .is_none();
        let Some(tab) = self
            .agents
            .tabs
            .get_mut(wt)
            .and_then(|g| g.tabs.iter_mut().find(|t| &t.view == v))
        else {
            return;
        };
        let first = !tab.prompted;
        tab.prompted = true;
        tab.used = true;
        if !tab.agent || !(first || untitled) {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TITLE_AFTER_PROMPT).await;
            this.update(cx, Self::agents_refresh_contexts).ok();
        })
        .detach();
    }

    /// Start the 1 s ticker if it isn't running: it settles working dots back to idle and,
    /// every minute, runs the idle cleanup. It stops itself once no tabs remain.
    fn agents_ensure_ticker(&mut self, cx: &mut Context<Self>) {
        if self.agents.ticking {
            return;
        }
        self.agents.ticking = true;
        cx.spawn(async move |this, cx| {
            let mut ticks: u32 = 0;
            loop {
                cx.background_executor().timer(TICK).await;
                ticks = ticks.wrapping_add(1);
                let alive = this
                    .update(cx, |this, cx| this.agents_tick(ticks, cx))
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// One ticker step. Returns `false` (and clears `ticking`) once there are no tabs.
    fn agents_tick(&mut self, ticks: u32, cx: &mut Context<Self>) -> bool {
        if self.agents.tabs.is_empty() {
            self.agents.ticking = false;
            return false;
        }
        self.agents_poll_hooks(cx);
        #[cfg(unix)]
        if ticks.is_multiple_of(HOST_SCAN_TICKS) {
            self.agents_scan_hosts(cx);
        }
        let finished = self.agents_refresh_states(cx);
        // Keep the rows' git badges current: right after an agent stops working (it likely
        // changed files), and every few seconds while the view is on screen.
        if self.mode == Mode::Agents && (finished || ticks.is_multiple_of(GIT_EVERY_TICKS)) {
            self.agents_refresh_branches(cx);
            // Throttled to MAIN_SYNC_EVERY inside.
            self.agents_sync_mains(cx);
        }
        if self.mode == Mode::Agents && ticks.is_multiple_of(CONTEXT_EVERY_TICKS) {
            self.agents_refresh_contexts(cx);
        }
        let gh_tab = self.agents.side_tab;
        // CI runs change by the second while one is under way.
        let every = if gh_tab == SideTab::Ci && self.agents_ci_busy() {
            ci::CI_EVERY_TICKS
        } else {
            GH_EVERY_TICKS
        };
        if self.mode == Mode::Agents && gh_tab != SideTab::Code && ticks.is_multiple_of(every) {
            self.agents_refresh_gh(gh_tab, true, cx);
        }
        if self.mode == Mode::Agents
            && ticks.is_multiple_of(SUB_AGE_TICKS)
            && self.agents.selected.as_deref().is_some_and(|w| {
                self.agents_subs_in(w).next().is_some() || self.agents_done_in(w).next().is_some()
            })
        {
            cx.notify();
        }
        if ticks.is_multiple_of(SWEEP_EVERY_TICKS) {
            self.agents_sweep(false, cx);
        }
        true
    }

    /// Re-decide every tab's dot (hook report, output clock, unseen), and alert when a tab
    /// with no hook goes quiet off screen. Returns whether a tab stopped working (its files
    /// likely changed).
    fn agents_refresh_states(&mut self, cx: &mut Context<Self>) -> bool {
        let now = Instant::now();
        let wall = std::time::SystemTime::now();
        let (mut changed, mut finished, mut done) = (false, false, false);
        let mut quiet = Vec::new();
        for (home, tab) in self
            .agents
            .tabs
            .iter_mut()
            .flat_map(|(wt, g)| g.tabs.iter_mut().map(move |t| (wt, t)))
        {
            // A background agent silent this long is gone (its files go with the next read);
            // its work stays until you dismiss it.
            let stale = |s: &crate::agents::SubAgent| {
                wall.duration_since(s.at).unwrap_or_default() >= crate::agents::SUB_STALE
            };
            if tab.subs.iter().any(stale) {
                let from = tab.name.clone().unwrap_or_else(|| tab_auto_title(tab, cx));
                quiet.extend(tab.subs.extract_if(.., |s| stale(s)).map(|sub| {
                    crate::agents::DoneSub {
                        sub,
                        tab: tab.id.clone(),
                        home: home.clone(),
                        from: from.clone(),
                        finished: false,
                    }
                }));
                changed = true;
            }
            let state = AgentState::of_tab(&tab_signals(tab, cx), now);
            if state != tab.shown {
                finished |= tab.shown == AgentState::Working;
                // Without the hook, going quiet off screen after a prompt is "done".
                done |= tab.hook.is_none() && tab.agent && state == AgentState::Unseen;
                tab.shown = state;
                changed = true;
            }
        }
        if done {
            self.agents_alert(alert::Sound::Done);
        }
        self.agents_keep_done(quiet, &[], cx);
        if changed {
            cx.notify();
        }
        finished
    }

    /// Read the agents' hook state files that changed since last time — off the UI thread.
    pub(crate) fn agents_poll_hooks(&mut self, cx: &mut Context<Self>) {
        if self.agents.hooks_polling {
            return;
        }
        type Wrote = Option<(std::time::SystemTime, Option<crate::agents::WriteReport>)>;
        type Seen = Option<std::time::SystemTime>;
        type Followed = std::collections::HashSet<String>;
        let probes: Vec<(PathBuf, String, Seen, Wrote, Seen, Followed)> = self
            .agents
            .tabs
            .iter()
            .flat_map(|(wt, g)| {
                g.tabs.iter().filter(|t| t.agent).map(|t| {
                    let (id, followed) = (t.id.clone(), t.followed.clone());
                    (
                        wt.clone(),
                        id,
                        t.hook_seen,
                        t.wrote.clone(),
                        t.subs_seen,
                        followed,
                    )
                })
            })
            .collect();
        if probes.is_empty() {
            return;
        }
        self.agents.hooks_polling = true;
        cx.spawn(async move |this, cx| {
            let found: Vec<HookRead> = cx
                .background_executor()
                .spawn(async move {
                    probes
                        .into_iter()
                        .filter_map(|(wt, id, seen, wrote, subs_seen, followed)| {
                            let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified());
                            let path = crate::agents::state_path(&id);
                            let m = mtime(&path).ok()?;
                            // Its background agents report to their own files.
                            let sm = mtime(&crate::agents::sub_dir(&id)).ok();
                            if Some(m) == seen && sm == subs_seen {
                                return None;
                            }
                            let report = std::fs::read_to_string(&path)
                                .ok()
                                .and_then(|s| crate::agents::parse_hook(&s));
                            // Its session's latest report says which of them still work…
                            let parent = report.as_ref().map(|r| (r, m));
                            let subs = (sm.is_some() || subs_seen.is_some())
                                .then(|| (sm, crate::agents::read_subs(&id, parent)));
                            // …and whether the tab goes where one of them works.
                            let follow = subs.as_ref().and_then(|(_, read)| {
                                crate::agents::helper_to_follow(&wt, &read.live, &followed)
                            });
                            let session = report.as_ref().and_then(|r| {
                                crate::agents::session_worktree(
                                    r.cwd.as_deref()?,
                                    r.transcript.as_deref()?,
                                )
                            });
                            if Some(m) == seen {
                                let (state, moved, wrote) = (None, None, None);
                                return Some(HookRead {
                                    wt,
                                    id,
                                    state,
                                    moved,
                                    wrote,
                                    subs,
                                    session,
                                    follow,
                                });
                            }
                            // Its latest write, read again only when there's a new one.
                            let (write_seen, kept) = wrote.unzip();
                            let kept = kept.flatten();
                            let wpath = crate::agents::write_path(&id);
                            let wrote = mtime(&wpath)
                                .ok()
                                .filter(|wm| Some(*wm) != write_seen)
                                .map(|wm| {
                                    let fresh = std::fs::read_to_string(&wpath)
                                        .ok()
                                        .and_then(|s| crate::agents::parse_write(&s));
                                    (wm, crate::agents::repo_write(&wt, fresh, kept.clone()))
                                });
                            let last = wrote.as_ref().map_or(kept, |(_, w)| w.clone());
                            let moved =
                                crate::agents::moved_worktree(&wt, report.as_ref(), last.as_ref());
                            let state = Some((m, report));
                            Some(HookRead {
                                wt,
                                id,
                                state,
                                moved,
                                wrote,
                                subs,
                                session,
                                follow,
                            })
                        })
                        .collect()
                })
                .await;
            this.update(cx, |this, cx| {
                this.agents.hooks_polling = false;
                this.agents_apply_hooks(found, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Take in fresh hook reports: the exact session (for resume), the agent's state — and
    /// alert when one finishes or starts waiting on you while you're not looking at it.
    pub(crate) fn agents_apply_hooks(&mut self, found: Vec<HookRead>, cx: &mut Context<Self>) {
        use crate::agents::Hook;
        let visible = self.agents_visible_tab().map(|(w, i)| (w.to_path_buf(), i));
        let active = self.agents.window_active;
        let (mut learned, mut sound, mut moves) = (false, None, Vec::new());
        let (mut subs_changed, mut stopped, mut live) = (false, Vec::new(), Vec::new());
        // Worktrees with review feedback waiting, whose agents just reported: each may be free
        // for it now. (Only these — a busy check for every queue each tick would spawn `pgrep`s.)
        let mut waiting: Vec<PathBuf> = found
            .iter()
            .map(|f| &f.wt)
            .filter(|wt| self.agents.feedback.contains_key(*wt))
            .cloned()
            .collect();
        waiting.sort();
        waiting.dedup();
        for HookRead {
            wt,
            id,
            state,
            moved,
            wrote,
            subs,
            session,
            follow,
        } in found
        {
            let Some(g) = self.agents.tabs.get_mut(&wt) else {
                continue;
            };
            let Some(i) = g.tabs.iter().position(|t| t.id == id) else {
                continue;
            };
            let on_screen = visible.as_ref().is_some_and(|(w, vi)| *w == wt && *vi == i);
            let t = &mut g.tabs[i];
            // A helper took it where it is: its session reporting from where it was then
            // doesn't take it back — its session moving, or writing itself, does.
            let mut moved = moved;
            if let Some(held) = t.held.clone() {
                if wrote.is_none() && session.as_ref().is_none_or(|s| *s == held) {
                    moved = None;
                } else {
                    t.held = None;
                }
            }
            // Its session's work is where its one helper works: the tab goes there too.
            if let Some((helper, to)) = follow.filter(|_| moved.is_none()) {
                if t.followed.insert(helper) {
                    t.held.clone_from(&session);
                    moved = Some(to);
                }
            }
            if let Some(to) = moved {
                moves.push((wt.clone(), id.clone(), to));
            }
            if let Some((seen, read)) = subs {
                t.subs_seen = seen;
                let from = t.name.clone().unwrap_or_else(|| tab_auto_title(t, cx));
                stopped.extend(read.stopped.into_iter().map(|(sub, finished)| {
                    crate::agents::DoneSub {
                        sub,
                        tab: t.id.clone(),
                        home: wt.clone(),
                        from: from.clone(),
                        finished,
                    }
                }));
                live.extend(read.live.iter().map(|s| s.id.clone()));
                subs_changed |= t.subs != read.live;
                t.subs = read.live;
            }
            let Some((mtime, report)) = state else {
                continue;
            };
            t.hook_seen = Some(mtime);
            if wrote.is_some() {
                t.wrote = wrote;
            }
            let Some(r) = report else { continue };
            if let Some(s) = r.session.filter(|s| t.session.as_ref() != Some(s)) {
                t.session = Some(s);
                learned = true;
            }
            let looking = on_screen && active;
            let was = t.hook.as_ref().map(|(h, _)| h);
            match (was, &r.hook) {
                (Some(Hook::Blocked(_)), Hook::Blocked(_)) => {}
                (_, Hook::Blocked(_)) if !looking => sound = Some(alert::Sound::NeedsInput),
                (Some(Hook::Working | Hook::Blocked(_)), Hook::Done) => {
                    t.unseen |= !on_screen;
                    // Its background agents still work: it'll pick up again — not done yet.
                    if !looking && sound.is_none() && t.subs.is_empty() {
                        sound = Some(alert::Sound::Done);
                    }
                }
                _ => {}
            }
            if r.hook == Hook::Working {
                t.used = true;
            }
            // The file's age, as an Instant (it may predate this launch).
            let age = std::time::SystemTime::now()
                .duration_since(mtime)
                .unwrap_or_default();
            let at = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
            let done = r.hook == Hook::Done;
            t.view.update(cx, |v, cx| v.set_claude_done(done, cx));
            t.hook = Some((r.hook, at));
        }
        if let Some(s) = sound {
            self.agents_alert(s);
        }
        if learned {
            self.agents_save(cx);
        }
        for (from, id, to) in moves {
            self.agents_move_tab(&from, &id, to, cx);
        }
        self.agents_keep_done(stopped, &live, cx);
        if subs_changed {
            // A feed you're at the bottom of follows its newest step.
            let h = &self.agents.sub_scroll;
            if -h.offset().y >= h.max_offset().height - px(24.0) {
                h.scroll_to_bottom();
            }
            cx.notify();
        }
        self.agents_refresh_states(cx);
        for wt in waiting {
            self.agents_flush_feedback(&wt, false, cx);
        }
    }

    /// The worktree tab `v` is in now.
    fn agents_wt_of(&self, v: &Entity<terminal::TerminalView>) -> Option<PathBuf> {
        self.agents
            .tabs
            .iter()
            .find(|(_, g)| g.tabs.iter().any(|t| &t.view == v))
            .map(|(wt, _)| wt.clone())
    }

    /// Tab `id`'s agent moved itself from worktree `from` to `to` (Claude Code's
    /// `EnterWorktree` — a new task, see `agents_task_brief`): the tab follows, agent host and
    /// session alike. `to` joins `from`'s project at once (git lists it on the next refresh
    /// anyway), and is shown if the tab was on screen.
    pub(crate) fn agents_move_tab(
        &mut self,
        from: &Path,
        id: &str,
        to: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let on_screen = self
            .agents_visible_tab()
            .and_then(|(w, i)| (w == from).then(|| self.agents.tabs.get(w)?.tabs.get(i)))
            .flatten()
            .is_some_and(|t| t.id == id);
        let Some(g) = self.agents.tabs.get_mut(from) else {
            return;
        };
        let Some(i) = g.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        let len = g.tabs.len();
        let tab = g.tabs.remove(i);
        match crate::term_panel::active_after_close(len, i, g.active) {
            Some(a) => g.active = a,
            None => {
                self.agents.tabs.remove(from);
            }
        }
        if let Some(s) = self
            .agents
            .cfg
            .spaces
            .iter_mut()
            .find(|s| s.path == from || s.worktrees.iter().any(|w| w == from))
        {
            if s.path != to && !s.worktrees.contains(&to) {
                s.worktrees.push(to.clone());
            }
        }
        self.agents.restored.insert(to.clone());
        let g = self
            .agents
            .tabs
            .entry(to.clone())
            .or_insert_with(|| AgentTabs {
                tabs: Vec::new(),
                active: 0,
            });
        g.tabs.push(tab);
        g.active = g.tabs.len() - 1;
        if on_screen {
            self.agents_select(to, cx);
        }
        self.agents_save(cx);
        self.agents_refresh_branches(cx);
        cx.notify();
    }

    /// The mouse was released with a tab of worktree `wt` pressed: a dragged one moves to
    /// where the drop line was, and the tab on screen stays on screen.
    #[cfg(feature = "terminal")]
    fn agents_drop_tab(&mut self, wt: &Path, cx: &mut Context<Self>) {
        if !self.agents.tab_drag.pressed() {
            return;
        }
        let moved = self.agents.tab_drag.release();
        if let (Some((item, onto)), Some(g)) = (moved, self.agents.tabs.get_mut(wt)) {
            let pos = |g: &AgentTabs, id: &str| g.tabs.iter().position(|t| t.id == id);
            if let (Some(from), Some(to)) = (pos(g, &item), pos(g, &onto)) {
                let shown = g.tabs.get(g.active).map(|t| t.id.clone());
                let tab = g.tabs.remove(from);
                g.tabs.insert(to, tab);
                if let Some(a) = shown.and_then(|id| pos(g, &id)) {
                    g.active = a;
                }
                self.agents_save(cx);
            }
        }
        cx.notify();
    }

    /// The PR the detail shows: (project, PR).
    fn agents_selected_pr(&self) -> Option<(PathBuf, kyde_git::github::PullRequest)> {
        let (space, tab, _) = self.agents.gh_selected.clone()?;
        let Some(Ok(d)) = self
            .agents
            .gh_detail
            .as_ref()
            .filter(|_| tab == SideTab::Prs)
        else {
            return None;
        };
        let p = d.pr.as_ref()?;
        let pr = kyde_git::github::PullRequest {
            number: d.number,
            title: d.title.clone(),
            url: d.url.clone(),
            author: d.author.clone(),
            head: p.head.clone(),
            draft: p.draft,
        };
        Some((space, pr))
    }

    /// Action comments: hand every unresolved review thread to the PR's agent.
    fn agents_action_unresolved(&mut self, cx: &mut Context<Self>) {
        let items = self.agents.gh_unresolved.clone();
        if let Some((space, pr)) = self.agents_selected_pr() {
            self.agents_action_feedback(space, pr, items, cx);
        }
    }

    /// Action comment: hand the comment at `at` to the PR's agent.
    fn agents_action_comment(&mut self, at: FeedbackAt, cx: &mut Context<Self>) {
        let one = |author: &str, body: &str| crate::agents::Feedback {
            path: None,
            line: None,
            context: Vec::new(),
            comments: vec![(author.to_string(), body.to_string())],
        };
        let pair =
            |c: &(String, String, Entity<mdview::MarkdownView>, String)| (c.0.clone(), c.3.clone());
        let item = match at {
            FeedbackAt::Card(ci) => match self.agents.gh_cards.get(ci) {
                Some(GhCard::Comment {
                    author,
                    raw: Some(raw),
                    ..
                }) => Some(one(author, raw)),
                Some(GhCard::Review { author, raw, .. }) if !raw.trim().is_empty() => {
                    Some(one(author, raw))
                }
                _ => None,
            },
            // The thread's earlier comments go along, for what it's answering.
            FeedbackAt::Thread(ci, ti, k) => match self.agents.gh_cards.get(ci) {
                Some(GhCard::Review { threads, .. }) => threads.get(ti).and_then(|th| {
                    Some(crate::agents::Feedback {
                        path: Some(th.path.clone()),
                        line: th.line,
                        context: th.comments.get(..k)?.iter().map(pair).collect(),
                        comments: vec![pair(th.comments.get(k)?)],
                    })
                }),
                _ => None,
            },
        };
        if let (Some(item), Some((space, pr))) = (item, self.agents_selected_pr()) {
            self.agents_action_feedback(space, pr, vec![item], cx);
        }
    }

    /// Hand `items` on `pr` to the agent in its worktree — one per PR (`pr-<n>`, or the one on
    /// its branch), so every hand-off on it goes to the same place. They wait while that
    /// worktree is set up or its agent is busy, and go to it once it finishes its turn; with
    /// no agent alive there, a fresh one starts on them.
    fn agents_action_feedback(
        &mut self,
        space: PathBuf,
        pr: kyde_git::github::PullRequest,
        items: Vec<crate::agents::Feedback>,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        let wt = self.agents_pr_worktree(&space, &pr).unwrap_or_else(|| {
            kyde_config::agents::worktree_path(&space, &format!("pr-{}", pr.number))
        });
        let fb = self
            .agents
            .feedback
            .entry(wt.clone())
            .or_insert_with(|| PrFeedback {
                space,
                pr: pr.clone(),
                items: Vec::new(),
                preparing: false,
                checking: false,
            });
        fb.pr = pr;
        // A second click on the same comment before it's gone over adds nothing.
        for f in items {
            if !fb.items.contains(&f) {
                fb.items.push(f);
            }
        }
        self.agents_flush_feedback(&wt, true, cx);
    }

    /// Hand worktree `wt`'s queued feedback over if its agent can take it now: one that's
    /// finished its turn (or never been prompted) — checked off the UI thread to still be
    /// running first — gets it typed in; with none alive a fresh one starts on it; a busy one
    /// (working, or asking you something, where Enter would answer) is left until its next
    /// hook report. `clicked`: from a button, so show the agent; else (a hook report) just say
    /// so in the note.
    fn agents_flush_feedback(&mut self, wt: &Path, clicked: bool, cx: &mut Context<Self>) {
        use crate::agents::Hook;
        let hooks = self.agents.cfg.claude_hooks;
        let creating = self.agents.creating.iter().any(|c| c.path == wt);
        let Some(fb) = self.agents.feedback.get(wt) else {
            return;
        };
        let n = fb.pr.number;
        if fb.items.is_empty() {
            self.agents.feedback.remove(wt);
            return;
        }
        if fb.preparing || fb.checking || creating {
            if creating && !fb.preparing && !hooks {
                // A review is setting it up, and without the hook nothing tells when its agent
                // is free.
                self.agents.feedback.remove(wt);
                self.agents.note = Some(format!(
                    "#{n}'s worktree is still being set up — try again once it's ready"
                ));
            } else if clicked {
                self.agents.note = Some(format!(
                    "Queued for #{n} — its agent gets it as soon as it's ready"
                ));
            }
            cx.notify();
            return;
        }
        // Without the hook there's no telling whether an agent is free: a fresh one each time.
        if !hooks {
            self.agents_feedback_fresh(wt, clicked, cx);
            return;
        }
        let tabs = self.agents.tabs.get(wt).map_or(&[][..], |g| &g.tabs[..]);
        let starting = |t: &AgentTab| {
            t.hook.is_none() && t.spawned_at.elapsed().is_ok_and(|d| d < FEEDBACK_STARTING)
        };
        let live: Vec<&AgentTab> = tabs
            .iter()
            .filter(|t| {
                t.agent && (t.hook.as_ref().is_some_and(|(h, _)| *h != Hook::Ended) || starting(t))
            })
            .collect();
        if live.is_empty() {
            self.agents_feedback_fresh(wt, clicked, cx);
            return;
        }
        // A seeded tab reports Ready just before its first prompt lands — only a never-prompted
        // one is really waiting.
        let free = live.iter().rev().find(|t| match &t.hook {
            Some((Hook::Done, _)) => true,
            Some((Hook::Ready, _)) => !t.prompted,
            _ => false,
        });
        let Some(tab) = free else {
            if clicked {
                self.agents.note = Some(format!(
                    "Queued for #{n} — its agent gets it once it finishes its turn"
                ));
                cx.notify();
            }
            return;
        };
        // Its shell still runs the agent (else zsh would run the pasted lines), and its state
        // file still says it's free — both read off the UI thread.
        let (id, pid) = (tab.id.clone(), tab.view.read(cx).shell_pid());
        if let Some(fb) = self.agents.feedback.get_mut(wt) {
            fb.checking = true;
        }
        let wt = wt.to_path_buf();
        cx.spawn(async move |this, cx| {
            let state = crate::agents::state_path(&id);
            let (alive, hook) = cx
                .background_executor()
                .spawn(async move {
                    let alive = pid != 0 && !crate::agents::shell_is_idle(pid);
                    let hook = std::fs::read_to_string(state)
                        .ok()
                        .and_then(|s| crate::agents::parse_hook(&s))
                        .map(|r| r.hook);
                    (alive, hook)
                })
                .await;
            this.update(cx, |this, cx| {
                this.agents_feedback_checked(&wt, &id, alive, hook, clicked, cx);
            })
            .ok();
        })
        .detach();
    }

    /// The check on tab `id` (in `wt`) is back: type the feedback in if its agent is still
    /// running and free; start a fresh one if it has quit.
    fn agents_feedback_checked(
        &mut self,
        wt: &Path,
        id: &str,
        alive: bool,
        hook: Option<crate::agents::Hook>,
        clicked: bool,
        cx: &mut Context<Self>,
    ) {
        use crate::agents::Hook;
        let Some(fb) = self.agents.feedback.get_mut(wt) else {
            return;
        };
        fb.checking = false;
        let n = fb.pr.number;
        let Some((i, tab)) = self
            .agents
            .tabs
            .get(wt)
            .and_then(|g| g.tabs.iter().enumerate().find(|(_, t)| t.id == id))
        else {
            // Closed or moved meanwhile: decide again.
            self.agents_flush_feedback(wt, clicked, cx);
            return;
        };
        if !alive || hook == Some(Hook::Ended) {
            self.agents_feedback_fresh(wt, clicked, cx);
            return;
        }
        let free = match hook {
            Some(Hook::Done) => true,
            Some(Hook::Ready) => !tab.prompted,
            _ => false,
        };
        if free {
            self.agents_feedback_submit(wt, i, clicked, cx);
        } else if clicked {
            // Its next hook report tries again.
            self.agents.note = Some(format!(
                "Queued for #{n} — its agent gets it once it finishes its turn"
            ));
            cx.notify();
        }
    }

    /// Type `wt`'s queued feedback into its tab `i` as one prompt, and send it — unless you've
    /// typed there since its agent last reported (your unsent text may be in its box: the
    /// prompt joins it, for you to send).
    fn agents_feedback_submit(
        &mut self,
        wt: &Path,
        i: usize,
        clicked: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = self.agents.tabs.get_mut(wt).and_then(|g| g.tabs.get_mut(i)) else {
            return;
        };
        let Some(fb) = self.agents.feedback.remove(wt) else {
            return;
        };
        let prompt = crate::agents::feedback_prompt(&fb.pr, &fb.items);
        let at = t.hook.as_ref().map(|(_, at)| *at);
        let enter = t
            .view
            .read(cx)
            .last_input
            .is_none_or(|typed| at.is_some_and(|at| typed <= at));
        t.view
            .update(cx, |v, cx| v.submit_prompt(&prompt, enter, cx));
        if enter {
            // Working at once, so a second click before its hook says so doesn't type it twice.
            t.hook = Some((crate::agents::Hook::Working, Instant::now()));
            t.view.update(cx, |v, cx| v.set_claude_done(false, cx));
        }
        let (n, count) = (fb.pr.number, fb.items.len());
        let what = if count == 1 {
            "a review comment".to_string()
        } else {
            format!("{count} review comments")
        };
        self.agents.note = Some(if enter {
            format!("Handed {what} on #{n} to its agent")
        } else {
            format!("Typed {what} on #{n} into its agent — press Enter there to send")
        });
        if clicked {
            self.agents.side_tab = SideTab::Code;
            if let Some(g) = self.agents.tabs.get_mut(wt) {
                g.active = i;
            }
            self.agents_select(wt.to_path_buf(), cx);
        }
        cx.notify();
    }

    /// Start a fresh agent on `wt`'s queued feedback, named "Comments #n": in a new `pr-<n>`
    /// worktree when there's none (as Review makes it), else after bringing the worktree up to
    /// the PR's latest commits (as Re-review does) — unless an agent there might still be at
    /// work. It starts on everything queued by then.
    fn agents_feedback_fresh(&mut self, wt: &Path, clicked: bool, cx: &mut Context<Self>) {
        let existing = match self.agents.feedback.get(wt) {
            Some(fb) => self.agents_pr_worktree(&fb.space, &fb.pr).as_deref() == Some(wt),
            None => return,
        };
        // With the hook on, no agent here is alive (see `agents_flush_feedback`).
        let update = self.agents.cfg.claude_hooks || !self.agents_has_tabs(wt);
        let Some(fb) = self.agents.feedback.get_mut(wt) else {
            return;
        };
        fb.preparing = true;
        let (space, pr) = (fb.space.clone(), fb.pr.clone());
        let n = pr.number;
        let tab = format!("Comments #{n}");
        if !existing {
            // The prompt is made again once the worktree is ready (`agents_feedback_take`).
            let start = WtStart {
                prompt: crate::agents::feedback_prompt(&pr, &fb.items),
                tab,
                origin: format!("Comments on #{n}: {}", pr.title),
                doing: format!("Setting up #{n} to act on its review comments"),
            };
            self.agents_create_worktree(space, format!("pr-{n}"), WtSource::Pr(n), Some(start), cx);
            return;
        }
        if clicked {
            self.agents.side_tab = SideTab::Code;
        }
        self.agents.note = Some(format!("Updating #{n} for its review comments…"));
        cx.notify();
        let wt = wt.to_path_buf();
        cx.spawn(async move |this, cx| {
            let dir = wt.clone();
            let res = cx
                .background_executor()
                .spawn(async move { update.then(|| kyde_git::github::pr_checkout(&dir, n)) })
                .await;
            this.update(cx, |this, cx| {
                this.agents.note = Some(match res {
                    Some(Err(e)) => format!(
                        "Couldn't update #{n}: {} — acting on what's there",
                        crate::agents::git_reason(&e.to_string())
                    ),
                    _ => format!("Started an agent on #{n}'s review comments"),
                });
                let Some(prompt) = this.agents_feedback_take(&wt) else {
                    return;
                };
                // New tab first: selecting first would resume an old session in an empty strip.
                this.agents_new_tab_as(wt.clone(), Some(prompt), Some(tab), cx);
                if clicked {
                    this.agents_select(wt, cx);
                }
                this.agents_refresh_branches(cx);
            })
            .ok();
        })
        .detach();
    }

    /// The first prompt for the fresh agent set up for `wt`'s feedback: everything queued by
    /// now, handed over with it. `None` if no agent was being set up for it.
    fn agents_feedback_take(&mut self, wt: &Path) -> Option<String> {
        if !self.agents.feedback.get(wt).is_some_and(|f| f.preparing) {
            return None;
        }
        let fb = self.agents.feedback.remove(wt)?;
        Some(crate::agents::feedback_prompt(&fb.pr, &fb.items))
    }

    /// An agent finished or needs you: a sound (one per burst), and a Dock bounce (Linux: a
    /// desktop notification) while Kyde is in the background — each per its setting.
    fn agents_alert(&mut self, sound: alert::Sound) {
        let quiet = self
            .agents
            .last_sound
            .is_some_and(|t| t.elapsed() < Duration::from_millis(1500));
        if self.agents.cfg.sound && !quiet {
            self.agents.last_sound = Some(Instant::now());
            alert::play(sound);
        }
        if self.agents.cfg.dock_alerts && !self.agents.window_active {
            alert::request_attention(sound);
        }
    }

    /// The cleanup pass — every minute, and on demand from the Clean Up window. It gathers
    /// each tab's facts off the UI thread (is its agent still running, is its branch's PR
    /// merged, is its worktree clean, and — for quiet agent tabs not judged since their last
    /// output — a quick AI "is this done?"), then marks finished tabs and, in Auto mode,
    /// closes them. `manual` = from the Clean Up window: never auto-closes, it pre-ticks.
    pub(crate) fn agents_sweep(&mut self, manual: bool, cx: &mut Context<Self>) {
        let mode = self.agents.cfg.cleanup_mode;
        if self.agents.sweeping || (mode == CleanupMode::Off && !manual) {
            return;
        }
        let rules = self.agents.cfg.cleanup;
        let now = Instant::now();
        let visible = self.agents_visible_tab().map(|(w, i)| (w.to_path_buf(), i));
        // Per tab: who it is, its shell — and, for quiet agent tabs the AI hasn't judged
        // since their last output, the question to ask (a few per pass).
        let mut probes: Vec<Probe> = Vec::new();
        let mut budget = MAX_AI_CHECKS;
        for (wt, g) in &self.agents.tabs {
            let pinned = self.agents.cfg.pinned.contains(wt);
            for (i, t) in g.tabs.iter().enumerate() {
                let on_screen = visible.as_ref().is_some_and(|(w, vi)| w == wt && *vi == i);
                let ask = rules.ai_done
                    && !pinned
                    && budget > 0
                    && t.agent
                    && !on_screen
                    && !t.unseen
                    && now.saturating_duration_since(t.last_active) >= rules.quiet()
                    && t.ai.as_ref().is_none_or(|a| a.after != t.last_output);
                let question = ask.then(|| {
                    budget -= 1;
                    self.agents_done_question(wt, t, cx)
                });
                probes.push((
                    wt.clone(),
                    t.spawned_at,
                    t.view.read(cx).shell_pid(),
                    t.last_output,
                    question,
                ));
            }
        }
        if probes.is_empty() {
            return;
        }
        // Merged-PR heads per space, re-fetched at most every 10 minutes (one `gh` call each).
        let stale: Vec<PathBuf> = if rules.merged {
            self.agents
                .cfg
                .spaces
                .iter()
                .filter(|s| self.agents.git_spaces.contains(&s.path))
                .filter(|s| {
                    self.agents
                        .merged
                        .get(&s.path)
                        .is_none_or(|(at, _)| at.elapsed() >= MERGED_EVERY)
                })
                .map(|s| s.path.clone())
                .collect()
        } else {
            Vec::new()
        };
        let worktrees: std::collections::HashSet<PathBuf> =
            probes.iter().map(|p| p.0.clone()).collect();
        self.agents.sweeping = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    let mut f = crate::agents::SweepFound::default();
                    for (wt, spawned, pid, after, question) in probes {
                        let key = (wt, spawned);
                        f.at_prompt
                            .push((key.clone(), crate::agents::shell_is_idle(pid)));
                        if let Some(q) = question {
                            f.ai.push((key, after, crate::agents::ai_says_done(&q)));
                        }
                    }
                    for wt in worktrees {
                        let clean = crate::agents::worktree_is_clean(&wt);
                        f.clean.insert(wt, clean);
                    }
                    for space in stale {
                        if let Ok(heads) = kyde_git::github::merged_heads(&space) {
                            f.merged.push((space, heads));
                        }
                    }
                    f
                })
                .await;
            this.update(cx, |this, cx| {
                this.agents.sweeping = false;
                this.agents_apply_sweep(found, manual, cx);
            })
            .ok();
        })
        .detach();
    }

    /// The AI check's question about tab `t` of worktree `wt`: its title and prompts (from
    /// its Claude Code session), its git state, and the last lines on its screen.
    fn agents_done_question(&self, wt: &Path, t: &AgentTab, cx: &gpui::App) -> String {
        let ctx = self.agents.contexts.get(wt);
        let field = |f: Option<&String>| f.map_or("(unknown)".to_string(), |s| clip(s, 400));
        let g = self.agents.git.get(wt).copied().unwrap_or_default();
        let git = format!(
            "{} uncommitted change(s), {} commit(s) not pushed",
            g.changed, g.ahead
        );
        crate::agents::done_prompt(
            &field(ctx.and_then(|c| c.title.as_ref())),
            &field(ctx.and_then(|c| c.first_prompt.as_ref())),
            &field(ctx.and_then(|c| c.last_prompt.as_ref())),
            &git,
            &t.view.read(cx).tail_text(30),
        )
    }

    /// Decide with what the probe found: mark each tab's `verdict`, and — in Auto mode, not
    /// from the Clean Up window — close the finished ones; from the Clean Up window, tick the
    /// ones this pass found finished. Returns how many it closed.
    pub(crate) fn agents_apply_sweep(
        &mut self,
        found: crate::agents::SweepFound,
        manual: bool,
        cx: &mut Context<Self>,
    ) -> usize {
        let now = Instant::now();
        for (space, heads) in found.merged {
            self.agents.merged.insert(space, (now, heads));
        }
        for ((wt, spawned), after, done) in found.ai {
            if let Some(t) = self
                .agents
                .tabs
                .get_mut(&wt)
                .and_then(|g| g.tabs.iter_mut().find(|t| t.spawned_at == spawned))
            {
                t.ai = Some(AiCheck { after, done });
            }
        }
        let at_prompt: std::collections::HashMap<_, _> = found.at_prompt.into_iter().collect();
        // Worktrees whose branch has a merged PR.
        let merged: std::collections::HashSet<PathBuf> = self
            .agents
            .tabs
            .keys()
            .filter(|wt| {
                let space = self.agents.cfg.space_of(wt).map(|s| s.path.clone());
                let branch = self.agents.branches.get(*wt);
                space
                    .and_then(|s| self.agents.merged.get(&s))
                    .zip(branch)
                    .is_some_and(|((_, heads), b)| heads.contains(b))
                    // …or whose work is on main already (squash-merged).
                    || self.agents.git.get(*wt).is_some_and(|g| g.landed)
            })
            .cloned()
            .collect();
        let visible = self.agents_visible_tab().map(|(w, i)| (w.to_path_buf(), i));
        let (rules, hours) = (self.agents.cfg.cleanup, self.agents.cfg.idle_hours);
        let mut finished: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        let mut newly: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        for (wt, g) in &mut self.agents.tabs {
            let pinned = self.agents.cfg.pinned.contains(wt);
            for (i, t) in g.tabs.iter_mut().enumerate() {
                let facts = crate::agents::TabFacts {
                    on_screen: visible.as_ref().is_some_and(|(w, vi)| w == wt && *vi == i),
                    unseen: t.unseen,
                    idle: now.saturating_duration_since(t.last_active),
                    at_prompt: at_prompt
                        .get(&(wt.clone(), t.spawned_at))
                        .copied()
                        .unwrap_or(false),
                    unused: !t.used,
                    merged: merged.contains(wt),
                    clean: found.clean.get(wt).copied().unwrap_or(false),
                    ai_done: t.ai.as_ref().and_then(|a| a.done.clone()),
                };
                let was = t.verdict.is_some();
                // Closing it would end its background agents too; a pinned worktree's tabs
                // are never finished.
                t.verdict = if t.subs.is_empty() && !pinned {
                    crate::agents::closable(&facts, rules, hours)
                } else {
                    None
                };
                if t.verdict.is_some() {
                    finished.push((wt.clone(), t.spawned_at));
                    if !was {
                        newly.push((wt.clone(), t.spawned_at));
                    }
                }
            }
        }
        // Ones already finished when the window opened were ticked then — an untick stands.
        if manual {
            self.agents.cleanup_tabs.extend(newly);
        }
        let mut closed = 0;
        if !manual && self.agents.cfg.cleanup_mode == CleanupMode::Auto {
            for (wt, spawned) in finished {
                if let Some(i) = self
                    .agents
                    .tabs
                    .get(&wt)
                    .and_then(|g| g.tabs.iter().position(|t| t.spawned_at == spawned))
                {
                    self.agents_close_tab(&wt, i, cx);
                    closed += 1;
                }
            }
            // A stopped background agent's tab has nothing left running: it goes once it's
            // been stopped as long as a tab must be quiet — never the one on screen.
            if rules.agent_quit {
                let shown = self.agents_visible_sub();
                let stale: Vec<String> = self
                    .agents
                    .done_subs
                    .iter()
                    .filter(|d| shown != Some(d.sub.id.as_str()))
                    .filter(|d| d.sub.at.elapsed().is_ok_and(|e| e >= rules.quiet()))
                    .filter(|d| !self.agents_sub_pinned(d))
                    .map(|d| d.sub.id.clone())
                    .collect();
                closed += self.agents_dismiss_subs(&stale, cx);
            }
            if closed > 0 {
                self.agents.note = Some(format!(
                    "Closed {closed} finished tab{}",
                    if closed == 1 { "" } else { "s" }
                ));
            }
        }
        cx.notify();
        closed
    }

    /// One worktree's line in the Clean Up window: what removing it costs, whether it's safe
    /// to tick by default (nothing uncommitted, nothing unpushed, no agent busy), and whether
    /// removing it would lose work.
    fn agents_wt_cleanup(&self, space: &Path, wt: &Path) -> (String, bool, bool) {
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let merged = self
            .agents
            .merged
            .get(space)
            .zip(self.agents.branches.get(wt))
            .is_some_and(|((_, heads), b)| heads.contains(b));
        let busy = self
            .agents
            .tabs
            .get(wt)
            .is_some_and(|g| g.tabs.iter().any(|t| t.shown == AgentState::Working))
            || self.agents_subs_in(wt).next().is_some();
        // Removing it would take the worktree inside it along (git refuses; see
        // `kyde_git::GitError::NestedWorktree`).
        if let Some(inner) = self
            .agents
            .cfg
            .spaces
            .iter()
            .flat_map(|s| &s.worktrees)
            .find(|w| *w != wt && w.starts_with(wt))
        {
            return (
                format!("Holds worktree {} — remove it first", leaf_name(inner)),
                false,
                false,
            );
        }
        match self.agents.git.get(wt).copied() {
            None => ("Git state not read yet".into(), false, false),
            Some(g) if g.changed > 0 => (
                format!(
                    "{} uncommitted change{} — lost if removed",
                    g.changed,
                    plural(g.changed)
                ),
                false,
                true,
            ),
            Some(_) if busy => ("Its agent is working".into(), false, false),
            Some(_) if merged => ("PR merged — finished".into(), true, false),
            Some(g) if g.landed => ("Its changes are on main already".into(), true, false),
            Some(g) if g.ahead > 0 => (
                format!(
                    "{} commit{} not pushed — kept on the branch",
                    g.ahead,
                    plural(g.ahead)
                ),
                false,
                false,
            ),
            Some(_) => ("Nothing uncommitted, nothing to push".into(), true, false),
        }
    }

    /// The Clean Up window's ticked finished tabs that closing would act on: still open, still
    /// finished, and not in a worktree it's removing anyway (those close with it).
    fn agents_cleanup_tabs_to_close(&self) -> Vec<(PathBuf, std::time::SystemTime)> {
        self.agents
            .cleanup_tabs
            .iter()
            .filter(|(wt, spawned)| {
                !self.agents.cleanup_checked.contains(wt)
                    && self.agents.tabs.get(wt).is_some_and(|g| {
                        g.tabs
                            .iter()
                            .any(|t| t.spawned_at == *spawned && t.verdict.is_some())
                    })
            })
            .cloned()
            .collect()
    }

    /// The Clean Up window's ticked stopped background agents that closing would dismiss: still
    /// kept, and not in a worktree it's removing anyway (those go with it).
    fn agents_cleanup_subs_to_close(&self) -> Vec<String> {
        self.agents
            .done_subs
            .iter()
            .filter(|d| self.agents.cleanup_subs.contains(&d.sub.id))
            .filter(|d| {
                d.sub
                    .worktree
                    .as_ref()
                    .is_none_or(|w| !self.agents.cleanup_checked.contains(w))
            })
            .map(|d| d.sub.id.clone())
            .collect()
    }

    /// Act on the Clean Up window: close the ticked finished tabs and background agents, then
    /// remove the ticked worktrees — one after another (git locks the repo's worktree list),
    /// closing their tabs; branches are kept.
    pub(crate) fn agents_clean_up_checked(&mut self, cx: &mut Context<Self>) {
        let tabs = self.agents_cleanup_tabs_to_close();
        self.agents.cleanup_tabs.clear();
        let subs = self.agents_cleanup_subs_to_close();
        self.agents.cleanup_subs.clear();
        let mut closed = self.agents_dismiss_subs(&subs, cx);
        for (wt, spawned) in tabs {
            if let Some(i) = self
                .agents
                .tabs
                .get(&wt)
                .and_then(|g| g.tabs.iter().position(|t| t.spawned_at == spawned))
            {
                self.agents_close_tab(&wt, i, cx);
                closed += 1;
            }
        }
        if closed > 0 {
            self.agents.note = Some(format!(
                "Closed {closed} finished tab{}",
                if closed == 1 { "" } else { "s" }
            ));
        }
        let list: Vec<(PathBuf, PathBuf)> = self
            .agents
            .cleanup_checked
            .drain()
            .filter_map(|wt| {
                let space = self.agents.cfg.space_of(&wt)?.path.clone();
                (space != wt).then_some((space, wt))
            })
            .collect();
        self.close_modal_window(ModalKind::Cleanup, cx);
        self.agents_remove_in_background(list, cx);
    }

    /// Body of the Clean Up window: the tabs the cleanup found finished (the main checkout's
    /// too), then every linked worktree, grouped by project, with what removing it costs.
    /// Finished ones (nothing uncommitted, nothing unpushed) come ticked.
    pub(crate) fn render_cleanup_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let fs = t.ui_font_size;
        let section = |label: &'static str| {
            div()
                .pt_2()
                .px_2()
                .text_color(t.secondary_text)
                .child(label)
                .into_any_element()
        };
        let mut tab_rows: Vec<gpui::AnyElement> = Vec::new();
        let pinned = &self.agents.cfg.pinned;
        for space in &self.agents.cfg.spaces {
            for wt in space.rows() {
                if pinned.contains(&wt) {
                    continue;
                }
                let removing = self.agents.cleanup_checked.contains(&wt);
                // Stopped background agents: finished by definition.
                for d in self.agents_done_in(&wt) {
                    let id = d.sub.id.clone();
                    let mut line = format!(
                        "Background agent from “{}” · {}",
                        d.from,
                        if d.finished { "finished" } else { "stopped" }
                    );
                    if removing {
                        line.push_str(" · closes with its worktree");
                    }
                    let row = cleanup_row(
                        format!("cleanup-sub-{id}").into(),
                        removing || self.agents.cleanup_subs.contains(&id),
                        d.sub.what.clone(),
                        worktree_title(Some(&space.path), &wt),
                        line,
                        false,
                    );
                    tab_rows.push(if removing {
                        row.into_any_element()
                    } else {
                        row.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                if !this.agents.cleanup_subs.remove(&id) {
                                    this.agents.cleanup_subs.insert(id.clone());
                                }
                                cx.notify();
                            }),
                        )
                        .into_any_element()
                    });
                }
                let Some(g) = self.agents.tabs.get(&wt) else {
                    continue;
                };
                for tab in &g.tabs {
                    let Some(why) = &tab.verdict else {
                        continue;
                    };
                    let key = (wt.clone(), tab.spawned_at);
                    let mut line = format!("Finished: {why}");
                    if removing {
                        line.push_str(" · closes with its worktree");
                    }
                    let row = cleanup_row(
                        format!("cleanup-tab-{}", tab.id).into(),
                        removing || self.agents.cleanup_tabs.contains(&key),
                        tab.name.clone().unwrap_or_else(|| tab_auto_title(tab, cx)),
                        worktree_title(Some(&space.path), &wt),
                        line,
                        false,
                    );
                    tab_rows.push(if removing {
                        row.into_any_element()
                    } else {
                        row.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                if !this.agents.cleanup_tabs.remove(&key) {
                                    this.agents.cleanup_tabs.insert(key.clone());
                                }
                                cx.notify();
                            }),
                        )
                        .into_any_element()
                    });
                }
            }
        }
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        if !tab_rows.is_empty() || self.agents.sweeping {
            rows.push(section("Finished tabs"));
        }
        if tab_rows.is_empty() && self.agents.sweeping {
            rows.push(
                div()
                    .px_2()
                    .text_size(px(fs - 1.0))
                    .text_color(t.line_number)
                    .child("Checking which tabs are finished…")
                    .into_any_element(),
            );
        }
        rows.append(&mut tab_rows);
        // Scratch rows aren't worktrees: they go with their last tab.
        if self
            .agents
            .cfg
            .spaces
            .iter()
            .any(|s| !s.scratch && !s.worktrees.is_empty())
        {
            rows.push(section("Worktrees"));
        }
        for space in &self.agents.cfg.spaces {
            if space.scratch || space.worktrees.is_empty() {
                continue;
            }
            rows.push(
                div()
                    .pt_2()
                    .pb_0p5()
                    .px_2()
                    .text_size(px(fs - 1.0))
                    .text_color(t.line_number)
                    .child(leaf_name(&space.path))
                    .into_any_element(),
            );
            for wt in space.worktrees.iter().filter(|w| !pinned.contains(*w)) {
                let (status, _, danger) = self.agents_wt_cleanup(&space.path, wt);
                let checked = self.agents.cleanup_checked.contains(wt);
                let title = self
                    .agents
                    .contexts
                    .get(wt)
                    .and_then(|c| c.title.clone())
                    .unwrap_or_else(|| worktree_title(Some(&space.path), wt));
                let branch = self.agents.branches.get(wt).cloned().unwrap_or_default();
                let tabs = self.agents.tabs.get(wt).map_or(0, |g| g.tabs.len());
                let mut line = status;
                if tabs > 0 {
                    line.push_str(&format!(
                        " · {tabs} tab{} open",
                        if tabs == 1 { "" } else { "s" }
                    ));
                }
                let key = wt.clone();
                rows.push(
                    cleanup_row(
                        format!("cleanup-{}", wt.display()).into(),
                        checked,
                        title,
                        branch,
                        line,
                        danger,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            if !this.agents.cleanup_checked.remove(&key) {
                                this.agents.cleanup_checked.insert(key.clone());
                            }
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
                );
            }
        }
        let n = self.agents.cleanup_checked.len();
        let lossy = self
            .agents
            .cleanup_checked
            .iter()
            .filter(|wt| {
                self.agents
                    .cfg
                    .space_of(wt)
                    .is_some_and(|s| self.agents_wt_cleanup(&s.path, wt).2)
            })
            .count();
        let closing =
            self.agents_cleanup_tabs_to_close().len() + self.agents_cleanup_subs_to_close().len();
        let cancel = btn_secondary("cleanup-cancel", "Cancel").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _e, _w, cx| {
                this.agents.cleanup_checked.clear();
                this.agents.cleanup_tabs.clear();
                this.agents.cleanup_subs.clear();
                this.close_modal_window(ModalKind::Cleanup, cx);
            }),
        );
        let plural = |k: usize| if k == 1 { "" } else { "s" };
        let mut parts = Vec::new();
        if closing > 0 {
            parts.push(format!("close {closing} tab{}", plural(closing)));
        }
        if lossy > 0 {
            parts.push(format!("remove {n} (discarding changes in {lossy})"));
        } else if n > 0 {
            parts.push(format!("remove {n} worktree{}", plural(n)));
        }
        let mut label = parts.join(", ");
        if let Some(first) = label.get(..1).map(str::to_uppercase) {
            label.replace_range(..1, &first);
        }
        let remove = if parts.is_empty() {
            btn_primary_state("cleanup-remove", "Clean up", true)
        } else {
            btn_primary("cleanup-remove", label).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.agents_clean_up_checked(cx)),
            )
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .p_4()
            .gap_2()
            .font_family(theme::font::ui_family())
            .text_size(px(fs))
            .text_color(t.text)
            .child(div().text_color(t.line_number).child(
                "Ticked tabs are finished; closing one ends its agent. Ticked worktrees are \
                 finished — nothing uncommitted, nothing unpushed — so removing them loses no \
                 work. Their tabs close; their branches are kept.",
            ))
            .when(!pinned.is_empty(), |d| {
                d.child(
                    div()
                        .text_color(t.line_number)
                        .child("Pinned worktrees are left alone — they aren't listed."),
                )
            })
            .child(
                div()
                    .id("cleanup-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .when(rows.is_empty(), |d| {
                        d.child(
                            div()
                                .text_color(t.line_number)
                                .child("Nothing to clean up."),
                        )
                    })
                    .children(rows),
            )
            .child(ui::modal_footer().child(cancel).child(remove))
            .into_any_element()
    }

    /// Whether an agent tab owns keyboard focus, its find box included (⌘T / ⌘W then act on
    /// agent tabs).
    pub(crate) fn agents_terminal_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.mode == Mode::Agents
            && self.agents_visible_tab().is_some_and(|(wt, i)| {
                self.agents.tabs[wt].tabs[i]
                    .view
                    .read(cx)
                    .handle()
                    .contains_focused(window, cx)
            })
    }

    /// Some agent finished while you were elsewhere, or is waiting on you (the rail badge).
    pub(crate) fn agents_any_unseen(&self) -> bool {
        self.agents
            .tabs
            .values()
            .flat_map(|g| &g.tabs)
            .any(|t| matches!(t.shown, AgentState::Unseen | AgentState::Blocked))
    }

    /// ⌘T in an agent tab: another agent tab in the same worktree.
    pub(crate) fn agents_new_tab_here(&mut self, cx: &mut Context<Self>) {
        if let Some(wt) = self.agents.selected.clone() {
            self.agents_new_tab(wt, true, None, cx);
        }
    }

    /// ⌘W in an agent tab: close it (asking first while its agent isn't done).
    pub(crate) fn agents_close_visible_tab(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self
            .agents_visible_tab()
            .map(|(wt, i)| self.agents.tabs[wt].tabs[i].id.clone())
        {
            self.agents_ask_close(vec![id], cx);
        }
    }

    /// Focus the visible agent tab (now and next frame — a just-spawned tab isn't in the
    /// window tree yet; same gotcha as `focus_active_terminal`), or the app root if none.
    pub(crate) fn agents_focus_visible(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((wt, i)) = self.agents_visible_tab() else {
            window.focus(&self.focus_handle);
            return;
        };
        let handle = self.agents.tabs[wt].tabs[i].view.read(cx).handle();
        window.focus(&handle);
        window.defer(cx, move |window, _cx| window.focus(&handle));
    }
}

// ── pinning ──────────────────────────────────────────────────────────────────────────────
impl Kyde {
    /// Pin / unpin worktree `wt`. Pinned, it's long-running work the cleanup leaves alone:
    /// its tabs are never found finished (or closed), and the Clean Up window doesn't list it.
    pub(crate) fn agents_toggle_pin(&mut self, wt: &Path, cx: &mut Context<Self>) {
        if !self.agents.cfg.pinned.remove(wt) {
            self.agents.cfg.pinned.insert(wt.to_path_buf());
            #[cfg(feature = "terminal")]
            {
                // What the cleanup already found there no longer stands.
                for t in self
                    .agents
                    .tabs
                    .get_mut(wt)
                    .into_iter()
                    .flat_map(|g| &mut g.tabs)
                {
                    t.verdict = None;
                }
                self.agents.cleanup_checked.remove(wt);
                self.agents.cleanup_tabs.retain(|(w, _)| w != wt);
            }
        }
        self.agents_save(cx);
        cx.notify();
    }

    /// The menu item that pins / unpins `wt` — named for the worktree where the menu is a
    /// tab's (`named`), plainly on the row's own.
    fn agents_pin_entry(&self, wt: &Path, named: bool) -> MenuEntry {
        let label = match (self.agents.cfg.pinned.contains(wt), named) {
            (false, false) => "Pin",
            (false, true) => "Pin Worktree",
            (true, false) => "Unpin",
            (true, true) => "Unpin Worktree",
        };
        let wt = wt.to_path_buf();
        MenuEntry::item(label, move |this, _w, cx| this.agents_toggle_pin(&wt, cx))
    }

    /// A sidebar worktree row's right-click menu.
    pub(crate) fn agents_row_menu(&self, wt: &Path) -> Vec<MenuEntry> {
        vec![self.agents_pin_entry(wt, false)]
    }

    /// A project's menu (its `…`, or a right-click on its header): jump to its Issues / Pull
    /// Requests / CI (each tab, narrowed to it), move it to another group (or out of its own),
    /// into a new group, rename it.
    pub(crate) fn agents_space_menu(&self, space: &Path) -> Vec<MenuEntry> {
        let cfg = &self.agents.cfg;
        let Some(s) = cfg.spaces.iter().find(|s| s.path == space) else {
            return Vec::new();
        };
        let mut out: Vec<MenuEntry> = Vec::new();
        if self.agents.git_spaces.contains(&s.path) {
            out.extend(
                SIDE_TABS
                    .iter()
                    .filter(|(tab, ..)| *tab != SideTab::Code)
                    .map(|&(tab, _, label, _)| {
                        let space = space.to_path_buf();
                        MenuEntry::item(label, move |this, _w, cx| {
                            this.agents_jump_gh(space.clone(), tab, cx);
                        })
                    }),
            );
            out.push(MenuEntry::Divider);
        }
        // Scratch is never in a group: it stays at the bottom.
        if !s.scratch {
            let current = cfg.group_of(s).map(|g| g.id);
            out.extend(
                cfg.groups
                    .iter()
                    .filter(|g| Some(g.id) != current)
                    .map(|g| {
                        let (id, p) = (g.id, space.to_path_buf());
                        MenuEntry::item(format!("Move to “{}”", g.name), move |this, _w, cx| {
                            this.agents_move_to_group(&p, Some(id), cx);
                        })
                    }),
            );
            if current.is_some() {
                let p = space.to_path_buf();
                out.push(MenuEntry::item("Remove from Group", move |this, _w, cx| {
                    this.agents_move_to_group(&p, None, cx);
                }));
            }
            let p = space.to_path_buf();
            out.push(MenuEntry::item("New Group…", move |this, window, cx| {
                this.agents_new_group(Some(p.clone()), window, cx);
            }));
            out.push(MenuEntry::Divider);
        }
        let p = space.to_path_buf();
        out.push(MenuEntry::item("Rename…", move |this, window, cx| {
            this.agents_start_rename(RenameTarget::Space(p.clone()), window, cx);
        }));
        out
    }

    /// A sidebar group header's right-click menu.
    pub(crate) fn agents_group_menu(&self, id: u32) -> Vec<MenuEntry> {
        vec![
            MenuEntry::item("Rename…", move |this, window, cx| {
                this.agents_start_rename(RenameTarget::Group(id), window, cx);
            }),
            MenuEntry::item("Remove Group", move |this, _w, cx| {
                this.agents_remove_group(id, cx);
            }),
        ]
    }
}

// ── rendering ────────────────────────────────────────────────────────────────────────────
impl Kyde {
    /// The Agents view: spaces sidebar | resize gap | the selected worktree's tabs.
    pub(crate) fn render_agents(
        &mut self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        #[cfg(feature = "terminal")]
        {
            self.agents_mark_seen();
            self.agents_reveal_tab();
        }
        if self
            .agents
            .cfg
            .spaces
            .iter()
            .all(|s| s.scratch && s.worktrees.is_empty())
        {
            return self.render_agents_welcome(ui, cx);
        }
        let divider = div()
            .id("agents-divider")
            .w(px(theme::FRAME_GAP))
            .h_full()
            .flex_none()
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                    this.start_divider_drag(Divider::AgentsSidebar, e.position, window);
                    cx.notify();
                }),
            );
        let hidden = self.agents.sidebar_hidden;
        div()
            .key_context("Agents")
            .on_action(cx.listener(|this, _: &crate::AgentNext, _w, cx| this.agents_cycle(1, cx)))
            .on_action(cx.listener(|this, _: &crate::AgentPrev, _w, cx| this.agents_cycle(-1, cx)))
            .on_action(cx.listener(|this, _: &crate::AgentAttention, _w, cx| {
                this.agents_jump_to_attention(cx);
            }))
            .on_action(cx.listener(|this, a: &crate::AgentJump, _w, cx| this.agents_jump(a.0, cx)))
            .on_action(cx.listener(|this, _: &crate::AgentTabNext, _w, cx| {
                this.agents_cycle_tab(1, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::AgentTabPrev, _w, cx| {
                this.agents_cycle_tab(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::AgentNewWorktree, _w, cx| {
                let space = this
                    .agents
                    .selected
                    .as_deref()
                    .and_then(|w| this.agents.cfg.space_of(w))
                    .map(|s| (s.path.clone(), s.scratch));
                match space {
                    Some((_, true)) => this.agents_new_scratch(cx),
                    Some((space, false)) => this.agents_new_worktree(space, cx),
                    None => {}
                }
            }))
            .on_action(cx.listener(|this, _: &crate::AgentRename, window, cx| {
                this.agents_rename_visible_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::AgentScrollback, _w, cx| {
                this.agents_scrollback_visible(cx);
            }))
            .on_action(cx.listener(|this, _: &crate::AgentToggleSidebar, _w, cx| {
                this.agents.sidebar_hidden = !this.agents.sidebar_hidden;
                this.agents.focus_pending = true;
                cx.notify();
            }))
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .when(!hidden, |d| {
                d.child(self.render_agents_sidebar(ui, cx)).child(divider)
            })
            .child(if self.agents.side_tab == SideTab::Code {
                self.render_agents_main(ui, cx)
            } else {
                self.render_agents_gh_detail(ui, cx)
            })
            .into_any_element()
    }

    /// The tab on screen is never "unseen", and never idle while you watch it.
    #[cfg(feature = "terminal")]
    fn agents_mark_seen(&mut self) {
        let Some((wt, i)) = self.agents_visible_tab().map(|(w, i)| (w.to_path_buf(), i)) else {
            return;
        };
        if let Some(tab) = self
            .agents
            .tabs
            .get_mut(&wt)
            .and_then(|g| g.tabs.get_mut(i))
        {
            let now = Instant::now();
            tab.last_active = now;
            if tab.unseen {
                tab.unseen = false;
                let signals = crate::agents::Signals {
                    hook: tab.hook.clone(),
                    last_output: tab.last_output,
                    last_input: None,
                    unseen: false,
                    background: !tab.subs.is_empty(),
                };
                tab.shown = AgentState::of_tab(&signals, now);
            }
        }
    }

    /// Keep the shown tab in sight. The strip scrolls sideways once its tabs outgrow the pane
    /// (`render_agents_tabs`), so the tab or background agent that just came on screen —
    /// clicked, cycled to, opened, jumped to — is scrolled into view. Only when that changes:
    /// in between, the wheel is free to look along the strip.
    #[cfg(feature = "terminal")]
    fn agents_reveal_tab(&mut self) {
        let Some(wt) = self.agents.selected.clone() else {
            return;
        };
        let group = self.agents.tabs.get(&wt);
        // Its place in the strip: the tabs first, then the background agents.
        let shown = match self.agents_visible_sub() {
            Some(id) => self
                .agents_subs_in(&wt)
                .map(|(.., s)| s)
                .chain(self.agents_done_in(&wt).map(|d| &d.sub))
                .position(|s| s.id == id)
                .map(|k| (group.map_or(0, |g| g.tabs.len()) + k, id.to_string())),
            None => group.and_then(|g| Some((g.active, g.tabs.get(g.active)?.id.clone()))),
        };
        let Some((ix, id)) = shown else {
            return;
        };
        let now = (wt, id);
        if self.agents.tab_revealed.as_ref() != Some(&now) {
            self.agents.tab_scroll.scroll_to_item(ix);
            self.agents.tab_revealed = Some(now);
        }
    }

    /// No spaces yet: explain the model and offer the open project / a folder picker.
    fn render_agents_welcome(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let current = self.repo_root.clone();
        let current_label: Option<SharedString> = current
            .as_deref()
            .map(|p| format!("Add {}", leaf_name(p)).into());
        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(t.main_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .font_family(ui)
            .text_size(px(t.ui_font_size + 1.0))
            .child(
                svg()
                    .path("icons/bot.svg")
                    .size(px(40.0))
                    .text_color(t.line_number),
            )
            .child(
                div()
                    .text_size(px(t.ui_font_size + 4.0))
                    .text_color(t.text)
                    .child("Agents"),
            )
            .child(
                div()
                    .max_w(px(460.0))
                    .text_center()
                    .text_color(t.line_number)
                    .child(SharedString::from(format!(
                        "A project is a folder you work in. Each project has worktrees — its own \
                         folder, plus any git worktrees you create — and each worktree runs one \
                         or more tabs that start with `{}`.",
                        self.agents.cfg.default_command()
                    ))),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .pt_2()
                    .when_some(current.zip(current_label), |d, (p, label)| {
                        d.child(btn_primary("agents-add-current", label).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                this.agents_add_space(p.clone(), cx);
                            }),
                        ))
                    })
                    .child(
                        btn_secondary("agents-add-folder", "Add Folder…").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.agents_add_space_prompt(cx)),
                        ),
                    )
                    .child(
                        btn_secondary("agents-new-scratch-welcome", "New Scratch Tab")
                            .tooltip(ui::tip(
                                "An agent in a throwaway folder, for anything ad hoc",
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _e, _w, cx| this.agents_new_scratch(cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The spaces sidebar: each space (its own folder = default worktree, with its branch)
    /// and its linked worktrees beneath, tree-style, each with a status dot.
    fn render_agents_sidebar(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let fs = t.ui_font_size;
        let tab = self.agents.side_tab;
        // Each tab's badge: issues / PRs across every project.
        let total = |which: SideTab| -> usize {
            self.agents
                .cfg
                .spaces
                .iter()
                .filter(|s| which == SideTab::Code || self.agents_in_scope(&s.path))
                .map(|s| match which {
                    SideTab::Code => 0,
                    SideTab::Issues => match self.agents.issues.get(&s.path) {
                        Some(GhList::Ready(v)) => v.len(),
                        _ => 0,
                    },
                    SideTab::Prs => match self.agents.prs.get(&s.path) {
                        Some(GhList::Ready(v)) => v.len(),
                        _ => 0,
                    },
                    // What's running: the number worth a glance whatever the filter.
                    SideTab::Ci => self.agents_ci_running(&s.path),
                })
                .sum()
        };
        let tab_btn =
            |id: &'static str, label: &'static str, which: SideTab, cx: &mut Context<Self>| {
                tab_pill(id, label, total(which), tab == which)
                    .flex_none()
                    .px_2()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| this.agents_show_tab(which, cx)),
                    )
            };
        // Each tab's pill width (full label, short label), measured: a pill's padding +
        // border, its label, and its count badge when it has one.
        let pill_w = {
            let ts = cx.text_system().clone();
            let font = ts.resolve_font(&gpui::font(ui));
            let text_w = move |s: &str, size: f32| -> f32 {
                s.chars()
                    .map(|c| {
                        ts.advance(font, px(size), c)
                            .map_or(size * 0.6, |a| f32::from(a.width))
                    })
                    .sum()
            };
            move |label: &str, count: usize| {
                let badge = if count > 0 {
                    6.0 + 10.0 + text_w(&count.to_string(), 10.0)
                } else {
                    0.0
                };
                18.0 + text_w(label, fs) + badge
            }
        };
        let widths: Vec<(f32, f32)> = SIDE_TABS
            .iter()
            .map(|(which, _, full, short)| {
                let n = total(*which);
                (pill_w(full, n), pill_w(short, n))
            })
            .collect();
        let icon = |id: &'static str, path: &'static str, tip: SharedString| {
            ui::icon_btn(id, 22.0)
                .tooltip(ui::tip(tip))
                .child(svg().path(path).size(px(14.0)).text_color(t.line_number))
        };
        // Expand all / Collapse all in one toggle (shared by every tab's tree).
        let any_open = self.agents.cfg.spaces.iter().any(|s| !s.collapsed)
            || self.agents.cfg.groups.iter().any(|g| !g.collapsed);
        let fold_all = icon(
            "agents-fold-all",
            if any_open {
                "icons/chevrons-down-up.svg"
            } else {
                "icons/chevrons-up-down.svg"
            },
            if any_open {
                "Collapse all".into()
            } else {
                "Expand all".into()
            },
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _e, _w, cx| this.agents_set_all_collapsed(any_open, cx)),
        )
        .into_any_element();
        let tools: Vec<gpui::AnyElement> = if tab == SideTab::Code {
            vec![
                fold_all,
                icon(
                    "agents-cleanup",
                    "icons/eraser.svg",
                    "Clean up — close finished tabs, remove finished worktrees".into(),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.agents_cleanup_now(cx)),
                )
                .into_any_element(),
            ]
        } else {
            vec![
                fold_all,
                icon(
                    "agents-gh-refresh",
                    "icons/rotate-ccw.svg",
                    "Refresh".into(),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        let tab = this.agents.side_tab;
                        this.agents_refresh_gh(tab, false, cx);
                    }),
                )
                .into_any_element(),
            ]
        };
        // What room the tabs have beside the tools + the Agents-only toggle (22 px icons,
        // 2 px gaps, 6 px padding each side); the ones that don't fit go behind a ▾ menu.
        #[allow(clippy::cast_precision_loss)]
        let avail = self.agents.sidebar_w - 12.0 - (tools.len() + 1) as f32 * 24.0 - 4.0;
        let active = SIDE_TABS.iter().position(|t| t.0 == tab).unwrap_or(0);
        let (shown, short) = fit_tabs(avail, &widths, active, 2.0, 22.0);
        let hidden: Vec<SideTab> = (0..SIDE_TABS.len())
            .filter(|i| !shown.contains(i))
            .map(|i| SIDE_TABS[i].0)
            .collect();
        let more = (!hidden.is_empty()).then(|| {
            icon(
                "agents-tab-more",
                "icons/chevron-down.svg",
                "More tabs".into(),
            )
            .flex_none()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                    let at = gpui::point(e.position.x - px(8.0), e.position.y + px(12.0));
                    this.open_menu(at, MenuTarget::SideTabs(hidden.clone()), cx);
                }),
            )
        });
        // Code | Issues | Pull Requests | CI, this tab's tools, and the Agents-only toggle.
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_0p5()
            .h(px(36.0))
            .px_1p5()
            .flex_none()
            // The tabs give way (clipped) when the sidebar is narrow — the tools never do.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_0p5()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .children(shown.into_iter().map(|i| {
                        let (which, id, full, brief) = SIDE_TABS[i];
                        tab_btn(id, if short { brief } else { full }, which, cx)
                    }))
                    .children(more),
            )
            .children(tools)
            .child({
                let only = self.agents.cfg.agents_only;
                icon(
                    "agents-only",
                    if only {
                        "icons/minimize-2.svg"
                    } else {
                        "icons/maximize-2.svg"
                    },
                    if only {
                        "Show the full IDE".into()
                    } else {
                        "Agents only — hide the rest of the IDE".into()
                    },
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| this.agents_set_only(!only, cx)),
                )
            });
        let rows: Vec<gpui::AnyElement> = match tab {
            SideTab::Code => self.agents_code_rows(cx),
            other => self.agents_gh_tree(other, cx),
        };
        let note = self.agents.note.clone().map(|n| {
            div()
                .flex_none()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(t.divider)
                .text_size(px(fs - 1.0))
                .text_color(t.line_number)
                .child(SharedString::from(n))
        });
        div()
            .flex()
            .flex_col()
            .w(px(self.agents.sidebar_w))
            .flex_none()
            .h_full()
            .bg(t.panel_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .overflow_hidden()
            .font_family(ui)
            // Compact, herdr-dense: the UI size — the same as the agent terminals beside it.
            .text_size(px(t.ui_font_size))
            .text_color(t.text)
            .child(header)
            .when(tab != SideTab::Code, |d| {
                d.child(self.render_agents_gh_filters(cx))
            })
            .child(
                div()
                    .id("agents-spaces")
                    .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, _w, cx| {
                        this.agents_drag_move(e, cx);
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.agents_drag_drop(cx)),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            if this.agents.drag.take().is_some() {
                                cx.notify();
                            }
                        }),
                    )
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .px_1p5()
                    .pb_2()
                    .children(rows),
            )
            .when(tab == SideTab::Code, |d| {
                let link = |id: &'static str, icon: &'static str, label: &'static str| {
                    div()
                        .id(id)
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1p5()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .text_color(t.primary)
                        .hover(|d| d.bg(t.bg_mid))
                        .child(svg().path(icon).size(px(14.0)).text_color(t.primary))
                        .child(label)
                };
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .flex_none()
                        .justify_center()
                        .gap_1()
                        .py_2()
                        .child(
                            link("agents-add-space", "icons/plus.svg", "Add project")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _e, _w, cx| {
                                        this.agents_add_space_prompt(cx);
                                    }),
                                ),
                        )
                        .child(
                            link("agents-new-group", "icons/folder-plus.svg", "New group")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _e, window, cx| {
                                        this.agents_new_group(None, window, cx);
                                    }),
                                ),
                        ),
                )
            })
            .children(note)
            .into_any_element()
    }

    /// The Code tab: each group's header with (unfolded) its projects, then the ungrouped
    /// projects — each project's header, then (unfolded) its default worktree, its linked
    /// worktrees, and one still being checked out.
    fn agents_code_rows(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let cfg = &self.agents.cfg;
        let line = self.agents_drop_line();
        for (group, mut idxs) in cfg.sections() {
            let Some(g) = group else {
                self.agents_sort_spaces(&mut idxs);
                for si in idxs {
                    let space = &cfg.spaces[si];
                    // Scratch sits at the bottom, just above Add project: the list's spare
                    // room goes above it (none once the list scrolls).
                    if space.scratch {
                        rows.push(div().flex_1().into_any_element());
                    }
                    self.agents_code_space(si, space, line.as_ref(), &mut rows, cx);
                }
                continue;
            };
            if line == Some(DropLine::BeforeGroup(g.id)) {
                rows.push(drop_line_el());
            }
            let lit = line == Some(DropLine::IntoGroup(g.id));
            if g.collapsed {
                // Folded: its projects' most urgent dot, and how many projects hide under it.
                let state = AgentState::rollup(idxs.iter().flat_map(|&i| {
                    let s = &cfg.spaces[i];
                    std::iter::once(&s.path)
                        .chain(&s.worktrees)
                        .map(|w| self.agents_state(w))
                }));
                let folded = Some((Some(state), idxs.len()));
                rows.push(self.agents_group_row(g, folded, true, lit, cx));
            } else {
                rows.push(self.agents_group_row(g, None, true, lit, cx));
                self.agents_sort_spaces(&mut idxs);
                let mut block = Vec::new();
                for si in idxs {
                    self.agents_code_space(si, &cfg.spaces[si], line.as_ref(), &mut block, cx);
                }
                if block.is_empty() {
                    block.push(self.agents_group_hint(g.id, cx));
                }
                rows.push(group_block(block));
            }
            if line == Some(DropLine::AfterGroup(g.id)) {
                rows.push(drop_line_el());
            }
        }
        rows
    }

    /// Needs-you-first (when on): spaces (indices into the config's) by their most urgent
    /// worktree — stable, so ties keep your order. Scratch stays last either way.
    fn agents_sort_spaces(&self, idxs: &mut [usize]) {
        let sort = self.agents.cfg.sort_attention;
        let spaces = &self.agents.cfg.spaces;
        idxs.sort_by_key(|&i| {
            let s = &spaces[i];
            let urgency = if sort {
                std::iter::once(&s.path)
                    .chain(&s.worktrees)
                    .map(|w| self.agents_state(w).urgency())
                    .max()
                    .unwrap_or(0)
            } else {
                0
            };
            (s.scratch, std::cmp::Reverse(urgency))
        });
    }

    /// One project's rows on the Code tab: its header, then (unfolded) its worktrees and
    /// those still being checked out — with the drop `line` where it falls among them.
    fn agents_code_space(
        &self,
        si: usize,
        space: &Space,
        line: Option<&DropLine>,
        rows: &mut Vec<gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let sort = self.agents.cfg.sort_attention;
        let urgency = |w: &Path| self.agents_state(w).urgency();
        if line == Some(&DropLine::BeforeSpace(space.path.clone())) {
            rows.push(drop_line_el());
        }
        rows.push(self.agents_space_row(si, space, None, cx));
        let after_space = line == Some(&DropLine::AfterSpace(space.path.clone()));
        if space.collapsed {
            if after_space {
                rows.push(drop_line_el());
            }
            return;
        }
        // Worktrees still being checked out sit last, so the connectors stay right.
        let pending: Vec<&Creating> = self
            .agents
            .creating
            .iter()
            .filter(|c| c.space == space.path)
            .collect();
        // The space's own folder (its default worktree) among its linked ones.
        let ordered = space.rows();
        let n = ordered.len() + pending.len();
        if space.scratch && ordered.is_empty() {
            rows.push(self.agents_scratch_hint(cx));
        }
        let mut wts: Vec<(usize, &PathBuf)> = ordered.iter().enumerate().collect();
        if sort {
            wts.sort_by_key(|(_, w)| std::cmp::Reverse(urgency(w)));
        }
        for (pos, (wi, wt)) in wts.into_iter().enumerate() {
            if line == Some(&DropLine::BeforeRow(wt.clone())) {
                rows.push(drop_line_el());
            }
            rows.push(self.agents_worktree_row(si, wi, &space.path, wt, pos + 1 == n, cx));
            if line == Some(&DropLine::AfterRow(wt.clone())) {
                rows.push(drop_line_el());
            }
        }
        let shown = ordered.len();
        for (k, c) in pending.into_iter().enumerate() {
            let selected = self.agents.selected.as_ref() == Some(&c.path);
            let path = c.path.clone();
            rows.push(
                div()
                    .id(SharedString::from(format!(
                        "agents-pending-{}",
                        c.path.display()
                    )))
                    .rounded_md()
                    .when(selected, |d| d.bg(theme::get().selected_bg))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.agents.selected = Some(path.clone());
                            cx.notify();
                        }),
                    )
                    .child(agents_pending_row(c.name.clone(), shown + k + 1 == n))
                    .into_any_element(),
            );
        }
        if after_space {
            rows.push(drop_line_el());
        }
    }

    /// An empty, unfolded group's only row: where a dragged project can be dropped into it.
    fn agents_group_hint(&self, id: u32, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        div()
            .id(SharedString::from(format!("agents-group-empty-{id}")))
            .pl(px(ROW_PAD_X + CHEVRON_W + GAP))
            .py(px(ROW_PAD_Y))
            .h(px(TITLE_H + 2.0 * ROW_PAD_Y))
            .flex()
            .items_center()
            .text_size(px(t.ui_font_size - 1.0))
            .text_color(t.line_number)
            .on_mouse_move(cx.listener(move |this, _e: &gpui::MouseMoveEvent, _w, cx| {
                this.agents_drag_hover_group(id, cx);
            }))
            .child("Drag a project here")
            .into_any_element()
    }

    /// A group's header: fold chevron, folder glyph, name — folded, its projects' rolled-up
    /// status dot (`folded.0`) and how many items hide under it. On the Code tab (`code`) it
    /// drags to reorder the groups, takes projects dropped on it (`lit` while one is over
    /// it), and offers Add project / Remove group on hover.
    fn agents_group_row(
        &self,
        g: &kyde_config::agents::Group,
        folded: Option<(Option<AgentState>, usize)>,
        code: bool,
        lit: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let id = g.id;
        let grp = SharedString::from(format!("agents-group-{id}"));
        let renaming = self.agents.renaming == Some(RenameTarget::Group(id));
        let dragged = self
            .agents
            .drag
            .as_ref()
            .is_some_and(|d| d.active && d.item == DragItem::Group(id));
        let action = |key: &str, icon: &'static str, tip: &'static str| {
            ui::icon_btn(SharedString::from(format!("agents-group-{key}-{id}")), 20.0)
                .tooltip(ui::tip(tip))
                .child(svg().path(icon).size(px(13.0)).text_color(t.secondary_text))
        };
        let actions = div()
            .flex()
            .flex_row()
            .flex_none()
            .opacity(0.0)
            .group_hover(grp.clone(), |s| s.opacity(1.0))
            .child(
                action("add", "icons/plus.svg", "Add project to this group").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.agents_add_space_to_group_prompt(id, cx);
                    }),
                ),
            )
            .child(
                action("rm", "icons/x.svg", "Remove group (its projects stay)").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.agents_remove_group(id, cx);
                    }),
                ),
            );
        let folded = folded.filter(|(_, k)| *k > 0);
        div()
            .id(grp.clone())
            .debug_selector(move || format!("agents-group-{id}"))
            .group(grp)
            .flex()
            .flex_col()
            .pl(px(ROW_PAD_X))
            .pr_2()
            .py(px(ROW_PAD_Y + 1.0))
            .mt(px(4.0))
            .rounded_md()
            // Lit while a dragged project is over it (a transparent border otherwise, so the
            // row doesn't shift when it lights).
            .border_1()
            .border_color(gpui::transparent_black())
            .cursor_pointer()
            .hover(|d| d.bg(t.bg_mid))
            .when(lit, |d| d.bg(t.selected_bg).border_color(t.primary))
            // Drag a group to a new place (Code tab).
            .when(code, |d| {
                d.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &gpui::MouseDownEvent, _w, _cx| {
                        this.agents_drag_press(DragItem::Group(id), e);
                    }),
                )
            })
            .on_mouse_move(cx.listener(move |this, _e: &gpui::MouseMoveEvent, _w, cx| {
                this.agents_drag_hover_group(id, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                    this.open_menu(e.position, MenuTarget::AgentGroup(id), cx);
                }),
            )
            .when(dragged, |d| d.opacity(0.4))
            .child(
                title_line()
                    .child(
                        div()
                            .flex_none()
                            .w(px(CHEVRON_W))
                            .flex()
                            .items_center()
                            .child(
                                svg()
                                    .path(if g.collapsed {
                                        "icons/chevron-right.svg"
                                    } else {
                                        "icons/chevron-down.svg"
                                    })
                                    .size(px(12.0))
                                    .text_color(t.line_number),
                            ),
                    )
                    .child(
                        svg()
                            .flex_none()
                            .path("icons/folder.svg")
                            .size(px(13.0))
                            .text_color(t.line_number),
                    )
                    .child(if renaming {
                        self.agents_rename_field(cx)
                    } else {
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(SharedString::from(g.name.clone()))
                            .into_any_element()
                    })
                    .when_some(folded, |d, (state, n)| {
                        d.children(state.map(status_dot)).child(
                            div()
                                .flex_none()
                                .text_size(px(t.ui_font_size - 1.0))
                                .text_color(t.line_number)
                                .child(SharedString::from(n.to_string())),
                        )
                    })
                    .when(!renaming, |d| {
                        d.child(div().flex_1()).when(code, |d| d.child(actions))
                    }),
            )
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
                // The end of a drag isn't a click.
                if renaming || this.agents.drag.as_ref().is_some_and(|d| d.active) {
                    return;
                }
                this.agents_toggle_group(id, cx);
                // Double-click: the first click's fold is undone; rename instead.
                if e.click_count() >= 2 {
                    this.agents_start_rename(RenameTarget::Group(id), window, cx);
                }
            }))
            .into_any_element()
    }

    /// Scratch's only row while it has none: start one.
    fn agents_scratch_hint(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        div()
            .id("agents-scratch-new")
            .relative()
            .flex()
            .flex_col()
            .pl(px(WT_DOT_X))
            .pr_2()
            .py(px(ROW_PAD_Y))
            .rounded_md()
            .cursor_pointer()
            .text_color(t.line_number)
            .hover(|d| d.bg(t.bg_mid).text_color(t.secondary_text))
            .children(tree_connector(true))
            .child(
                title_line()
                    .child(
                        svg()
                            .flex_none()
                            .path("icons/plus.svg")
                            .size(px(DOT))
                            .text_color(t.line_number),
                    )
                    .child("New scratch tab"),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| this.agents_new_scratch(cx)),
            )
            .into_any_element()
    }

    /// The Issues / Pull Requests / CI tabs: the same project tree as Code, with each
    /// project's issues (or PRs, or runs) as its children.
    fn agents_gh_tree(&self, tab: SideTab, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let mut any_loading = false;
        let mut failed: Vec<String> = Vec::new();
        // CI filters as it renders: the search, lowercased once.
        let query = self.agents_gh_query(cx).to_lowercase();
        let cfg = &self.agents.cfg;
        for (group, idxs) in cfg.sections() {
            let folded = group.is_some_and(|g| g.collapsed);
            let (mut block, mut total) = (Vec::new(), 0);
            for si in idxs {
                let space = &cfg.spaces[si];
                if !self.agents.git_spaces.contains(&space.path)
                    || !self.agents_in_scope(&space.path)
                {
                    continue;
                }
                let (ready, loading, why) = match tab {
                    SideTab::Issues => gh_status(self.agents.issues.get(&space.path)),
                    SideTab::Ci => self.agents_ci_status(&space.path, &query),
                    _ => gh_status(self.agents.prs.get(&space.path)),
                };
                // Only projects with something to show (no empty / issues-disabled headers).
                if ready == 0 {
                    any_loading |= loading;
                    if let Some(why) = why {
                        failed.push(format!("{} — {why}", leaf_name(&space.path)));
                    }
                    continue;
                }
                total += ready;
                if folded {
                    continue;
                }
                block.push(self.agents_space_row(si, space, Some(ready), cx));
                if space.collapsed {
                    continue;
                }
                block.extend(match tab {
                    SideTab::Issues => self.agents_issue_rows(si, &space.path, cx),
                    SideTab::Ci => self.agents_ci_rows(si, &space.path, &query, cx),
                    _ => self.agents_pr_rows(si, &space.path, cx),
                });
            }
            match group {
                // Only groups with something to show; folded, how many items hide under it.
                Some(g) if total > 0 => {
                    let count = folded.then_some((None, total));
                    rows.push(self.agents_group_row(g, count, false, false, cx));
                    if !folded {
                        rows.push(group_block(block));
                    }
                }
                Some(_) => {}
                None => rows.extend(block),
            }
        }
        if rows.is_empty() {
            rows.push(gh_note(if any_loading || self.agents.git_refreshing {
                "Loading…"
            } else if tab == SideTab::Issues {
                "No open issues match."
            } else if tab == SideTab::Ci {
                "No workflow runs match."
            } else {
                "No open pull requests match."
            }));
            // Why the hidden projects came back empty-handed (gh missing, not logged in…).
            if !any_loading && !self.agents.git_refreshing {
                rows.extend(failed.into_iter().map(gh_note));
            }
        }
        rows
    }

    /// The Issues tab: issues assigned to you in `space`'s repo — one click makes (or opens)
    /// the issue's worktree with its agent started on it.
    fn agents_issue_rows(
        &self,
        si: usize,
        space: &Path,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let issues = match self.agents.issues.get(space) {
            None | Some(GhList::Loading) => return vec![gh_note("Loading issues…")],
            Some(GhList::Failed(why)) => return vec![gh_note(why.clone())],
            Some(GhList::Ready(v)) if v.is_empty() => {
                return vec![gh_note("No open issues assigned to you.")]
            }
            Some(GhList::Ready(v)) => v,
        };
        let len = issues.len();
        issues
            .iter()
            .enumerate()
            .map(|(i, issue)| {
                let open = self.agents_issue_worktree(space, issue.number).is_some();
                let folder =
                    crate::agents::issue_branch(issue.number, &issue.title, &issue.labels).0;
                let mut sub = format!("#{}", issue.number);
                if !issue.assignees.is_empty() {
                    let who: Vec<String> =
                        issue.assignees.iter().map(|a| format!("@{a}")).collect();
                    sub.push_str(" · ");
                    sub.push_str(&who.join(", "));
                }
                if !issue.labels.is_empty() {
                    sub.push_str(" · ");
                    sub.push_str(&issue.labels.join(", "));
                }
                let (sp, it) = (space.to_path_buf(), issue.clone());
                let (sel_space, n) = (space.to_path_buf(), issue.number);
                self.agents_gh_row(
                    ("agents-issue", si * 10_000 + i),
                    i + 1 == len,
                    self.agents.gh_selected.as_ref()
                        == Some(&(space.to_path_buf(), SideTab::Issues, n)),
                    None,
                    issue.title.clone(),
                    sub,
                    RowAct::starting(self.agents_action_label(space, &folder, open, "Start")),
                    move |this, cx| this.agents_start_issue(sp.clone(), it.clone(), cx),
                    move |this, cx| {
                        this.agents_select_gh(sel_space.clone(), SideTab::Issues, n, cx);
                    },
                    cx,
                )
            })
            .collect()
    }

    /// The Pull Requests tab: open PRs in `space`'s repo — one click checks one out into a
    /// review worktree (or opens it) with its agent started on the review.
    fn agents_pr_rows(
        &self,
        si: usize,
        space: &Path,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let prs = match self.agents.prs.get(space) {
            None | Some(GhList::Loading) => return vec![gh_note("Loading pull requests…")],
            Some(GhList::Failed(why)) => return vec![gh_note(why.clone())],
            Some(GhList::Ready(v)) if v.is_empty() => {
                return vec![gh_note("No open pull requests.")]
            }
            Some(GhList::Ready(v)) => v,
        };
        let len = prs.len();
        prs.iter()
            .enumerate()
            .map(|(i, pr)| {
                let open = self.agents_pr_worktree(space, pr).is_some();
                let folder = format!("pr-{}", pr.number);
                let mut sub = format!("#{} · {} · {}", pr.number, pr.author, pr.head);
                if pr.draft {
                    sub.push_str(" · draft");
                }
                let (sp, it) = (space.to_path_buf(), pr.clone());
                let (sel_space, n) = (space.to_path_buf(), pr.number);
                self.agents_gh_row(
                    ("agents-pr", si * 10_000 + i),
                    i + 1 == len,
                    self.agents.gh_selected.as_ref()
                        == Some(&(space.to_path_buf(), SideTab::Prs, n)),
                    None,
                    pr.title.clone(),
                    sub,
                    RowAct::starting(match self.agents_review_state(space, pr) {
                        ReviewState::InProgress(_) => Some("In progress"),
                        ReviewState::Done(_) => Some("Re-review"),
                        ReviewState::None => {
                            self.agents_action_label(space, &folder, open, "Review")
                        }
                    }),
                    move |this, cx| this.agents_start_review(sp.clone(), it.clone(), cx),
                    move |this, cx| this.agents_select_gh(sel_space.clone(), SideTab::Prs, n, cx),
                    cx,
                )
            })
            .collect()
    }

    /// An issue/PR row's button label: "Open" when its worktree exists, else `start`; `None`
    /// (disabled, "Starting…") while a worktree is being made.
    fn agents_action_label(
        &self,
        space: &Path,
        folder: &str,
        open: bool,
        start: &'static str,
    ) -> Option<&'static str> {
        if open {
            Some("Open")
        } else if self
            .agents
            .creating
            .iter()
            .any(|c| c.space == space && c.name == folder)
        {
            None
        } else {
            Some(start)
        }
    }

    /// One issue / PR / run row: an optional `lead` (a run's status glyph), title over
    /// `#n · …`, and the action button shown on hover (or disabled while it's under way).
    #[allow(clippy::too_many_arguments)]
    fn agents_gh_row(
        &self,
        id: (&'static str, usize),
        last: bool,
        selected: bool,
        lead: Option<gpui::AnyElement>,
        title: String,
        sub: String,
        action: RowAct,
        on_action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        on_select: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let key = SharedString::from(format!("{}-{}", id.0, id.1));
        let hovered = self.agents.hovered_row.as_ref() == Some(&key);
        let btn_id = SharedString::from(format!("{}-act-{}", id.0, id.1));
        let button = match action {
            RowAct::Hidden => None,
            RowAct::Go(_) if !hovered => None,
            RowAct::Go(label) => Some(
                btn_secondary(btn_id, label)
                    .px_2()
                    .py_0()
                    .h(px(TITLE_H))
                    .flex()
                    .items_center()
                    .flex_none()
                    .text_size(px(t.ui_font_size - 2.0))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            cx.stop_propagation();
                            on_action(this, cx);
                        }),
                    ),
            ),
            RowAct::Busy(label) => Some(
                btn_secondary_state(btn_id, label, true)
                    .px_2()
                    .py_0()
                    .h(px(TITLE_H))
                    .flex()
                    .items_center()
                    .flex_none()
                    .text_size(px(t.ui_font_size - 2.0)),
            ),
        };
        let sel = key.clone();
        div()
            .id(id)
            .debug_selector(move || sel.to_string())
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(GAP))
            // A tree child of its project, like a worktree row (no status dot).
            .pl(px(WT_DOT_X))
            .pr_2()
            .py(px(ROW_PAD_Y))
            .rounded_md()
            .cursor_pointer()
            .when(selected, |d| d.bg(t.selected_bg))
            .when(!selected, |d| d.hover(|d| d.bg(t.bg_mid)))
            .on_hover(cx.listener(move |this, over: &bool, _w, cx| {
                this.agents_hover_row(&key, *over, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| on_select(this, cx)),
            )
            .children(tree_connector(last))
            // Title over number · who · labels; the action centred beside both.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        title_line()
                            .children(lead)
                            .child(div().flex_1().min_w_0().truncate().child(title)),
                    )
                    .child(sub_line(sub.into(), selected)),
            )
            .children(button)
            .into_any_element()
    }

    /// Track which sidebar row the mouse is over (its buttons show only then).
    fn agents_hover_row(&mut self, key: &SharedString, over: bool, cx: &mut Context<Self>) {
        if over {
            self.agents.hovered_row = Some(key.clone());
        } else if self.agents.hovered_row.as_ref() == Some(key) {
            self.agents.hovered_row = None;
        } else {
            return;
        }
        cx.notify();
    }

    /// Show issue / PR `n` of `space` in the main panel, loading it in full with `gh` — and,
    /// for a PR, its reviews and inline review threads.
    pub(crate) fn agents_select_gh(
        &mut self,
        space: PathBuf,
        tab: SideTab,
        n: u64,
        cx: &mut Context<Self>,
    ) {
        if tab == SideTab::Ci {
            self.agents_select_run(space, n, cx);
            return;
        }
        let key = (space.clone(), tab, n);
        if self.agents.gh_selected.as_ref() == Some(&key) {
            return;
        }
        self.agents.gh_selected = Some(key.clone());
        self.agents.gh_detail = None;
        self.agents.gh_cards.clear();
        self.agents.gh_unresolved.clear();
        self.agents.gh_diff = None;
        self.agents.gh_reply_to = None;
        self.agents.merge_armed = false;
        if tab == SideTab::Prs && self.agents.gh_pr_tab == PrTab::Files {
            self.agents_fetch_pr_diff(space.clone(), n, cx);
        }
        let need_merge = tab == SideTab::Prs && !self.agents.gh_merge.contains_key(&space);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (r, merge) = cx
                .background_executor()
                .spawn(async move {
                    use kyde_git::github as gh;
                    let merge = need_merge.then(|| gh::repo_merge(&space).ok()).flatten();
                    let r = if tab == SideTab::Prs {
                        let reviews = gh::reviews(&space, n).unwrap_or_default();
                        gh::pr_detail(&space, n).map(|d| (d, reviews))
                    } else {
                        gh::issue_detail(&space, n).map(|d| (d, Default::default()))
                    };
                    (r, merge)
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(m) = merge {
                    this.agents.gh_merge.insert(key.0.clone(), m);
                }
                // A newer pick supersedes this one.
                if this.agents.gh_selected.as_ref() != Some(&key) {
                    return;
                }
                match r {
                    Ok((d, (reviews, threads))) => {
                        // The lists are open-only: one that merged / closed since it was
                        // fetched leaves the list (its detail stays, showing why).
                        if !d.state.eq_ignore_ascii_case("open") {
                            this.agents_drop_from_list(&key.0, tab, n);
                        }
                        this.agents.gh_cards = this.agents_build_cards(&d, reviews, threads, cx);
                        this.agents.gh_detail = Some(Ok(d));
                    }
                    Err(e) => {
                        this.agents.gh_detail =
                            Some(Err(crate::agents::git_reason(&e.to_string()).to_string()));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Take issue / PR `n` out of `space`'s cached open list.
    fn agents_drop_from_list(&mut self, space: &Path, tab: SideTab, n: u64) {
        match tab {
            SideTab::Code | SideTab::Ci => {}
            SideTab::Issues => {
                if let Some(GhList::Ready(v)) = self.agents.issues.get_mut(space) {
                    v.retain(|i| i.number != n);
                }
            }
            SideTab::Prs => {
                if let Some(GhList::Ready(v)) = self.agents.prs.get_mut(space) {
                    v.retain(|p| p.number != n);
                }
            }
        }
    }

    /// Reload the selected issue / PR (after commenting, closing, replying).
    fn agents_reload_gh(&mut self, cx: &mut Context<Self>) {
        if let Some((space, tab, n)) = self.agents.gh_selected.take() {
            self.agents_select_gh(space, tab, n, cx);
        }
    }

    /// The conversation, GitHub-style: the description, then comments, reviews (with their
    /// inline threads) and pushed commits in time order.
    fn agents_build_cards(
        &mut self,
        d: &kyde_git::github::ItemDetail,
        reviews: Vec<kyde_git::github::Review>,
        threads: Vec<kyde_git::github::ReviewThread>,
        cx: &mut Context<Self>,
    ) -> Vec<GhCard> {
        let md = |text: &str, cx: &mut Context<Self>| {
            let text = text.to_string();
            cx.new(|cx| mdview::MarkdownView::new(&text, None, cx))
        };
        let description = if d.body.trim().is_empty() {
            "*No description provided.*"
        } else {
            d.body.as_str()
        };
        self.agents
            .gh_body
            .update(cx, |v, cx| v.set_text(description, None, cx));
        let mut timeline: Vec<(String, GhCard)> = Vec::new();
        for (author, at, body, bot) in &d.comments {
            timeline.push((
                at.clone(),
                GhCard::Comment {
                    author: author.clone(),
                    at: at.clone(),
                    verb: "commented",
                    body: md(body, cx),
                    raw: (!bot).then(|| body.clone()),
                },
            ));
        }
        // What Action comments hands over: every thread still open on GitHub.
        self.agents.gh_unresolved = threads
            .iter()
            .filter(|t| t.resolved == Some(false))
            .map(|t| crate::agents::Feedback {
                path: Some(t.path.clone()),
                line: t.line,
                context: Vec::new(),
                comments: t
                    .comments
                    .iter()
                    .map(|(a, _, b)| (a.clone(), b.clone()))
                    .collect(),
            })
            .collect();
        let mut threads: Vec<Option<kyde_git::github::ReviewThread>> =
            threads.into_iter().map(Some).collect();
        let thread_view = |t: kyde_git::github::ReviewThread, cx: &mut Context<Self>| {
            let rows: Vec<_> = kyde_git::github::parse_diff(&t.hunk)
                .into_iter()
                .filter(|r| !matches!(r, kyde_git::github::DiffRow::Hunk(_)))
                .collect();
            let tail = rows.len().saturating_sub(4);
            GhThread {
                root: t.root,
                path: t.path,
                line: t.line,
                resolved: t.resolved,
                hunk: rows[tail..].to_vec(),
                comments: t
                    .comments
                    .into_iter()
                    .map(|(a, at, b)| {
                        let view = md(&b, cx);
                        (a, at, view, b)
                    })
                    .collect(),
            }
        };
        for r in reviews {
            let mine: Vec<GhThread> = threads
                .iter_mut()
                .filter(|t| t.as_ref().is_some_and(|t| t.review == r.id))
                .filter_map(Option::take)
                .map(|t| thread_view(t, cx))
                .collect();
            // A bare "commented" review is just the container for its threads.
            if r.body.trim().is_empty() && mine.is_empty() && r.state == "COMMENTED" {
                continue;
            }
            let body = (!r.body.trim().is_empty()).then(|| md(&r.body, cx));
            timeline.push((
                r.at.clone(),
                GhCard::Review {
                    author: r.author,
                    at: r.at,
                    state: r.state,
                    body,
                    raw: r.body,
                    threads: mine,
                },
            ));
        }
        // Threads whose review we didn't get still show, as their own entries.
        for t in threads.into_iter().flatten() {
            let (author, at) = t
                .comments
                .first()
                .map(|(a, at, _)| (a.clone(), at.clone()))
                .unwrap_or_default();
            let view = thread_view(t, cx);
            timeline.push((
                at.clone(),
                GhCard::Review {
                    author,
                    at,
                    state: "COMMENTED".into(),
                    body: None,
                    raw: String::new(),
                    threads: vec![view],
                },
            ));
        }
        if let Some(pr) = &d.pr {
            for c in &pr.commits {
                timeline.push((
                    c.date.clone(),
                    GhCard::Commit {
                        oid: c.oid.clone(),
                        headline: c.headline.clone(),
                        author: c.author.clone(),
                        at: c.date.clone(),
                    },
                ));
            }
        }
        // ISO-8601 UTC times sort as text.
        timeline.sort_by(|a, b| a.0.cmp(&b.0));
        let mut cards = vec![GhCard::Comment {
            author: d.author.clone(),
            at: d.created_at.clone(),
            verb: if d.pr.is_some() {
                "commented"
            } else {
                "opened"
            },
            body: self.agents.gh_body.clone(),
            raw: None,
        }];
        cards.extend(timeline.into_iter().map(|(_, c)| c));
        cards
    }

    /// Post the comment box (or a thread reply), close / reopen the issue, or approve / merge
    /// the PR, then reload.
    fn agents_gh_post(&mut self, post: GhPost, cx: &mut Context<Self>) {
        let Some((space, tab, n)) = self.agents.gh_selected.clone() else {
            return;
        };
        let text = match post {
            GhPost::Reply(_) => self.agents.gh_reply.read(cx).text().trim().to_string(),
            _ => self.agents.gh_comment.read(cx).text().trim().to_string(),
        };
        if matches!(post, GhPost::Comment | GhPost::Reply(_)) && text.is_empty() {
            return;
        }
        // The head the merge box showed — a commit pushed since then stops the merge.
        let head = match &self.agents.gh_detail {
            Some(Ok(d)) => {
                d.pr.as_ref()
                    .map(|p| p.head_oid.clone())
                    .unwrap_or_default()
            }
            _ => String::new(),
        };
        // Merge stays armed while it's sent ("Merging…"); any other post disarms it.
        self.agents.merge_armed = matches!(post, GhPost::Merge(_));
        self.agents.gh_posting = true;
        cx.notify();
        let pr = tab == SideTab::Prs;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    use kyde_git::github as gh;
                    match post {
                        GhPost::Comment => gh::comment(&space, pr, n, &text),
                        GhPost::Reply(root) => gh::reply_to_thread(&space, n, root, &text),
                        // A comment typed before closing goes in first, like GitHub's
                        // "Close with comment".
                        GhPost::Close => {
                            if text.is_empty() {
                                gh::close_issue(&space, n)
                            } else {
                                gh::comment(&space, false, n, &text)
                                    .and_then(|()| gh::close_issue(&space, n))
                            }
                        }
                        GhPost::Reopen => gh::reopen_issue(&space, n),
                        GhPost::Approve => gh::approve(&space, n, &text),
                        GhPost::Merge(method) => gh::merge(&space, n, method, &head),
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.agents.gh_posting = false;
                this.agents.merge_armed = false;
                match res {
                    Ok(()) => {
                        let clear = |e: &mut CodeEditor, cx: &mut Context<CodeEditor>| {
                            e.set_content(String::new(), Lang::PlainText, cx);
                        };
                        match post {
                            GhPost::Reply(_) => {
                                this.agents.gh_reply.update(cx, clear);
                                this.agents.gh_reply_to = None;
                            }
                            // A merge leaves the comment box alone (it wasn't sent).
                            GhPost::Merge(_) => {}
                            _ => this.agents.gh_comment.update(cx, clear),
                        }
                        match post {
                            GhPost::Close | GhPost::Reopen => {
                                this.agents_refresh_gh(SideTab::Issues, true, cx);
                            }
                            GhPost::Merge(_) => this.agents_refresh_gh(SideTab::Prs, true, cx),
                            _ => {}
                        }
                        this.agents_reload_gh(cx);
                    }
                    Err(e) => {
                        this.agents.note = Some(format!(
                            "GitHub said no: {}",
                            crate::agents::git_reason(&e.to_string())
                        ));
                        // A refused merge (a push since, a rule) — show where it stands now.
                        if matches!(post, GhPost::Merge(_)) {
                            this.agents_reload_gh(cx);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The Issues / Pull Requests tabs' main panel, laid out like GitHub: title + number, a
    /// state pill and who-wants-what, assignees / reviewers / labels, the actions — then the
    /// conversation (and, for a PR, Commits / Checks / Files changed).
    fn render_agents_gh_detail(
        &self,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let fs = t.ui_font_size;
        let tab = self.agents.side_tab;
        if tab == SideTab::Ci {
            return self.render_agents_ci_detail(ui, cx);
        }
        let picked = self
            .agents
            .gh_selected
            .clone()
            .filter(|(_, k, _)| *k == tab);
        let Some((space, _, n)) = picked else {
            return ui::empty_state(
                if tab == SideTab::Prs {
                    "Select a pull request"
                } else {
                    "Select an issue"
                },
                ui,
            )
            .into_any_element();
        };
        let d = match &self.agents.gh_detail {
            None => return ui::empty_state("Loading…", ui).into_any_element(),
            Some(Err(why)) => return ui::empty_state(why.clone(), ui).into_any_element(),
            Some(Ok(d)) => d.clone(),
        };
        let is_pr = d.pr.is_some();
        // The same one-click action as the list row.
        let action: Option<gpui::AnyElement> = if is_pr {
            self.agents
                .prs
                .get(&space)
                .and_then(|l| match l {
                    GhList::Ready(v) => v.iter().find(|p| p.number == n).cloned(),
                    _ => None,
                })
                .map(|pr| {
                    let label = match self.agents_review_state(&space, &pr) {
                        ReviewState::None => "Review",
                        ReviewState::InProgress(_) => "Review in progress →",
                        ReviewState::Done(_) => "Re-review",
                    };
                    let sp = space.clone();
                    btn_primary("agents-gh-act", label)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                this.agents_start_review(sp.clone(), pr.clone(), cx);
                            }),
                        )
                        .into_any_element()
                })
        } else {
            self.agents
                .issues
                .get(&space)
                .and_then(|l| match l {
                    GhList::Ready(v) => v.iter().find(|i| i.number == n).cloned(),
                    _ => None,
                })
                .map(|issue| {
                    let open = self.agents_issue_worktree(&space, n).is_some();
                    let sp = space.clone();
                    btn_primary(
                        "agents-gh-act",
                        if open { "Open worktree" } else { "Start" },
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.agents_start_issue(sp.clone(), issue.clone(), cx);
                        }),
                    )
                    .into_any_element()
                })
        };
        // Every unresolved review thread, to the agent in the PR's worktree.
        #[cfg(feature = "terminal")]
        let action_all = (is_pr
            && d.state.eq_ignore_ascii_case("open")
            && !self.agents.gh_unresolved.is_empty())
        .then(|| {
            let count = self.agents.gh_unresolved.len();
            btn_primary("agents-gh-action-all", "Action comments")
                .tooltip(ui::tip(format!(
                    "Hand the {count} unresolved review thread{} to an agent in this PR's \
                     worktree",
                    if count == 1 { "" } else { "s" }
                )))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.agents_action_unresolved(cx)),
                )
                .into_any_element()
        });
        #[cfg(not(feature = "terminal"))]
        let action_all: Option<gpui::AnyElement> = None;
        let url = d.url.clone();
        let state = d.state.to_uppercase();
        let (pill_label, pill_color) = match (is_pr, state.as_str()) {
            (_, "OPEN") => ("Open", t.status_added),
            (true, "MERGED") => ("Merged", t.primary),
            _ => ("Closed", t.status_conflict),
        };
        let pill = div()
            .flex_none()
            .px_2p5()
            .py_0p5()
            .rounded_full()
            .bg(pill_color)
            .text_color(t.primary_text)
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(pill_label);
        let who: SharedString = match &d.pr {
            Some(pr) => format!(
                "{} wants to merge {} commit{} into {} from {}",
                d.author,
                pr.commits.len(),
                if pr.commits.len() == 1 { "" } else { "s" },
                pr.base,
                pr.head
            )
            .into(),
            None => format!(
                "{} opened {} · {} comment{}",
                d.author,
                crate::agents::iso_ago(&d.created_at),
                d.comments.len(),
                if d.comments.len() == 1 { "" } else { "s" }
            )
            .into(),
        };
        let people = |label: &'static str, names: &[String]| {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap_1p5()
                .child(div().text_color(t.line_number).child(label))
                .when(names.is_empty(), |d| {
                    d.child(div().text_color(t.secondary_text).child("No one"))
                })
                .children(names.iter().map(|n| {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(avatar(n))
                        .child(SharedString::from(n.clone()))
                }))
        };
        let header =
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_4()
                .flex_none()
                .border_b_1()
                .border_color(t.divider)
                .child({
                    // "Title #123", the number muted, wrapping as one paragraph like GitHub.
                    let text = format!("{} #{}", d.title, d.number);
                    let muted = gpui::HighlightStyle {
                        color: Some(t.line_number.into()),
                        ..Default::default()
                    };
                    div().text_size(px(fs + 7.0)).child(
                        gpui::StyledText::new(text.clone())
                            .with_highlights([(d.title.len() + 1..text.len(), muted)]),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(pill)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(t.secondary_text)
                                .child(who),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_4()
                        .text_size(px(fs - 1.0))
                        .child(people("Assignees", &d.assignees))
                        .when_some(d.pr.as_ref(), |el, pr| {
                            el.child(people("Reviewers", &pr.review_requests))
                        }),
                )
                .when(!d.labels.is_empty(), |el| {
                    el.child(div().flex().flex_row().flex_wrap().gap_1().children(
                        d.labels.iter().map(|l| {
                            div()
                                .px_2()
                                .rounded_full()
                                .border_1()
                                .border_color(t.divider)
                                .text_size(px(fs - 1.0))
                                .text_color(t.secondary_text)
                                .child(SharedString::from(l.clone()))
                        }),
                    ))
                })
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .pt_1()
                        .children(action)
                        .children(action_all)
                        .child(
                            btn_secondary("agents-gh-web", "Open on GitHub").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |_this, _e, _w, cx| cx.open_url(&url)),
                            ),
                        ),
                );
        // A PR: Conversation · Commits · Checks · Files changed.
        let pr_tabs = d.pr.as_ref().map(|pr| {
            let current = self.agents.gh_pr_tab;
            let conv = 1 + d.comments.len();
            let tabs: [(&'static str, &'static str, usize, PrTab); 4] = [
                ("gh-t-conv", "Conversation", conv, PrTab::Conversation),
                ("gh-t-commits", "Commits", pr.commits.len(), PrTab::Commits),
                ("gh-t-checks", "Checks", pr.checks.len(), PrTab::Checks),
                ("gh-t-files", "Files changed", pr.files.len(), PrTab::Files),
            ];
            div()
                .flex()
                .flex_row()
                .gap_1()
                .px_3()
                .py_1p5()
                .flex_none()
                .border_b_1()
                .border_color(t.divider)
                .children(tabs.into_iter().map(|(id, label, count, which)| {
                    let sp = space.clone();
                    tab_pill(id, label, count, current == which).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.agents.gh_pr_tab = which;
                            if which == PrTab::Files && this.agents.gh_diff.is_none() {
                                this.agents_fetch_pr_diff(sp.clone(), n, cx);
                            }
                            cx.notify();
                        }),
                    )
                }))
        });
        let body = match (&d.pr, self.agents.gh_pr_tab) {
            (Some(_), PrTab::Files) => self.render_pr_diff(),
            (Some(pr), PrTab::Commits) => scroll_list(
                "gh-commits",
                pr.commits
                    .iter()
                    .map(|c| {
                        div()
                            .flex()
                            .flex_col()
                            .px_4()
                            .py_2()
                            .border_b_1()
                            .border_color(t.divider)
                            .child(
                                div()
                                    .truncate()
                                    .child(SharedString::from(c.headline.clone())),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1p5()
                                    .text_size(px(fs - 1.0))
                                    .text_color(t.line_number)
                                    .child(avatar(&c.author))
                                    .child(SharedString::from(format!(
                                        "{} committed {} · {}",
                                        c.author,
                                        crate::agents::iso_ago(&c.date),
                                        c.oid.get(..7).unwrap_or(&c.oid)
                                    ))),
                            )
                            .into_any_element()
                    })
                    .collect(),
            ),
            (Some(pr), PrTab::Checks) => scroll_list(
                "gh-checks",
                pr.checks
                    .iter()
                    .enumerate()
                    .map(|(i, (name, result, link))| {
                        let (glyph, color) = check_outcome(result).glyph();
                        let link = link.clone();
                        div()
                            .id(("gh-check", i))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_4()
                            .py_2()
                            .border_b_1()
                            .border_color(t.divider)
                            .cursor_pointer()
                            .hover(|d| d.bg(t.bg_mid))
                            .child(div().text_color(color).child(glyph))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(name.clone())),
                            )
                            .child(
                                div()
                                    .text_size(px(fs - 1.0))
                                    .text_color(t.line_number)
                                    .child(SharedString::from(result.to_lowercase())),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |_this, _e, _w, cx| {
                                    if !link.is_empty() {
                                        cx.open_url(&link);
                                    }
                                }),
                            )
                            .into_any_element()
                    })
                    .collect(),
            ),
            _ => self.render_gh_conversation(&d, cx),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(t.main_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .overflow_hidden()
            .font_family(ui)
            .text_size(px(fs))
            .text_color(t.text)
            .child(header)
            .children(pr_tabs)
            .child(body)
            .into_any_element()
    }

    /// The conversation: description, comments, reviews with their inline threads, commits —
    /// then the comment box (and, for an issue, Close / Reopen).
    fn render_gh_conversation(
        &self,
        d: &kyde_git::github::ItemDetail,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let fs = t.ui_font_size;
        let card_head = |author: &str, rest: String| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_1p5()
                .bg(t.bg_mid)
                .border_b_1()
                .border_color(t.divider)
                .child(avatar(author))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(SharedString::from(author.to_string())),
                )
                .child(
                    div()
                        .text_color(t.line_number)
                        .child(SharedString::from(rest)),
                )
        };
        let card = || {
            div()
                .flex()
                .flex_col()
                .border_1()
                .border_color(t.divider)
                .rounded_md()
                .overflow_hidden()
        };
        let open = d.state.eq_ignore_ascii_case("open");
        // An open PR's comments can each go to an agent (Action comment).
        let actionable = cfg!(feature = "terminal") && d.pr.is_some() && open;
        let mut items: Vec<gpui::AnyElement> = Vec::new();
        for (ci, c) in self.agents.gh_cards.iter().enumerate() {
            match c {
                GhCard::Comment {
                    author,
                    at,
                    verb,
                    body,
                    raw,
                } => {
                    let head = card_head(author, format!("{verb} {}", crate::agents::iso_ago(at)));
                    #[cfg(feature = "terminal")]
                    let head = head.when(actionable && raw.is_some(), |h| {
                        h.child(div().flex_1()).child(action_comment_btn(
                            ("gh-act-c", ci),
                            FeedbackAt::Card(ci),
                            cx,
                        ))
                    });
                    #[cfg(not(feature = "terminal"))]
                    let _ = raw;
                    items.push(
                        card()
                            .child(head)
                            .child(div().px_1().py_2().child(body.clone()))
                            .into_any_element(),
                    );
                }
                GhCard::Commit {
                    oid,
                    headline,
                    author,
                    at,
                } => items.push(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .text_size(px(fs - 1.0))
                        .text_color(t.line_number)
                        .child(div().child("●"))
                        .child(avatar(author))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(theme::font::family())
                                .text_color(t.secondary_text)
                                .child(SharedString::from(headline.clone())),
                        )
                        .child(SharedString::from(format!(
                            "{} · {}",
                            oid.get(..7).unwrap_or(oid),
                            crate::agents::iso_ago(at)
                        )))
                        .into_any_element(),
                ),
                GhCard::Review {
                    author,
                    at,
                    state,
                    body,
                    threads,
                    ..
                } => {
                    let (verb, color) = match state.as_str() {
                        "APPROVED" => ("approved these changes", t.status_added),
                        "CHANGES_REQUESTED" => ("requested changes", t.status_conflict),
                        "DISMISSED" => ("had a review dismissed", t.line_number),
                        _ => ("reviewed", t.line_number),
                    };
                    let mut review = div().flex().flex_col().gap_2().child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .child(div().text_color(color).child("◉"))
                            .child(avatar(author))
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(SharedString::from(author.clone())),
                            )
                            .child(div().text_color(t.line_number).child(SharedString::from(
                                format!("{verb} {}", crate::agents::iso_ago(at)),
                            ))),
                    );
                    if let Some(b) = body {
                        let head = card_head(author, "left a comment".into());
                        #[cfg(feature = "terminal")]
                        let head = head.when(actionable, |h| {
                            h.child(div().flex_1()).child(action_comment_btn(
                                ("gh-act-r", ci),
                                FeedbackAt::Card(ci),
                                cx,
                            ))
                        });
                        review = review.child(
                            card()
                                .child(head)
                                .child(div().px_1().py_2().child(b.clone())),
                        );
                    }
                    for (ti, th) in threads.iter().enumerate() {
                        review = review.child(self.render_gh_thread(ci, ti, th, actionable, cx));
                    }
                    items.push(review.pl_4().into_any_element());
                }
            }
        }
        if let Some(merge_box) =
            d.pr.as_ref()
                .filter(|_| open)
                .and_then(|pr| self.render_merge_box(pr, cx))
        {
            items.push(merge_box);
        }
        // The comment box.
        let posting = self.agents.gh_posting;
        let is_issue = d.pr.is_none();
        let closed = d.state.eq_ignore_ascii_case("closed");
        let mut buttons = div().flex().flex_row().justify_end().gap_2();
        if !is_issue && open {
            // What's typed in the box goes with the approval, as its comment.
            buttons = buttons.child(btn_secondary_state("gh-approve", "Approve", posting).when(
                !posting,
                |b| {
                    b.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.agents_gh_post(GhPost::Approve, cx)),
                    )
                },
            ));
        }
        if is_issue {
            buttons = buttons.child(
                btn_secondary_state(
                    "gh-close",
                    if closed {
                        "Reopen issue"
                    } else {
                        "Close issue"
                    },
                    posting,
                )
                .when(!posting, |b| {
                    b.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.agents_gh_post(
                                if closed {
                                    GhPost::Reopen
                                } else {
                                    GhPost::Close
                                },
                                cx,
                            );
                        }),
                    )
                }),
            );
        }
        buttons = buttons.child(
            btn_primary_state(
                "gh-comment",
                if posting { "Posting…" } else { "Comment" },
                posting,
            )
            .when(!posting, |b| {
                b.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| this.agents_gh_post(GhPost::Comment, cx)),
                )
            }),
        );
        items.push(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .pt_2()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Add a comment"),
                )
                .child(
                    div()
                        .h(px(96.0))
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(t.divider)
                        .bg(t.panel_bg)
                        // A comment longer than the box scrolls inside it.
                        .child(
                            div()
                                .id("gh-comment-scroll")
                                .size_full()
                                .overflow_y_scroll()
                                .track_scroll(&self.agents.gh_comment_scroll)
                                .child(self.agents.gh_comment.clone()),
                        ),
                )
                .child(buttons)
                .into_any_element(),
        );
        div()
            .id("agents-gh-conv")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            // Each card keeps its full height — the column scrolls instead of squashing them.
            .children(items.into_iter().map(|e| div().flex_none().child(e)))
            .into_any_element()
    }

    /// An open PR's merge box, like GitHub's: where its review, checks and conflicts stand,
    /// then the merge method and Merge — which asks to confirm before it merges. `None` when
    /// you can't merge into the repo.
    fn render_merge_box(
        &self,
        pr: &kyde_git::github::PrDetail,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        use kyde_git::github::MergeMethod;
        let t = theme::get();
        let space = &self.agents.gh_selected.as_ref()?.0;
        // Settings gh couldn't read: offer every method and let GitHub say no.
        let (methods, default) = match self.agents.gh_merge.get(space) {
            Some(m) if !m.can_merge => return None,
            Some(m) => (m.methods.clone(), m.default),
            None => (
                vec![MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase],
                None,
            ),
        };
        let method = self
            .agents
            .merge_method
            .filter(|m| methods.contains(m))
            .or(default)
            .or_else(|| methods.first().copied())?;
        let line = |(glyph, color): (&'static str, kyde_color::Color), text: SharedString| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().w(px(14.0)).flex_none().text_color(color).child(glyph))
                .child(text)
        };
        let mut lines = Vec::new();
        match pr.review_decision.as_str() {
            "APPROVED" => lines.push(line(CheckOutcome::Pass.glyph(), "Changes approved".into())),
            "CHANGES_REQUESTED" => {
                lines.push(line(CheckOutcome::Fail.glyph(), "Changes requested".into()));
            }
            "REVIEW_REQUIRED" => {
                lines.push(line(
                    CheckOutcome::Pending.glyph(),
                    "Review required".into(),
                ));
            }
            _ => {}
        }
        if !pr.checks.is_empty() {
            let count = |o: CheckOutcome| {
                pr.checks
                    .iter()
                    .filter(|(_, r, _)| check_outcome(r) == o)
                    .count()
            };
            let checks = |n: usize, what: &str| {
                format!("{n} {what} check{}", if n == 1 { "" } else { "s" }).into()
            };
            let (failing, pending) = (count(CheckOutcome::Fail), count(CheckOutcome::Pending));
            lines.push(if failing > 0 {
                line(CheckOutcome::Fail.glyph(), checks(failing, "failing"))
            } else if pending > 0 {
                line(CheckOutcome::Pending.glyph(), checks(pending, "pending"))
            } else {
                line(CheckOutcome::Pass.glyph(), "All checks have passed".into())
            });
        }
        let block = pr.merge_block();
        if let Some(why) = block {
            lines.push(line(CheckOutcome::Fail.glyph(), why.into()));
        } else if pr.merge_state == "BEHIND" {
            lines.push(line(
                CheckOutcome::Pending.glyph(),
                "This branch is out-of-date with the base branch".into(),
            ));
        } else if pr.mergeable == "MERGEABLE" {
            lines.push(line(
                CheckOutcome::Pass.glyph(),
                "No conflicts with base branch".into(),
            ));
        }

        let posting = self.agents.gh_posting;
        let armed = self.agents.merge_armed && block.is_none();
        let (option, verb) = merge_labels(method);
        let mut controls = div().flex().flex_row().items_center().gap_2();
        if methods.len() > 1 {
            let options: Vec<&'static str> = methods.iter().map(|&m| merge_labels(m).0).collect();
            let picked = methods.iter().position(|&m| m == method);
            let pick = methods.clone();
            controls = controls.child(ui::select(
                cx,
                "gh-merge-method",
                190.0,
                &options,
                picked,
                &self.agents.gh_merge_sel,
                |k: &mut Kyde| &mut k.agents.gh_merge_sel,
                |_this, _cx| {},
                move |this, i, _cx| {
                    this.agents.merge_method = pick.get(i).copied();
                    this.agents.merge_armed = false;
                },
            ));
        }
        let label: SharedString = if posting && armed {
            "Merging…".into()
        } else if armed {
            format!("Confirm {verb}").into()
        } else if methods.len() > 1 {
            "Merge".into()
        } else {
            option.into()
        };
        let disabled = posting || block.is_some();
        controls = controls.child(btn_primary_state("gh-merge", label, disabled).when(
            !disabled,
            |b| {
                b.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        // The first click arms it; the second merges (GitHub's Confirm merge).
                        if this.agents.merge_armed {
                            this.agents_gh_post(GhPost::Merge(method), cx);
                        } else {
                            this.agents.merge_armed = true;
                            cx.notify();
                        }
                    }),
                )
            },
        ));
        if armed && !posting {
            controls = controls.child(btn_secondary("gh-merge-cancel", "Cancel").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.agents.merge_armed = false;
                    cx.notify();
                }),
            ));
        }
        Some(
            div()
                .flex()
                .flex_col()
                .border_1()
                .border_color(t.divider)
                .rounded_md()
                .when(!lines.is_empty(), |el| {
                    el.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .px_3()
                            .py_2()
                            .border_b_1()
                            .border_color(t.divider)
                            .children(lines),
                    )
                })
                .child(div().px_3().py_2().child(controls))
                .into_any_element(),
        )
    }

    /// An inline review thread (card `ci`'s thread `ti`): the file, the diff lines it's on, its
    /// comments — each with Action comment when `actionable` — and a reply.
    fn render_gh_thread(
        &self,
        ci: usize,
        ti: usize,
        th: &GhThread,
        actionable: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        use kyde_git::github::DiffRow;
        let id = ci * 1000 + ti;
        let t = theme::get();
        let fs = t.ui_font_size;
        let lines = th.hunk.iter().map(|r| {
            let (text, bg) = match r {
                DiffRow::Add(l) => (format!("+ {l}"), Some(t.diff_inserted_bg)),
                DiffRow::Del(l) => (format!("− {l}"), Some(t.diff_deleted_bg)),
                DiffRow::Ctx(l) => (format!("  {l}"), None),
                DiffRow::Hunk(h) | DiffRow::File { path: h, .. } => (h.clone(), None),
            };
            div()
                .px_3()
                .font_family(theme::font::family())
                .text_size(px(fs - 1.0))
                .when_some(bg, gpui::Styled::bg)
                .child(
                    div()
                        .truncate()
                        .child(SharedString::from(text.replace('\t', "    "))),
                )
        });
        let mut comments = Vec::with_capacity(th.comments.len());
        for (k, (author, at, body, _)) in th.comments.iter().enumerate() {
            let head = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(avatar(author))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(SharedString::from(author.clone())),
                )
                .child(
                    div()
                        .text_color(t.line_number)
                        .child(SharedString::from(crate::agents::iso_ago(at))),
                );
            #[cfg(feature = "terminal")]
            let head = head.when(actionable, |h| {
                h.child(div().flex_1()).child(action_comment_btn(
                    ("gh-act-t", id * 1000 + k),
                    FeedbackAt::Thread(ci, ti, k),
                    cx,
                ))
            });
            #[cfg(not(feature = "terminal"))]
            let _ = (actionable, k);
            comments.push(
                div()
                    .flex()
                    .flex_col()
                    .px_3()
                    .pt_2()
                    .child(head)
                    .child(body.clone()),
            );
        }
        let root = th.root;
        let replying = self.agents.gh_reply_to == Some(root);
        let reply = if replying {
            let posting = self.agents.gh_posting;
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .p_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(t.divider)
                        .bg(t.main_bg)
                        .child(self.agents.gh_reply.clone()),
                )
                .child(
                    btn_secondary(("gh-reply-cancel", id), "Cancel").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.agents.gh_reply_to = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_primary_state(("gh-reply-send", id), "Reply", posting).when(
                        !posting,
                        |b| {
                            b.on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    this.agents_gh_post(GhPost::Reply(root), cx);
                                }),
                            )
                        },
                    ),
                )
                .into_any_element()
        } else {
            div()
                .id(("gh-reply", id))
                .m_2()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(t.divider)
                .cursor_pointer()
                .text_color(t.line_number)
                .hover(|d| d.bg(t.bg_mid))
                .child("Reply…")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, window, cx| {
                        this.agents.gh_reply_to = Some(root);
                        let focus = this.agents.gh_reply.read(cx).focus_handle.clone();
                        window.focus(&focus);
                        cx.notify();
                    }),
                )
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .border_1()
            .border_color(t.divider)
            .rounded_md()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .bg(t.bg_mid)
                    .border_b_1()
                    .border_color(t.divider)
                    .text_size(px(fs - 1.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme::font::family())
                            .child(SharedString::from(th.path.clone())),
                    )
                    .when(th.resolved == Some(true), |h| {
                        h.child(
                            div()
                                .flex_none()
                                .px_2()
                                .rounded_full()
                                .border_1()
                                .border_color(t.divider)
                                .text_color(t.line_number)
                                .child("Resolved"),
                        )
                    }),
            )
            .children(lines)
            .children(comments)
            .child(reply)
            .into_any_element()
    }

    /// Load PR `n`'s diff (`gh pr diff`) for the Files changed tab, in the background.
    fn agents_fetch_pr_diff(&mut self, space: PathBuf, n: u64, cx: &mut Context<Self>) {
        let key = (space.clone(), SideTab::Prs, n);
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    kyde_git::github::pr_diff(&space, n)
                        .map(|d| std::sync::Arc::new(kyde_git::github::parse_diff(&d)))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.agents.gh_selected.as_ref() == Some(&key) {
                    this.agents.gh_diff =
                        Some(r.map_err(|e| crate::agents::git_reason(&e.to_string()).to_string()));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The Files changed tab: the PR's whole diff, a header per file then its hunks' lines
    /// (+ green, − red) — virtualized, so a huge PR only lays out the rows on screen.
    fn render_pr_diff(&self) -> gpui::AnyElement {
        let t = theme::get();
        let rows = match &self.agents.gh_diff {
            None => return gh_note("Loading the changes…"),
            Some(Err(why)) => return gh_note(why.clone()),
            Some(Ok(rows)) => rows.clone(),
        };
        if rows.is_empty() {
            return gh_note("No changes.");
        }
        let count = rows.len();
        let fs = t.ui_font_size - 1.0;
        gpui::uniform_list("agents-pr-diff", count, move |range, _window, _cx| {
            let t = theme::get();
            range
                .map(|i| {
                    use kyde_git::github::DiffRow;
                    let row = div()
                        .h(px(fs * 1.6))
                        .flex()
                        .flex_row()
                        .items_center()
                        .px_3()
                        .text_size(px(fs))
                        .font_family(theme::font::family());
                    let text = |s: &str| SharedString::from(s.replace('\t', "    "));
                    match &rows[i] {
                        DiffRow::File {
                            path,
                            additions,
                            deletions,
                        } => row
                            .gap_2()
                            .bg(t.bg_mid)
                            .font_family(theme::font::ui_family())
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(path.clone())),
                            )
                            .child(
                                div()
                                    .text_color(t.status_added)
                                    .child(SharedString::from(format!("+{additions}"))),
                            )
                            .child(
                                div()
                                    .text_color(t.status_conflict)
                                    .child(SharedString::from(format!("−{deletions}"))),
                            ),
                        DiffRow::Hunk(h) => row
                            .text_color(t.line_number)
                            .child(div().truncate().child(text(h))),
                        DiffRow::Add(l) => row
                            .bg(t.diff_inserted_bg)
                            .child(div().truncate().child(text(&format!("+ {l}")))),
                        DiffRow::Del(l) => row
                            .bg(t.diff_deleted_bg)
                            .child(div().truncate().child(text(&format!("− {l}")))),
                        DiffRow::Ctx(l) => row
                            .text_color(t.secondary_text)
                            .child(div().truncate().child(text(&format!("  {l}")))),
                    }
                })
                .collect::<Vec<_>>()
        })
        .flex_1()
        .min_h_0()
        .into_any_element()
    }

    /// A space's header row: fold chevron + name (click toggles its worktrees); folded, a
    /// count and the rolled-up status dot of everything inside. Hover → new worktree / remove.
    fn agents_space_row(
        &self,
        si: usize,
        space: &Space,
        items: Option<usize>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let is_git = self.agents.git_spaces.contains(&space.path);
        let n = space.rows().len();
        let grp = SharedString::from(format!("agents-space-{si}"));
        let renaming = self.agents.renaming == Some(RenameTarget::Space(space.path.clone()));
        let (toggle, new_wt, rm) = (space.path.clone(), space.path.clone(), space.path.clone());
        let (drop_space, menu) = (space.path.clone(), space.path.clone());
        // Scratch is pinned to the bottom.
        let draggable = items.is_none() && !self.agents.cfg.sort_attention && !space.scratch;
        let dragged = self
            .agents
            .drag
            .as_ref()
            .is_some_and(|d| d.active && d.item == DragItem::Space(space.path.clone()));
        // `…`: jump to this project's issues / pull requests / CI (those tabs, narrowed to it).
        let more = space.path.clone();
        let jump = ui::icon_btn(("agents-space-more", si), 20.0)
            .debug_selector(move || format!("agents-space-more-{si}"))
            .tooltip(ui::tip("Issues, pull requests, CI and more"))
            .child(
                svg()
                    .path("icons/ellipsis.svg")
                    .size(px(13.0))
                    .text_color(t.secondary_text),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    let at = gpui::point(e.position.x - px(8.0), e.position.y + px(12.0));
                    this.open_menu(at, MenuTarget::AgentSpace(more.clone()), cx);
                }),
            );
        // Its `…` menu is open: the pointer has left the row, the buttons stay.
        let menu_open = self
            .context_menu
            .as_ref()
            .is_some_and(|m| matches!(&m.target, MenuTarget::AgentSpace(p) if *p == space.path));
        let actions = div()
            .flex()
            .flex_row()
            .flex_none()
            .opacity(if menu_open { 1.0 } else { 0.0 })
            .group_hover(grp.clone(), |s| s.opacity(1.0))
            .when(space.scratch, |d| {
                d.child(
                    ui::icon_btn(("agents-new-scratch", si), 20.0)
                        .tooltip(ui::tip("New scratch tab (in a folder of its own)"))
                        .child(
                            svg()
                                .path("icons/plus.svg")
                                .size(px(13.0))
                                .text_color(t.secondary_text),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| {
                                cx.stop_propagation();
                                this.agents_new_scratch(cx);
                            }),
                        ),
                )
            })
            .when(is_git, |d| {
                d.child(jump).child(
                    ui::icon_btn(("agents-new-wt", si), 20.0)
                        .tooltip(ui::tip("New worktree (from the latest main)"))
                        .child(
                            svg()
                                .path("icons/plus.svg")
                                .size(px(13.0))
                                .text_color(t.secondary_text),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                cx.stop_propagation();
                                this.agents_new_worktree(new_wt.clone(), cx);
                            }),
                        ),
                )
            })
            // Scratch is always there.
            .when(!space.scratch, |d| {
                d.child(
                    ui::icon_btn(("agents-rm-space", si), 20.0)
                        .tooltip(ui::tip("Remove project (the folder stays on disk)"))
                        .child(
                            svg()
                                .path("icons/x.svg")
                                .size(px(13.0))
                                .text_color(t.secondary_text),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                cx.stop_propagation();
                                this.agents_remove_space(&rm, cx);
                            }),
                        ),
                )
            });
        // Folded: how many items hide under it — its issues / PRs on those tabs, else its
        // worktrees with their rolled-up status dot.
        let folded = space
            .collapsed
            .then(|| match items {
                Some(k) => (None, k),
                None => (
                    Some(AgentState::rollup(
                        std::iter::once(&space.path)
                            .chain(&space.worktrees)
                            .map(|w| self.agents_state(w)),
                    )),
                    n,
                ),
            })
            .filter(|(_, k)| *k > 0);
        let selector = format!("agents-space-{}", leaf_name(&space.path));
        div()
            .id(("agents-space", si))
            .debug_selector(move || selector)
            .group(grp)
            .flex()
            .flex_col()
            .pl(px(ROW_PAD_X))
            .pr_2()
            .py(px(ROW_PAD_Y + 1.0))
            .mt(px(2.0))
            .rounded_md()
            .cursor_pointer()
            .hover(|d| d.bg(t.bg_mid))
            // Drag a project to a new place (Code tab, your own order).
            .when(draggable, |d| {
                let item = DragItem::Space(space.path.clone());
                d.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &gpui::MouseDownEvent, _w, _cx| {
                        this.agents_drag_press(item.clone(), e);
                    }),
                )
            })
            .on_mouse_move(cx.listener(move |this, _e: &gpui::MouseMoveEvent, _w, cx| {
                this.agents_drag_hover(&drop_space, None, cx);
            }))
            .when(dragged, |d| d.opacity(0.4))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                    this.open_menu(e.position, MenuTarget::AgentSpace(menu.clone()), cx);
                }),
            )
            .child(
                title_line()
                    .child(
                        div()
                            .flex_none()
                            .w(px(CHEVRON_W))
                            .flex()
                            .items_center()
                            .child(
                                svg()
                                    .path(if space.collapsed {
                                        "icons/chevron-right.svg"
                                    } else {
                                        "icons/chevron-down.svg"
                                    })
                                    .size(px(12.0))
                                    .text_color(t.line_number),
                            ),
                    )
                    .child(if renaming {
                        self.agents_rename_field(cx)
                    } else {
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(self.agents_space_name(space))
                            .into_any_element()
                    })
                    .when_some(folded, |d, (state, n)| {
                        d.children(state.map(status_dot)).child(
                            div()
                                .flex_none()
                                .text_size(px(t.ui_font_size - 1.0))
                                .text_color(t.line_number)
                                .child(SharedString::from(n.to_string())),
                        )
                    })
                    // New worktree / remove space belong to the Code tab. Renaming, the name
                    // field takes the row.
                    .when(!renaming, |d| {
                        d.child(div().flex_1())
                            .when(items.is_none(), |d| d.child(actions))
                    }),
            )
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
                // The end of a drag isn't a click.
                if renaming || this.agents.drag.as_ref().is_some_and(|d| d.active) {
                    return;
                }
                this.agents_toggle_space(&toggle, cx);
                // Double-click: the first click's fold is undone; rename instead.
                if e.click_count() >= 2 {
                    this.agents_start_rename(RenameTarget::Space(toggle.clone()), window, cx);
                }
            }))
            .into_any_element()
    }

    /// A worktree's line as its sidebar row draws it: the status dot, a branch glyph on the
    /// project's own checkout, `title`, then what needs saying — "needs input" / "done", how
    /// long ago it was active, its git badges (each per the row settings). `ns` + `id` keep
    /// the badges' element id unique where it's drawn.
    pub(crate) fn agents_wt_line(
        &self,
        ns: &'static str,
        space: &Path,
        wt: &Path,
        id: usize,
        title: gpui::AnyElement,
    ) -> gpui::Div {
        let t = theme::get();
        let fields = self.agents.cfg.row;
        let is_default = wt == space;
        let state = self.agents_state(wt);
        // "needs input" / "done", in the dot's colour.
        let status = (fields.status && !state.label().is_empty()).then(|| {
            div()
                .flex_none()
                .text_size(px(t.ui_font_size - 1.0))
                .text_color(if state == AgentState::Blocked {
                    t.status_conflict
                } else {
                    t.status_added
                })
                .child(state.label())
        });
        let activity = fields
            .activity
            .then(|| self.agents_last_active(wt))
            .flatten()
            .map(|at| {
                div()
                    .flex_none()
                    .text_size(px(t.ui_font_size - 1.0))
                    .text_color(t.line_number)
                    .child(SharedString::from(short_age(at.elapsed())))
            });
        let badges = fields
            .git
            .then(|| git_badges((ns, id), self.agents.git.get(wt)))
            .flatten();
        // One line: dot, title, then what needs saying (status, activity, git).
        title_line()
            .child(status_dot(state))
            .when(is_default, |d| {
                d.child(
                    svg()
                        .flex_none()
                        .path("icons/git-branch.svg")
                        .size(px(13.0))
                        .text_color(t.line_number),
                )
            })
            .child(title)
            // Pinned: the cleanup leaves it alone.
            .when(self.agents.cfg.pinned.contains(wt), |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("{ns}-pin-{id}")))
                        .flex_none()
                        .tooltip(ui::tip("Pinned — the cleanup leaves it alone"))
                        .child(
                            svg()
                                .path("icons/pin.svg")
                                .size(px(11.0))
                                .text_color(t.line_number),
                        ),
                )
            })
            .children(status)
            .children(activity)
            .children(badges)
    }

    /// A linked-worktree row under its space: tree connector, dot + name, its branch + git
    /// badges beneath; hover → remove.
    fn agents_worktree_row(
        &self,
        si: usize,
        wi: usize,
        space: &Path,
        wt: &Path,
        last: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let selected = self.agents.selected.as_deref() == Some(wt);
        let id = si * 10_000 + wi;
        let key = SharedString::from(format!("agents-wt-{id}"));
        let hovered = self.agents.hovered_row.as_ref() == Some(&key);
        let (sel, rm_space, rm_wt) = (wt.to_path_buf(), space.to_path_buf(), wt.to_path_buf());
        let menu_wt = wt.to_path_buf();
        // The space's own folder: titled by its branch, behind a branch glyph.
        let is_default = wt == space;
        let scratch = self.agents.cfg.is_scratch(wt);
        let title: SharedString = self.agents_wt_title(space, wt).into();

        let renaming = self.agents.renaming == Some(RenameTarget::Worktree(wt.to_path_buf()));
        let title_el = if renaming {
            self.agents_rename_field(cx)
        } else {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(title)
                .into_any_element()
        };
        let lines = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(self.agents_wt_line("agents-wt-git", space, wt, id, title_el));
        // Any row drags to a new place in its project (the checkout too).
        let drag_item = (!self.agents.cfg.sort_attention).then(|| DragItem::Worktree {
            space: space.to_path_buf(),
            wt: wt.to_path_buf(),
        });
        let dragged = self
            .agents
            .drag
            .as_ref()
            .is_some_and(|d| d.active && Some(&d.item) == drag_item.as_ref());
        let (hover_space, hover_wt) = (space.to_path_buf(), wt.to_path_buf());
        ui::picker::row(("agents-wt", id), selected, t.bg_mid)
            .debug_selector(|| format!("agents-wt-{}", leaf_name(wt)))
            .on_mouse_move(cx.listener(move |this, _e: &gpui::MouseMoveEvent, _w, cx| {
                this.agents_drag_hover(&hover_space, Some(&hover_wt), cx);
            }))
            .when(dragged, |d| d.opacity(0.4))
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .pl(px(WT_DOT_X))
            .pr_2()
            .py(px(ROW_PAD_Y))
            .on_hover(cx.listener(move |this, over: &bool, _w, cx| {
                this.agents_hover_row(&key, *over, cx);
            }))
            .children(tree_connector(last))
            .child(lines)
            // Remove: only while hovered — it takes room then, so the badges shift left
            // rather than being covered. The default worktree (`main`) has none.
            .when(!is_default && hovered, |d| {
                d.child(
                    ui::icon_btn(("agents-rm-wt", id), 18.0)
                        .tooltip(ui::tip(if scratch {
                            "Close (ends its agent)"
                        } else {
                            "Remove worktree…"
                        }))
                        .child(
                            svg()
                                .path("icons/x.svg")
                                .size(px(12.0))
                                .text_color(t.secondary_text),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                cx.stop_propagation();
                                // Nothing to lose in scratch: no git, and saved files stay.
                                if scratch {
                                    this.agents_close_scratch(&rm_wt, cx);
                                } else {
                                    this.agents_confirm_remove(rm_space.clone(), rm_wt.clone(), cx);
                                }
                            }),
                        ),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &gpui::MouseDownEvent, window, cx| {
                    if renaming {
                        return;
                    }
                    // Double-click: rename it — not the checkout, which is always its branch.
                    if e.click_count >= 2 && !is_default {
                        this.agents_start_rename(RenameTarget::Worktree(sel.clone()), window, cx);
                        return;
                    }
                    if let Some(item) = &drag_item {
                        this.agents_drag_press(item.clone(), e);
                    }
                    this.agents_select(sel.clone(), cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                    this.open_menu(e.position, MenuTarget::AgentRow(menu_wt.clone()), cx);
                }),
            )
            .into_any_element()
    }

    /// The selected worktree: its tab strip (with Open in editor at the right) and the active
    /// terminal. No header — the sidebar row already says which worktree this is.
    fn render_agents_main(&self, ui: &'static str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let fs = t.ui_font_size;
        if let Some(c) = self
            .agents
            .creating
            .iter()
            .find(|c| self.agents.selected.as_ref() == Some(&c.path))
        {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .h_full()
                .items_center()
                .justify_center()
                .gap_2()
                .bg(t.main_bg)
                .rounded(px(theme::ISLAND_RADIUS))
                .font_family(ui)
                .text_size(px(fs))
                .child(
                    div()
                        .text_size(px(fs + 3.0))
                        .text_color(t.text)
                        .child(SharedString::from(c.doing.clone())),
                )
                .child(div().text_color(t.line_number).child(
                    "Checking out a fresh worktree — a few seconds in a big repo. The agent \
                     starts as soon as it's ready.",
                ))
                .into_any_element();
        }
        let Some(wt) = self
            .agents
            .selected
            .clone()
            .filter(|wt| self.agents.cfg.space_of(wt).is_some())
        else {
            return ui::empty_state("Select a project or worktree", ui).into_any_element();
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(t.main_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .overflow_hidden()
            .font_family(ui)
            .text_size(px(fs))
            .text_color(t.text)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(self.render_agents_tabs(&wt, ui, cx)),
                    )
                    .when(self.agents.context_open, |d| {
                        d.child(div().w(px(1.0)).h_full().flex_none().bg(t.divider))
                            .child(self.render_agents_context(&wt, cx))
                    }),
            )
            .into_any_element()
    }

    /// The context panel beside the terminal: what this worktree is (its agent's AI title,
    /// the task it started with, the latest prompt — from Claude Code's own session record),
    /// plus its branch, path and git state.
    fn render_agents_context(&self, wt: &Path, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        let fs = t.ui_font_size;
        let ctx = self.agents.contexts.get(wt);
        let space = self.agents.cfg.space_of(wt).map(|s| s.path.clone());
        let is_default = space.as_deref() == Some(wt);
        let title: SharedString = ctx
            .and_then(|c| c.title.clone())
            .unwrap_or_else(|| worktree_title(space.as_deref(), wt))
            .into();
        let label = |text: &'static str| {
            div()
                .pt_3()
                .text_size(px(fs - 2.0))
                .text_color(t.line_number)
                .child(text)
        };
        let body = |text: String| {
            div()
                .text_color(t.secondary_text)
                .child(SharedString::from(text))
        };
        let git = self.agents.git.get(wt).copied().unwrap_or_default();
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let git_line = match (git.changed, git.ahead) {
            (0, 0) => "clean, nothing to push".to_string(),
            (c, 0) => format!("{c} uncommitted change{}", plural(c)),
            (0, a) => format!("{a} commit{} to push", plural(a)),
            (c, a) => format!(
                "{c} uncommitted change{} · {a} commit{} to push",
                plural(c),
                plural(a)
            ),
        };
        let sync_line = is_default.then(|| match self.agents.main_sync.get(wt) {
            Some(crate::agents::MainSync::Synced(r)) => format!("Kept in sync with {r}"),
            Some(crate::agents::MainSync::OffBranch(b)) => {
                format!("On {b} — only the default branch is kept in sync")
            }
            Some(crate::agents::MainSync::Failed(why)) => format!("Not synced: {why}"),
            None => "Default worktree".to_string(),
        });
        div()
            .id("agents-context")
            .w(px(320.0))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p_3()
            .bg(t.panel_bg)
            .rounded_r(px(theme::ISLAND_RADIUS))
            .text_size(px(fs))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(fs + 2.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        ui::icon_btn("agents-context-close", 20.0)
                            .tooltip(ui::tip("Hide context"))
                            .child("×")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _e, _w, cx| {
                                    this.agents.context_open = false;
                                    cx.notify();
                                }),
                            ),
                    ),
            )
            .children(self.agents.branches.get(wt).map(|b| {
                div()
                    .pt_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .text_color(t.secondary_text)
                    .child(
                        svg()
                            .path("icons/git-branch.svg")
                            .size(px(13.0))
                            .text_color(t.line_number),
                    )
                    .child(SharedString::from(b.clone()))
            }))
            .child(
                div()
                    .pt_1()
                    .text_size(px(fs - 1.0))
                    .text_color(t.line_number)
                    .child(SharedString::from(wt.display().to_string())),
            )
            // A scratch folder has no git.
            .when(!self.agents.cfg.is_scratch(wt), |d| {
                d.child(label("GIT")).child(body(git_line))
            })
            .children(sync_line.map(body))
            .child(label("TASK"))
            .child(body(ctx.and_then(|c| c.first_prompt.clone()).map_or_else(
                || "No agent session recorded here yet.".to_string(),
                |p| clip(&p, 1200),
            )))
            .children(
                ctx.and_then(|c| c.last_prompt.clone())
                    .map(|p| [label("LATEST PROMPT"), body(clip(&p, 600))])
                    .into_iter()
                    .flatten(),
            )
            .children(
                ctx.and_then(|c| c.updated)
                    .map(|u| [label("UPDATED"), body(ago(u))])
                    .into_iter()
                    .flatten(),
            )
            .child(
                div()
                    .pt_4()
                    .text_size(px(fs - 2.0))
                    .text_color(t.line_number)
                    .child(
                        "The title and prompts come from the agent's own Claude Code session \
                         — it keeps the title current as the task changes.",
                    ),
            )
            .into_any_element()
    }

    /// Tab strip + the active terminal of worktree `wt`.
    #[cfg(feature = "terminal")]
    fn render_agents_tabs(
        &self,
        wt: &Path,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        let group = self.agents.tabs.get(wt);
        let renaming_tab = matches!(self.agents.renaming, Some(RenameTarget::Tab(..)));
        // Background agents at work here (from a tab here or elsewhere): read-only tabs of
        // their own, showing their work. Stopped ones keep it here until dismissed.
        let subs: Vec<Bg> = self
            .agents_subs_in(wt)
            .map(|(_, t, s)| Bg::Live(t, s))
            .chain(self.agents_done_in(wt).map(Bg::Done))
            .collect();
        let picked = self.agents.sub_shown.as_deref();
        let has_tab = group.is_some_and(|g| !g.tabs.is_empty());
        let shown_sub = subs
            .iter()
            .find(|b| Some(b.sub().id.as_str()) == picked)
            .or_else(|| subs.first().filter(|_| !has_tab))
            .copied();
        // One tab and the setting's on: no strip — just its tools, floating top-right.
        let bare = self.agents.cfg.hide_single_tab
            && group.is_some_and(|g| g.tabs.len() == 1)
            && subs.is_empty()
            && !renaming_tab;
        let drop_wt = wt.to_path_buf();
        let mut strip = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .h(px(30.0))
            .px_2()
            .flex_none()
            .bg(t.panel_bg)
            // `overflow_hidden` clips to the island's rectangle, not its rounded corners, so
            // the strip rounds its own to match (the context panel takes the right one).
            .when(!bare, |d| {
                d.rounded_tl(px(theme::ISLAND_RADIUS))
                    .when(!self.agents.context_open, |d| {
                        d.rounded_tr(px(theme::ISLAND_RADIUS))
                    })
            })
            // A pressed tab drags along the strip; released anywhere, it lands where the
            // drop line shows.
            .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, _w, cx| {
                if this.agents.tab_drag.moved(e) {
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, {
                let wt = drop_wt.clone();
                cx.listener(move |this, _e, _w, cx| this.agents_drop_tab(&wt, cx))
            })
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| this.agents_drop_tab(&drop_wt, cx)),
            )
            .when(bare, |d| {
                d.absolute()
                    .top_0()
                    .right(px(8.0))
                    .rounded_bl_md()
                    .opacity(0.85)
            });
        let tabs: &[AgentTab] = if bare {
            &[]
        } else {
            group.map(|g| g.tabs.as_slice()).unwrap_or_default()
        };
        // The tabs scroll sideways once they outgrow the pane, so the strip's tools (`+`, the
        // context toggle, …) stay put on the right and no tab is ever out of reach.
        // `agents_reveal_tab` keeps the shown one in view.
        let mut pills = div()
            .id("agents-tabs-scroll")
            .flex()
            .flex_row()
            .items_center()
            .gap(px(AGENT_TAB_GAP))
            // Room inside the scroll clip for a drop line past the first / last tab, given
            // back outside so the tabs sit where they would without it.
            .px(px(AGENT_TAB_GAP))
            .mx(px(-AGENT_TAB_GAP))
            .min_w_0()
            .overflow_x_scroll()
            // (gpui scrolls it with a plain mouse's vertical wheel too.)
            .track_scroll(&self.agents.tab_scroll);
        for (i, tab) in tabs.iter().enumerate() {
            let active = group.is_some_and(|g| g.active == i) && shown_sub.is_none();
            // Dragged along the strip to reorder: it dims, and the tab under the cursor
            // shows the line it would land on.
            let dragged = self.agents.tab_drag.dragging(&tab.id);
            let drop_line = self
                .agents
                .tab_drag
                .line(&tab.id, |k| tabs.iter().position(|t| &t.id == k));
            let (press, hover) = (tab.id.clone(), tab.id.clone());
            let title: SharedString = tab
                .name
                .clone()
                .unwrap_or_else(|| tab_auto_title(tab, cx))
                .into();
            let renaming =
                self.agents.renaming == Some(RenameTarget::Tab(wt.to_path_buf(), tab.id.clone()));
            let grp = SharedString::from(format!("agents-tab-{i}"));
            let (sel, menu_wt) = (wt.to_path_buf(), wt.to_path_buf());
            let (tab_id, close, menu_id) = (tab.id.clone(), tab.id.clone(), tab.id.clone());
            pills = pills.child(
                div()
                    .id(("agents-tab", i))
                    .debug_selector(|| format!("agents-tab-{i}"))
                    .group(grp.clone())
                    .relative()
                    .when(dragged, |d| d.opacity(0.4))
                    .when_some(drop_line, |d, side| {
                        d.child(ui::reorder::drop_line(side, AGENT_TAB_GAP))
                    })
                    .on_mouse_move(cx.listener(move |this, _e: &gpui::MouseMoveEvent, _w, cx| {
                        if this.agents.tab_drag.hover(&hover) {
                            cx.notify();
                        }
                    }))
                    .flex()
                    .flex_row()
                    .flex_none()
                    .items_center()
                    .gap_1p5()
                    .max_w(px(260.0))
                    .h(px(22.0))
                    .pl_2()
                    .pr_1()
                    .rounded_md()
                    .border_1()
                    .cursor_pointer()
                    .when(active, |d| {
                        d.bg(t.bg_light).border_color(t.divider).text_color(t.text)
                    })
                    .when(!active, |d| {
                        d.border_color(gpui::rgba(0x0000_0000))
                            .text_color(t.line_number)
                            .hover(|d| d.bg(t.bg_mid))
                    })
                    .child(status_dot(tab.shown))
                    .child(if renaming {
                        self.agents_rename_field(cx)
                    } else {
                        div().min_w_0().truncate().child(title).into_any_element()
                    })
                    // Its background agents still at work, and where.
                    .when(!tab.subs.is_empty(), |d| {
                        let n = tab.subs.len();
                        let mut at: Vec<String> = tab
                            .subs
                            .iter()
                            .map(|s| match s.worktree.as_deref() {
                                Some(w) if w == wt => "this worktree".to_string(),
                                Some(w) => leaf_name(w),
                                None => "outside git".to_string(),
                            })
                            .collect();
                        at.sort();
                        at.dedup();
                        let tip = format!(
                            "{n} background agent{} at work — in {}",
                            if n == 1 { "" } else { "s" },
                            at.join(", ")
                        );
                        d.child(
                            div()
                                .id(("agents-tab-subs", i))
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_0p5()
                                .text_size(px(t.ui_font_size - 1.0))
                                .text_color(t.line_number)
                                .tooltip(ui::tip(tip))
                                .child(
                                    svg()
                                        .path("icons/bot.svg")
                                        .size(px(12.0))
                                        .text_color(t.line_number),
                                )
                                .child(SharedString::from(n.to_string())),
                        )
                    })
                    // The cleanup found it finished (closable from the Clean Up window).
                    .when_some(tab.verdict.clone(), |d, why| {
                        d.tooltip(ui::tip(format!("Finished: {why}")))
                            .child(div().flex_none().text_color(t.status_added).child("✓"))
                    })
                    .child(
                        div()
                            .id(("agents-tab-x", i))
                            .flex_none()
                            .w(px(18.0))
                            .flex()
                            .justify_center()
                            .rounded_sm()
                            .text_color(t.line_number)
                            .hover(|d| d.bg(t.bg_light).text_color(t.text))
                            .when(!active, |d| {
                                d.opacity(0.0).group_hover(grp, |s| s.opacity(1.0))
                            })
                            .child("×")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    cx.stop_propagation();
                                    this.agents_ask_close(vec![close.clone()], cx);
                                }),
                            ),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, window, cx| {
                            if renaming {
                                return;
                            }
                            // Double-click: rename it.
                            if e.click_count >= 2 {
                                this.agents_start_rename(
                                    RenameTarget::Tab(sel.clone(), tab_id.clone()),
                                    window,
                                    cx,
                                );
                                return;
                            }
                            this.agents.tab_drag.press(press.clone(), e);
                            if let Some(g) = this.agents.tabs.get_mut(&sel) {
                                g.active = i;
                            }
                            this.agents.sub_shown = None;
                            this.agents.focus_pending = true;
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, _w, cx| {
                            let target = MenuTarget::AgentTab(menu_wt.clone(), menu_id.clone());
                            this.open_menu(e.position, target, cx);
                        }),
                    ),
            );
        }
        for (k, &bg) in subs.iter().enumerate() {
            let sub = bg.sub();
            let active = shown_sub.is_some_and(|b| b.sub().id == sub.id);
            let (id, gone) = (sub.id.clone(), sub.id.clone());
            let grp = SharedString::from(format!("agents-sub-tab-{k}"));
            let mark = match bg {
                Bg::Live(..) => status_dot(AgentState::Working).into_any_element(),
                Bg::Done(d) => div()
                    .flex_none()
                    .text_color(if d.finished {
                        t.status_added
                    } else {
                        t.line_number
                    })
                    .child(if d.finished { "✓" } else { "–" })
                    .into_any_element(),
            };
            let tip = format!(
                "Background agent from “{}” ({}) — {}",
                bg.from(cx),
                bg.status(),
                sub.what
            );
            pills = pills.child(
                div()
                    .id(("agents-sub-tab", k))
                    .group(grp.clone())
                    .flex()
                    .flex_row()
                    .flex_none()
                    .items_center()
                    .gap_1p5()
                    .max_w(px(260.0))
                    .h(px(22.0))
                    .pl_2()
                    .pr(px(if matches!(bg, Bg::Done(_)) { 4.0 } else { 8.0 }))
                    .rounded_md()
                    .border_1()
                    .cursor_pointer()
                    .when(active, |d| {
                        d.bg(t.bg_light).border_color(t.divider).text_color(t.text)
                    })
                    .when(!active, |d| {
                        d.border_color(gpui::rgba(0x0000_0000))
                            .text_color(t.line_number)
                            .hover(|d| d.bg(t.bg_mid))
                    })
                    .tooltip(ui::tip(tip))
                    .child(mark)
                    .child(
                        svg()
                            .flex_none()
                            .path("icons/bot.svg")
                            .size(px(13.0))
                            .text_color(t.line_number),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(sub.what.clone())),
                    )
                    // Its work is kept until you dismiss it.
                    .when(matches!(bg, Bg::Done(_)), |d| {
                        d.child(
                            div()
                                .id(("agents-sub-tab-x", k))
                                .flex_none()
                                .w(px(18.0))
                                .flex()
                                .justify_center()
                                .rounded_sm()
                                .text_color(t.line_number)
                                .hover(|d| d.bg(t.bg_light).text_color(t.text))
                                .when(!active, |d| {
                                    d.opacity(0.0).group_hover(grp, |s| s.opacity(1.0))
                                })
                                .tooltip(ui::tip("Dismiss"))
                                .child("×")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _e, _w, cx| {
                                        cx.stop_propagation();
                                        this.agents_dismiss_sub(&gone, cx);
                                    }),
                                ),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.agents.sub_shown = Some(id.clone());
                            this.agents.sub_scroll.scroll_to_bottom();
                            cx.notify();
                        }),
                    ),
            );
        }
        if !bare {
            strip = strip.child(pills);
        }
        let (new_agent, new_shell, open) = (wt.to_path_buf(), wt.to_path_buf(), wt.to_path_buf());
        let cmd = self.agents.cfg.default_command().to_string();
        let new_tip = if subs.iter().any(|b| matches!(b, Bg::Live(..))) {
            format!("Start `{cmd}` here anyway — a background agent is already at work in it")
        } else {
            format!(
                "New tab running `{cmd}`  ({})",
                pretty_key(&format!("{TERMINAL_MOD}-t"))
            )
        };
        strip = strip
            .child(
                ui::icon_btn("agents-tab-new", 24.0)
                    .tooltip(ui::tip(new_tip))
                    .child("+")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.agents_new_tab(new_agent.clone(), true, None, cx);
                        }),
                    ),
            )
            .child(
                ui::icon_btn("agents-tab-shell", 24.0)
                    .tooltip(ui::tip("New shell tab"))
                    .child(
                        svg()
                            .path("icons/terminal.svg")
                            .size(px(14.0))
                            .text_color(t.line_number),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.agents_new_tab(new_shell.clone(), false, None, cx);
                        }),
                    ),
            )
            .child(div().flex_1().min_w_0())
            .child({
                let open = self.agents.context_open;
                ui::icon_btn("agents-context-toggle", 24.0)
                    .when(open, |d| d.bg(t.bg_light))
                    .tooltip(ui::tip(if open {
                        "Hide context"
                    } else {
                        "What is this worktree? — title, task, git"
                    }))
                    .child(
                        svg()
                            .path("icons/info.svg")
                            .size(px(15.0))
                            .text_color(t.line_number),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.agents.context_open = !open;
                            cx.notify();
                        }),
                    )
            })
            .child(
                ui::icon_btn("agents-open-editor", 24.0)
                    .tooltip(ui::tip(format!(
                        "Open {} in the editor{}",
                        leaf_name(wt),
                        self.keymap
                            .key_for("agent_toggle_code")
                            .map(|k| format!("  ({})", pretty_key(&k)))
                            .unwrap_or_default()
                    )))
                    .child(
                        svg()
                            .path("icons/folder.svg")
                            .size(px(15.0))
                            .text_color(t.line_number),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.agents_open_code(open.clone(), Some(Mode::Browse), cx);
                        }),
                    ),
            );
        let body = if let Some(bg) = shown_sub {
            self.render_agents_sub_feed(bg, ui, cx)
        } else if let Some(tab) = group.and_then(|g| g.tabs.get(g.active)) {
            div()
                .flex_1()
                .min_h_0()
                .pl_2()
                .pt_1()
                // The terminal paints a square background; this inset keeps it inside the
                // island's rounded bottom-right corner (and the top-right one when the strip
                // is hidden).
                .pr_1()
                .pb_1()
                .child(tab.view.clone())
                .into_any_element()
        } else {
            let restart = wt.to_path_buf();
            ui::empty_state("No tabs open in this worktree", ui)
                .flex_col()
                .gap_3()
                .child(btn_primary("agents-start", "Start agent").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        this.agents_new_tab(restart.clone(), true, None, cx);
                    }),
                ))
                .child(
                    div()
                        .text_size(px(t.ui_font_size - 1.0))
                        .font_family(theme::font::family())
                        .child(SharedString::from(format!("runs `{cmd}` here"))),
                )
                .into_any_element()
        };
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .map(|d| {
                if bare {
                    // Floating over the terminal's top-right corner.
                    d.child(body).child(strip)
                } else {
                    d.child(strip).child(body)
                }
            })
            .into_any_element()
    }

    /// A background agent's work, live: whose it is (and a way to that tab), then each step —
    /// what it said, each tool call with what it's on and how it went — newest at the bottom.
    #[cfg(feature = "terminal")]
    fn render_agents_sub_feed(
        &self,
        bg: Bg,
        ui: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        use crate::agents::Step;
        let t = theme::get();
        let sub = bg.sub();
        let name = bg.from(cx);
        let small = px(t.ui_font_size - 1.0);
        let age = short_age(
            std::time::SystemTime::now()
                .duration_since(sub.at)
                .unwrap_or_default(),
        );
        let ago = if age == "now" {
            "just now".to_string()
        } else {
            format!("{age} ago")
        };
        let said = match bg {
            Bg::Live(..) => format!("is working here · last step {ago}"),
            Bg::Done(d) if d.finished => format!("finished here · {ago}"),
            Bg::Done(_) => format!("stopped here (its session ended or went quiet) · {ago}"),
        };
        // Its tab, wherever it is now — if it's still open.
        let go = self
            .agents
            .tabs
            .iter()
            .find(|(_, g)| g.tabs.iter().any(|t| t.id == bg.tab_id()))
            .map(|(w, _)| (w.clone(), bg.tab_id().to_string()));
        let done = matches!(bg, Bg::Done(_)).then(|| sub.id.clone());
        let banner = div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(t.divider)
            .child(
                svg()
                    .flex_none()
                    .path("icons/bot.svg")
                    .size(px(18.0))
                    .text_color(t.line_number),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_color(t.text)
                            .child(SharedString::from(sub.what.clone())),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(small)
                            .text_color(t.line_number)
                            .child(SharedString::from(format!(
                                "A background agent from “{name}” {said}"
                            ))),
                    ),
            )
            .children(go.map(|(go_wt, go_id)| {
                btn_secondary("agents-sub-go", format!("Go to “{name}”")).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        this.agents_go_to_tab(go_wt.clone(), &go_id, cx);
                    }),
                )
            }))
            .children(done.map(|id| {
                btn_secondary("agents-sub-dismiss", "Dismiss").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        this.agents_dismiss_sub(&id, cx);
                    }),
                )
            }));
        let row = || div().flex().flex_row().items_start().gap_2().min_w_0();
        let steps = sub.steps.iter().map(|step| match step {
            Step::Said(text) => row()
                .py_1()
                .child(div().flex_none().w(px(14.0)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(t.text)
                        .child(SharedString::from(clip(text, 600))),
                )
                .into_any_element(),
            Step::Tool {
                name,
                on,
                note,
                outcome,
            } => {
                let (glyph, color) = match outcome {
                    // Never answered: still running — or, once it stopped, cut short.
                    None if matches!(bg, Bg::Done(_)) => ("–", t.line_number),
                    None => ("•", t.primary),
                    Some(Ok(())) => ("✓", t.line_number),
                    Some(Err(_)) => ("✗", t.status_conflict),
                };
                let why = match outcome {
                    Some(Err(why)) => Some(why.clone()),
                    _ => None,
                };
                row()
                    .child(div().flex_none().w(px(14.0)).text_color(color).child(glyph))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(t.secondary_text)
                                            .child(SharedString::from(name.clone())),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(theme::font::family())
                                            .text_color(t.line_number)
                                            .child(SharedString::from(
                                                on.clone().unwrap_or_default(),
                                            )),
                                    )
                                    .children(note.clone().map(|n| {
                                        div()
                                            .flex_none()
                                            .font_family(theme::font::family())
                                            .text_color(t.line_number)
                                            .child(SharedString::from(n))
                                    })),
                            )
                            .children(why.map(|w| {
                                div()
                                    .truncate()
                                    .text_size(small)
                                    .font_family(theme::font::family())
                                    .text_color(t.status_conflict)
                                    .child(SharedString::from(w))
                            })),
                    )
                    .into_any_element()
            }
        });
        let feed = div()
            .id("agents-sub-feed")
            .track_scroll(&self.agents.sub_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .when(sub.steps.is_empty(), |d| {
                d.child(div().text_color(t.line_number).child("Starting…"))
            })
            .children(steps.map(|r| div().flex_none().child(r)));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .font_family(ui)
            .bg(t.main_bg)
            .rounded(px(theme::ISLAND_RADIUS))
            .child(banner)
            .child(feed)
            .into_any_element()
    }

    #[cfg(not(feature = "terminal"))]
    fn render_agents_tabs(
        &self,
        _wt: &Path,
        ui: &'static str,
        _cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        ui::empty_state("This build has no terminal support", ui).into_any_element()
    }

    /// First visit to the Agents view: explain spaces → worktrees → tabs and confirm the start
    /// command + cleanup (both editable later in Settings → Agents). Not dismissable — Continue
    /// records `setup_done`.
    pub(crate) fn render_agents_setup(
        &mut self,
        ui: &'static str,
        fs: gpui::Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = theme::get();
        if !self.agents.setup_focused {
            self.agents.setup_focused = true;
            let handle = self.agents.default_cmd_input.read(cx).focus_handle.clone();
            window.focus(&handle);
            window.defer(cx, move |window, _cx| window.focus(&handle));
        }
        let point = |title: &'static str, body: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                .child(div().text_color(t.line_number).child(body))
        };
        let cleanup = self.agents.cfg.cleanup_mode == CleanupMode::Auto;
        let panel = div()
            .flex()
            .flex_col()
            .gap_4()
            .w(px(520.0))
            .p_5()
            .bg(t.bg_mid)
            .border_1()
            .border_color(t.bg_light)
            .rounded_md()
            .shadow_lg()
            .font_family(ui)
            .text_size(fs)
            .text_color(t.text)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        svg()
                            .path("icons/bot.svg")
                            .size(px(20.0))
                            .text_color(t.text),
                    )
                    .child(
                        div()
                            .text_size(px(t.ui_font_size + 3.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Set up Agents"),
                    ),
            )
            .child(point(
                "Projects",
                "Folders you work in — usually a repo. Add as many as you like.",
            ))
            .child(point(
                "Worktrees",
                "Each project's own folder, plus git worktrees you create with + so agents can \
                 work in parallel without stepping on each other.",
            ))
            .child(point(
                "Tabs",
                "Every worktree runs one or more terminal tabs that start with your start command.",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .pt_3()
                    .border_t_1()
                    .border_color(t.divider)
                    .child(div().text_color(t.secondary_text).child("Start command"))
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(t.divider)
                            .bg(t.main_bg)
                            .font_family(theme::font::family())
                            .child(self.agents.default_cmd_input.clone()),
                    ),
            )
            .child(
                div()
                    .id("agents-setup-cleanup")
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_2()
                    .cursor_pointer()
                    .child(div().pt(px(2.0)).child(crate::checkbox_box(cleanup)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child("Close finished tabs automatically")
                            .child(div().text_color(t.line_number).child(
                                "When its agent has quit, its PR is merged, or a quick AI check \
                                 says it's done — never the tab on screen. Unticked, they're only \
                                 marked for the Clean Up window.",
                            )),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| {
                            this.agents.cfg.cleanup_mode =
                                if this.agents.cfg.cleanup_mode == CleanupMode::Auto {
                                    CleanupMode::Suggest
                                } else {
                                    CleanupMode::Auto
                                };
                            cx.notify();
                        }),
                    ),
            )
            .child(div().text_color(t.line_number).child(format!(
                "Change these any time in Settings → Agents ({}).",
                pretty_key("cmd-,")
            )))
            .child(
                ui::modal_footer().child(
                    btn_primary("agents-setup-continue", "Continue")
                        .px_5()
                        .py_1p5()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| {
                                this.agents.cfg.setup_done = true;
                                this.agents.focus_pending = true;
                                this.agents_save(cx);
                                this.agents_autostart(cx);
                                cx.notify();
                            }),
                        ),
                ),
            )
            // Clicks inside the panel must not reach the backdrop.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        overlay(cx, false).child(panel).into_any_element()
    }

    /// No terminal, no tabs — the Clean Up window has nothing to list.
    #[cfg(not(feature = "terminal"))]
    pub(crate) fn render_cleanup_body(&mut self, _cx: &mut Context<Self>) -> gpui::AnyElement {
        div().into_any_element()
    }

    /// No terminal, no tabs to close.
    #[cfg(not(feature = "terminal"))]
    pub(crate) fn render_close_tabs_body(&mut self, _cx: &mut Context<Self>) -> gpui::AnyElement {
        div().into_any_element()
    }

    /// No terminal, no tabs to close.
    #[cfg(not(feature = "terminal"))]
    pub(crate) fn agents_close_confirmed(&mut self, _cx: &mut Context<Self>) {}

    /// The eraser button: the Clean Up window — close finished tabs, remove finished
    /// worktrees.
    fn agents_cleanup_now(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "terminal")]
        {
            // Tick every finished worktree (nothing uncommitted, nothing unpushed, no agent
            // busy) and every tab already found finished; re-read git so the statuses are
            // current, and re-run the cleanup's checks (it ticks tabs it newly finds finished).
            self.agents.cleanup_checked = self
                .agents
                .cfg
                .spaces
                .iter()
                .filter(|s| !s.scratch)
                .flat_map(|s| s.worktrees.iter().map(move |w| (s.path.clone(), w.clone())))
                .filter(|(_, wt)| !self.agents.cfg.pinned.contains(wt))
                .filter(|(space, wt)| self.agents_wt_cleanup(space, wt).1)
                .map(|(_, wt)| wt)
                .collect();
            self.agents.cleanup_tabs = self
                .agents
                .tabs
                .iter()
                .flat_map(|(wt, g)| {
                    g.tabs
                        .iter()
                        .filter(|t| t.verdict.is_some())
                        .map(move |t| (wt.clone(), t.spawned_at))
                })
                .collect();
            // …and every stopped background agent the window lists.
            self.agents.cleanup_subs = self
                .agents
                .cfg
                .spaces
                .iter()
                .flat_map(kyde_config::agents::Space::rows)
                .filter(|wt| !self.agents.cfg.pinned.contains(wt))
                .flat_map(|wt| {
                    self.agents_done_in(&wt)
                        .map(|d| d.sub.id.clone())
                        .collect::<Vec<_>>()
                })
                .collect();
            self.open_modal_window(ModalKind::Cleanup, "Clean Up", 600.0, 540.0, cx);
            self.agents_refresh_branches(cx);
            self.agents_sweep(true, cx);
        }
        #[cfg(not(feature = "terminal"))]
        let _ = cx;
    }
}

/// The placeholder row of a worktree still being checked out (it has no tabs or actions yet).
fn agents_pending_row(name: String, last: bool) -> gpui::AnyElement {
    let t = theme::get();
    div()
        .relative()
        .flex()
        .flex_col()
        .pl(px(WT_DOT_X))
        .pr_2()
        .py(px(ROW_PAD_Y))
        .text_color(t.line_number)
        .children(tree_connector(last))
        .child(
            title_line()
                .child(status_dot(AgentState::None))
                .child(div().flex_1().min_w_0().truncate().child(name))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(t.ui_font_size - 1.0))
                        .child("checking out…"),
                ),
        )
        .into_any_element()
}

/// The sidebar's tabs, in order: (tab, element id, label, short label for a narrow sidebar).
pub(crate) const SIDE_TABS: [(SideTab, &str, &str, &str); 4] = [
    (SideTab::Code, "agents-tab-code", "Code", "Code"),
    (SideTab::Issues, "agents-tab-issues", "Issues", "Issues"),
    (SideTab::Prs, "agents-tab-prs", "Pull Requests", "PRs"),
    (SideTab::Ci, "agents-tab-ci", "CI", "CI"),
];

/// Which sidebar tabs fit in `avail` px, given each one's (full, short) pill width, `gap`
/// between pills and the width of the ▾ that holds the rest: every tab at full width if
/// they fit, else every tab short, else the `active` one plus as many others as fit (in
/// order), short. Returns the shown tabs' indices in order, and whether they're short.
fn fit_tabs(
    avail: f32,
    widths: &[(f32, f32)],
    active: usize,
    gap: f32,
    more: f32,
) -> (Vec<usize>, bool) {
    #[allow(clippy::cast_precision_loss)]
    let span = |tabs: &[usize], short: bool| -> f32 {
        let pills: f32 = tabs
            .iter()
            .filter_map(|&i| widths.get(i))
            .map(|&(full, brief)| if short { brief } else { full })
            .sum();
        pills + gap * tabs.len().saturating_sub(1) as f32
    };
    let all: Vec<usize> = (0..widths.len()).collect();
    if span(&all, false) <= avail {
        return (all, false);
    }
    if span(&all, true) <= avail {
        return (all, true);
    }
    let room = avail - more - gap;
    let mut shown = vec![active];
    for i in all.into_iter().filter(|&i| i != active) {
        shown.push(i);
        if span(&shown, true) > room {
            shown.pop();
        }
    }
    shown.sort_unstable();
    (shown, true)
}

/// A GitHub list row's action button (`agents_gh_row`).
#[derive(Clone, Copy)]
enum RowAct {
    /// Shown on hover, with this label.
    Go(&'static str),
    /// Under way: shown disabled, with this label.
    Busy(&'static str),
    /// None.
    Hidden,
}

impl RowAct {
    /// An issue / PR row's action: `label`, or "Starting…" while its worktree is being made.
    fn starting(label: Option<&'static str>) -> Self {
        label.map_or(Self::Busy("Starting…"), Self::Go)
    }
}

/// Where a pull request's review stands (see `agents_review_state`).
enum ReviewState {
    /// No review worktree yet.
    None,
    /// Its worktree has a live agent tab.
    InProgress(PathBuf),
    /// Its worktree exists with no live agent — reviewed before.
    Done(PathBuf),
}

/// How a CI check came out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckOutcome {
    Pass,
    Fail,
    Pending,
}

impl CheckOutcome {
    /// Its glyph and colour.
    fn glyph(self) -> (&'static str, kyde_color::Color) {
        let t = theme::get();
        match self {
            Self::Pass => ("✓", t.status_added),
            Self::Fail => ("✗", t.status_conflict),
            Self::Pending => ("●", t.status_modified),
        }
    }
}

/// A check's status-or-conclusion (`SUCCESS`, `FAILURE`, `IN_PROGRESS`, …) as an outcome.
fn check_outcome(result: &str) -> CheckOutcome {
    match result.to_uppercase().as_str() {
        "SUCCESS" | "NEUTRAL" | "SKIPPED" => CheckOutcome::Pass,
        "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" => CheckOutcome::Fail,
        _ => CheckOutcome::Pending,
    }
}

/// A merge method's name in the merge box's dropdown, and what Merge says to confirm it.
fn merge_labels(m: kyde_git::github::MergeMethod) -> (&'static str, &'static str) {
    use kyde_git::github::MergeMethod;
    match m {
        MergeMethod::Merge => ("Create a merge commit", "merge"),
        MergeMethod::Squash => ("Squash and merge", "squash and merge"),
        MergeMethod::Rebase => ("Rebase and merge", "rebase and merge"),
    }
}

/// What a post from the issue / PR panel does.
#[derive(Clone, Copy)]
enum GhPost {
    Comment,
    Reply(u64),
    Close,
    Reopen,
    /// Approve the PR (the comment box, if filled, is the review's comment).
    Approve,
    /// Merge the PR.
    Merge(kyde_git::github::MergeMethod),
}

/// The PR conversation comment an Action comment button hands over, by position in
/// `gh_cards` (read when clicked, so the button keeps nothing of its own).
#[cfg(feature = "terminal")]
#[derive(Clone, Copy)]
enum FeedbackAt {
    /// Card `ci`: a comment, or a review's summary.
    Card(usize),
    /// Card `ci`'s thread `ti`, its comment `k` (the ones before it go along, for context).
    Thread(usize, usize, usize),
}

/// A small round avatar: the login's initial on a colour picked from the login.
fn avatar(login: &str) -> gpui::Div {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    login.hash(&mut h);
    const HUES: [u32; 6] = [
        0x0058_A6FF,
        0x003F_B950,
        0x00D2_9922,
        0x00DB_6D28,
        0x00A3_71F7,
        0x00F7_8166,
    ];
    #[allow(clippy::cast_possible_truncation)]
    let rgb = HUES[(h.finish() % HUES.len() as u64) as usize];
    div()
        .flex_none()
        .size(px(18.0))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::rgb(rgb))
        .text_color(gpui::white())
        .text_size(px(10.0))
        .font_weight(gpui::FontWeight::BOLD)
        .child(SharedString::from(
            login
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default(),
        ))
}

/// A scrolling list of rows (the PR's Commits / Checks tabs).
fn scroll_list(id: &'static str, rows: Vec<gpui::AnyElement>) -> gpui::AnyElement {
    div()
        .id(id)
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        // Rows keep their height; the list scrolls.
        .children(rows.into_iter().map(|r| div().flex_none().child(r)))
        .into_any_element()
}

/// A project's GitHub list as (items, still loading, why it failed).
fn gh_status<T>(list: Option<&GhList<T>>) -> (usize, bool, Option<&str>) {
    match list {
        Some(GhList::Ready(v)) => (v.len(), false, None),
        Some(GhList::Loading) | None => (0, true, None),
        Some(GhList::Failed(why)) => (0, false, Some(why)),
    }
}

/// A muted one-liner in the Issues / Pull Requests list (loading, empty, gh's error).
fn gh_note(text: impl Into<SharedString>) -> gpui::AnyElement {
    let t = theme::get();
    div()
        .px_2()
        .py_2()
        .text_size(px(t.ui_font_size - 1.0))
        .text_color(t.line_number)
        .child(text.into())
        .into_any_element()
}

/// The drop line for moving the item at `from` to `to` in a list: `before` it moving up,
/// `after` it moving down, none in place.
fn drop_side(from: usize, to: usize, before: DropLine, after: DropLine) -> Option<DropLine> {
    match to.cmp(&from) {
        std::cmp::Ordering::Less => Some(before),
        std::cmp::Ordering::Greater => Some(after),
        std::cmp::Ordering::Equal => None,
    }
}

/// A group's rows in the sidebar, indented under its header.
fn group_block(rows: Vec<gpui::AnyElement>) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .pl(px(GROUP_INDENT))
        .children(rows)
        .into_any_element()
}

/// The drop line between sidebar rows.
fn drop_line_el() -> gpui::AnyElement {
    div()
        .flex_none()
        .h(px(2.0))
        .mx(px(4.0))
        .rounded_full()
        .bg(theme::get().primary)
        .into_any_element()
}

/// A sidebar row's first line, at a fixed height so the dot, chevron and actions centre on
/// the title and the tree connector's stub can hang off its middle.
fn title_line() -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(GAP))
        .h(px(TITLE_H))
}

/// A sidebar row's second line: branch (or status) text, then the git badges.
fn sub_line(text: SharedString, selected: bool) -> gpui::Div {
    let t = theme::get();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .text_size(px(t.ui_font_size - 1.0))
        // `line_number` vanishes on the selected row's tint.
        .text_color(if selected {
            t.secondary_text
        } else {
            t.line_number
        })
        .child(div().min_w_0().truncate().child(text))
}

/// The drawn tree lines of a worktree row: a vertical line under the space's dot (running on
/// to the next row unless this is the `last`), and a stub across to this row's dot. Drawn,
/// not `├─` glyphs, so two-line rows stay connected.
fn tree_connector(last: bool) -> [gpui::Div; 2] {
    let line = theme::get().line_number;
    let mid = ROW_PAD_Y + TITLE_H / 2.0;
    [
        div()
            .absolute()
            .left(px(WT_LINE_X))
            // Overlap the row above a touch so consecutive rows join seamlessly.
            .top(px(-2.0))
            .w(px(1.0))
            .when(last, |d| d.h(px(mid + 2.0)))
            .when(!last, |d| d.bottom(px(0.0)))
            .bg(line)
            .opacity(0.6),
        div()
            .absolute()
            .left(px(WT_LINE_X))
            .top(px(mid))
            .w(px(WT_DOT_X - WT_LINE_X - 4.0))
            .h(px(1.0))
            .bg(line)
            .opacity(0.6),
    ]
}

/// `text` cut to about `max` characters (at a char boundary) with an ellipsis.
fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_string(),
    }
}

/// "just now" / "5 min ago" / "3 h ago" / "2 d ago" for a past time.
fn ago(t: std::time::SystemTime) -> String {
    let s = t.elapsed().map_or(0, |d| d.as_secs());
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86_400),
    }
}

/// A background agent in a worktree's strip: at work (its tab, and it), or stopped with its
/// work kept until dismissed.
#[cfg(feature = "terminal")]
#[derive(Clone, Copy)]
enum Bg<'a> {
    Live(&'a AgentTab, &'a crate::agents::SubAgent),
    Done(&'a crate::agents::DoneSub),
}

#[cfg(feature = "terminal")]
impl Bg<'_> {
    fn sub(&self) -> &crate::agents::SubAgent {
        match self {
            Self::Live(_, s) => s,
            Self::Done(d) => &d.sub,
        }
    }

    /// The id of the tab that started it.
    fn tab_id(&self) -> &str {
        match self {
            Self::Live(t, _) => &t.id,
            Self::Done(d) => &d.tab,
        }
    }

    /// That tab's name.
    fn from(&self, cx: &gpui::App) -> String {
        match self {
            Self::Live(t, _) => t.name.clone().unwrap_or_else(|| tab_auto_title(t, cx)),
            Self::Done(d) => d.from.clone(),
        }
    }

    fn status(&self) -> &'static str {
        match self {
            Self::Live(..) => "working",
            Self::Done(d) if d.finished => "finished",
            Self::Done(_) => "stopped",
        }
    }
}

/// The small Action comment button at the right of a comment's header: hands the comment at
/// `at` to an agent in the PR's worktree.
#[cfg(feature = "terminal")]
fn action_comment_btn(
    id: impl Into<gpui::ElementId>,
    at: FeedbackAt,
    cx: &mut Context<Kyde>,
) -> gpui::Stateful<gpui::Div> {
    btn_primary(id, "Action comment")
        .px_2()
        .py_0()
        .flex_none()
        .text_size(px(theme::get().ui_font_size - 2.0))
        .tooltip(ui::tip(
            "Hand this comment to an agent in the PR's worktree",
        ))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _e, _w, cx| {
                cx.stop_propagation();
                this.agents_action_comment(at, cx);
            }),
        )
}

/// A tab's inputs to its status dot.
#[cfg(feature = "terminal")]
fn tab_signals(t: &AgentTab, cx: &gpui::App) -> crate::agents::Signals {
    crate::agents::Signals {
        hook: t.hook.clone(),
        last_output: t.last_output,
        last_input: t.view.read(cx).last_input,
        unseen: t.unseen,
        background: !t.subs.is_empty(),
    }
}

/// Whether closing tab `t` should ask first: an agent given work that isn't seen as done —
/// not by its own report (`agents::close_asks`), not by the cleanup.
#[cfg(feature = "terminal")]
fn tab_unfinished(t: &AgentTab, now: Instant, cx: &gpui::App) -> bool {
    t.agent
        && t.used
        && t.verdict.is_none()
        && crate::agents::close_asks(
            AgentState::of_tab(&tab_signals(t, cx), now),
            t.hook.as_ref().map(|(h, _)| h),
        )
}

/// A tab's automatic title: what its terminal calls itself, else "agent" / "shell".
#[cfg(feature = "terminal")]
fn tab_auto_title(t: &AgentTab, cx: &gpui::App) -> String {
    let view = t.view.read(cx);
    if view.title.is_empty() || view.title == "Local" {
        if t.agent { "agent" } else { "shell" }.to_string()
    } else {
        view.title.clone()
    }
}

// ── keyboard navigation, renaming, scrollback ────────────────────────────────────────────
impl Kyde {
    /// The worktrees in the sidebar's order (unfolded groups and spaces only, attention order
    /// if on).
    fn agents_row_order(&self) -> Vec<PathBuf> {
        let cfg = &self.agents.cfg;
        let urgency = |w: &Path| self.agents_state(w).urgency();
        let mut out = Vec::new();
        for (group, mut idxs) in cfg.sections() {
            if group.is_some_and(|g| g.collapsed) {
                continue;
            }
            self.agents_sort_spaces(&mut idxs);
            for s in idxs
                .into_iter()
                .map(|i| &cfg.spaces[i])
                .filter(|s| !s.collapsed)
            {
                let mut wts = s.rows();
                if cfg.sort_attention {
                    wts.sort_by_key(|w| std::cmp::Reverse(urgency(w)));
                }
                out.extend(wts);
            }
        }
        out
    }

    /// Select the next (`1`) / previous (`-1`) worktree in the sidebar, wrapping.
    fn agents_cycle(&mut self, delta: isize, cx: &mut Context<Self>) {
        let order = self.agents_row_order();
        if order.is_empty() {
            return;
        }
        let len = order.len().cast_signed();
        let at = self
            .agents
            .selected
            .as_ref()
            .and_then(|s| order.iter().position(|w| w == s))
            .map_or(if delta > 0 { -1 } else { 0 }, usize::cast_signed);
        let next = (at + delta).rem_euclid(len).cast_unsigned();
        self.agents_select(order[next].clone(), cx);
    }

    /// ⌃N: the Nth worktree in the sidebar.
    fn agents_jump(&mut self, n: usize, cx: &mut Context<Self>) {
        if let Some(wt) = n
            .checked_sub(1)
            .and_then(|i| self.agents_row_order().get(i).cloned())
        {
            self.agents_select(wt, cx);
        }
    }

    /// The worktree that most needs you (waiting on input, then finished unseen) — the next
    /// one after the current, so pressing again walks through them.
    fn agents_jump_to_attention(&mut self, cx: &mut Context<Self>) {
        let order = self.agents_row_order();
        let top = order
            .iter()
            .map(|w| self.agents_state(w).urgency())
            .max()
            .unwrap_or(0);
        if top < AgentState::Unseen.urgency() {
            self.agents.note = Some("No agent is waiting on you".into());
            cx.notify();
            return;
        }
        let start = self
            .agents
            .selected
            .as_ref()
            .and_then(|s| order.iter().position(|w| w == s))
            .map_or(0, |i| i + 1);
        let pick = (0..order.len())
            .map(|k| &order[(start + k) % order.len()])
            .find(|w| self.agents_state(w).urgency() == top)
            .cloned();
        if let Some(wt) = pick {
            self.agents_select(wt, cx);
        }
    }

    /// Switch the selected worktree to its next (`1`) / previous (`-1`) tab, wrapping.
    fn agents_cycle_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        #[cfg(feature = "terminal")]
        if let Some(g) = self
            .agents
            .selected
            .clone()
            .and_then(|wt| self.agents.tabs.get_mut(&wt))
        {
            let len = g.tabs.len().cast_signed();
            if len > 0 {
                g.active = (g.active.cast_signed() + delta)
                    .rem_euclid(len)
                    .cast_unsigned();
                self.agents.focus_pending = true;
                cx.notify();
            }
        }
        #[cfg(not(feature = "terminal"))]
        let _ = (delta, cx);
    }

    /// Rename `target` in place: fill the name field with its current name and focus it.
    fn agents_start_rename(
        &mut self,
        target: RenameTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Start from the name it shows now, all selected — type to replace, or edit it.
        let custom = match &target {
            RenameTarget::Space(p) | RenameTarget::Worktree(p) => {
                self.agents.cfg.names.get(p).cloned()
            }
            // A group's name is always its own (`agents_auto_name`).
            RenameTarget::Group(_) => None,
            #[cfg(feature = "terminal")]
            RenameTarget::Tab(wt, id) => self
                .agents
                .tabs
                .get(wt)
                .and_then(|g| g.tabs.iter().find(|t| &t.id == id))
                .and_then(|t| t.name.clone()),
            #[cfg(not(feature = "terminal"))]
            RenameTarget::Tab(..) => None,
        };
        let current = custom.unwrap_or_else(|| self.agents_auto_name(&target, cx));
        let len = current.len();
        self.agents.rename_input.update(cx, |e, cx| {
            e.set_content(current, Lang::PlainText, cx);
            e.select_range(0..len, cx);
        });
        let focus = self.agents.rename_input.read(cx).focus_handle.clone();
        window.focus(&focus);
        self.agents.renaming = Some(target);
        cx.notify();
    }

    /// What `target` is called when you haven't named it.
    fn agents_auto_name(&self, target: &RenameTarget, cx: &App) -> String {
        match target {
            RenameTarget::Space(p) if *p == kyde_config::agents::scratch_dir() => "Scratch".into(),
            RenameTarget::Space(p) => leaf_name(p),
            // Its current name: renaming it to nothing (or the same) keeps that.
            RenameTarget::Group(id) => self
                .agents
                .cfg
                .groups
                .iter()
                .find(|g| g.id == *id)
                .map(|g| g.name.clone())
                .unwrap_or_default(),
            RenameTarget::Worktree(wt) => {
                let space = self
                    .agents
                    .cfg
                    .space_of(wt)
                    .map_or_else(|| wt.clone(), |s| s.path.clone());
                self.agents_wt_auto_title(&space, wt)
            }
            #[cfg(feature = "terminal")]
            RenameTarget::Tab(wt, id) => self
                .agents
                .tabs
                .get(wt)
                .and_then(|g| g.tabs.iter().find(|t| &t.id == id))
                .map(|t| tab_auto_title(t, cx))
                .unwrap_or_default(),
            #[cfg(not(feature = "terminal"))]
            RenameTarget::Tab(..) => {
                let _ = cx;
                String::new()
            }
        }
    }

    /// What a space is called: the name you gave it, else its folder's (Scratch's is fixed).
    fn agents_space_name(&self, space: &Space) -> String {
        self.agents
            .cfg
            .names
            .get(&space.path)
            .cloned()
            .unwrap_or_else(|| {
                if space.scratch {
                    "Scratch".into()
                } else {
                    leaf_name(&space.path)
                }
            })
    }

    /// What a worktree row is called: the name you gave it, else its automatic title. A
    /// project's own checkout is always its branch — it's shared, so it never takes a task's
    /// name (its agent's AI title) or a rename (`names` holds the project's under that path).
    pub(crate) fn agents_wt_title(&self, space: &Path, wt: &Path) -> String {
        if wt == space {
            return self.agents_wt_auto_title(space, wt);
        }
        self.agents
            .cfg
            .names
            .get(wt)
            .cloned()
            .unwrap_or_else(|| self.agents_wt_auto_title(space, wt))
    }

    /// A worktree row's automatic title: a project's own checkout is its branch; any other,
    /// its agent's AI title (per the row setting), else its origin, else its folder name.
    fn agents_wt_auto_title(&self, space: &Path, wt: &Path) -> String {
        if wt == space {
            return self
                .agents
                .branches
                .get(wt)
                .cloned()
                .unwrap_or_else(|| "default".into());
        }
        let ai = self
            .agents
            .cfg
            .row
            .ai_title
            .then(|| self.agents.contexts.get(wt).and_then(|c| c.title.clone()))
            .flatten();
        if let Some(t) = ai {
            t
        } else if let Some(o) = self.agents.cfg.origins.get(wt) {
            o.clone()
        } else {
            worktree_title(Some(space), wt)
        }
    }

    /// F2: rename the tab on screen.
    fn agents_rename_visible_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(feature = "terminal")]
        if let Some((wt, i)) = self.agents_visible_tab().map(|(w, i)| (w.to_path_buf(), i)) {
            if let Some(id) = self
                .agents
                .tabs
                .get(&wt)
                .and_then(|g| g.tabs.get(i))
                .map(|t| t.id.clone())
            {
                self.agents_start_rename(RenameTarget::Tab(wt, id), window, cx);
            }
        }
        #[cfg(not(feature = "terminal"))]
        let _ = (window, cx);
    }

    /// Enter keeps the new name (empty = back to the automatic one); Escape drops it.
    pub(crate) fn agents_finish_rename(&mut self, keep: bool, cx: &mut Context<Self>) {
        let Some(target) = self.agents.renaming.take() else {
            return;
        };
        self.agents.focus_pending = true;
        if keep {
            let name = self.agents.rename_input.read(cx).text().trim().to_string();
            // Empty, or unchanged from the automatic name: it stays automatic (an AI title
            // keeps updating).
            let auto = self.agents_auto_name(&target, cx);
            let name = (!name.is_empty() && name != auto).then_some(name);
            match target {
                RenameTarget::Space(p) | RenameTarget::Worktree(p) => match name {
                    Some(n) => {
                        self.agents.cfg.names.insert(p, n);
                    }
                    None => {
                        self.agents.cfg.names.remove(&p);
                    }
                },
                RenameTarget::Group(id) => {
                    if let (Some(n), Some(g)) =
                        (name, self.agents.cfg.groups.iter_mut().find(|g| g.id == id))
                    {
                        g.name = n;
                    }
                }
                #[cfg(feature = "terminal")]
                RenameTarget::Tab(wt, id) => {
                    if let Some(t) = self
                        .agents
                        .tabs
                        .get_mut(&wt)
                        .and_then(|g| g.tabs.iter_mut().find(|t| t.id == id))
                    {
                        t.name = name;
                    }
                }
                #[cfg(not(feature = "terminal"))]
                RenameTarget::Tab(..) => {}
            }
            self.agents_save(cx);
        }
        cx.notify();
    }

    /// The name field, in place of a title being renamed.
    fn agents_rename_field(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        div()
            // A click anywhere else keeps the name (Esc drops it).
            .on_mouse_down_out(cx.listener(|this, _e, _w, cx| {
                this.agents_finish_rename(true, cx);
            }))
            .flex_1()
            .min_w(px(140.0))
            .h(px(TITLE_H + 2.0))
            .flex()
            .items_center()
            .px_1()
            .overflow_hidden()
            .rounded_sm()
            .border_1()
            .border_color(t.primary)
            .bg(t.main_bg)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(self.agents.rename_input.clone()),
            )
            .into_any_element()
    }

    /// ⌘⌥E: the tab on screen's scrollback, in an editor window.
    fn agents_scrollback_visible(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "terminal")]
        if let Some(view) = self
            .agents_visible_tab()
            .map(|(w, i)| (w.to_path_buf(), i))
            .and_then(|(wt, i)| {
                self.agents
                    .tabs
                    .get(&wt)?
                    .tabs
                    .get(i)
                    .map(|t| t.view.clone())
            })
        {
            self.agents_open_scrollback(&view, cx);
        }
        #[cfg(not(feature = "terminal"))]
        let _ = cx;
    }

    /// Open terminal `v`'s whole scrollback in the Scrollback window's editor.
    #[cfg(feature = "terminal")]
    fn agents_open_scrollback(
        &mut self,
        v: &Entity<terminal::TerminalView>,
        cx: &mut Context<Self>,
    ) {
        let text = v.read(cx).scrollback_text();
        let title = {
            let view = v.read(cx);
            if view.title.is_empty() || view.title == "Local" {
                "Scrollback".to_string()
            } else {
                format!("Scrollback — {}", view.title)
            }
        };
        self.agents
            .scrollback
            .update(cx, |e, cx| e.set_content(text, Lang::PlainText, cx));
        self.close_modal_window(ModalKind::Scrollback, cx);
        self.open_modal_window(ModalKind::Scrollback, title, 960.0, 680.0, cx);
    }

    /// Body of the Scrollback window: the text, editable, ⌘F to search.
    pub(crate) fn render_scrollback_body(&mut self, _cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = theme::get();
        div()
            .size_full()
            .p_2()
            .bg(t.main_bg)
            .font_family(theme::font::family())
            .child(self.agents.scrollback.clone())
            .into_any_element()
    }
}

/// A compact age for the sidebar: "now", "5m", "3h", "2d".
fn short_age(d: Duration) -> String {
    match d.as_secs() {
        0..60 => "now".into(),
        s @ 60..3600 => format!("{}m", s / 60),
        s @ 3600..86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// A worktree's display name: its folder, minus herdr's `worktree-` prefix and its repo's
/// own name (`hoxtonmix-api-dsa` in `hoxtonmix-api` → `dsa`).
fn worktree_title(space: Option<&Path>, wt: &Path) -> String {
    let mut name = leaf_name(wt);
    let repo = space
        .and_then(Path::file_name)
        .map(|n| format!("{}-", n.to_string_lossy()));
    for prefix in std::iter::once("worktree-".to_string()).chain(repo) {
        if let Some(rest) = name.strip_prefix(&prefix).filter(|r| !r.is_empty()) {
            name = rest.to_string();
        }
    }
    name
}

/// A row's git badges: ↑ + unpushed commits, ✎ + uncommitted files — or a ✓ when it's clean
/// and everything is pushed (the tooltip spells them out). Nothing outside git.
fn git_badges(
    id: impl Into<gpui::ElementId>,
    g: Option<&crate::agents::WtGit>,
) -> Option<gpui::Stateful<gpui::Div>> {
    let g = *g?;
    let t = theme::get();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let tip = match (g.changed, g.ahead) {
        (0, 0) if g.landed => "No uncommitted changes · already merged into main".to_string(),
        (0, 0) => "No uncommitted changes · everything pushed".to_string(),
        (0, a) => format!("{a} commit{} to push", plural(a)),
        (c, 0) => format!("{c} uncommitted change{}", plural(c)),
        (c, a) => format!(
            "{c} uncommitted change{} · {a} commit{} to push",
            plural(c),
            plural(a)
        ),
    };
    Some(
        div()
            .id(id)
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .flex_none()
            .text_size(px(t.ui_font_size - 1.0))
            .tooltip(ui::tip(tip))
            // Icons + counts; the tooltip spells them out.
            .when(g.changed == 0 && g.ahead == 0, |d| {
                d.child(
                    svg()
                        .path("icons/check.svg")
                        .size(px(12.0))
                        .text_color(t.status_added),
                )
            })
            .when(g.ahead > 0, |d| {
                d.child(badge_icon("icons/arrow-up.svg", g.ahead, t.status_added))
            })
            .when(g.changed > 0, |d| {
                d.child(badge_icon("icons/pencil.svg", g.changed, t.status_modified))
            }),
    )
}

/// One git badge: a small icon and its count, in `color`.
fn badge_icon(icon: &'static str, n: usize, color: kyde_color::Color) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_0p5()
        .text_color(color)
        .child(svg().path(icon).size(px(11.0)).text_color(color))
        .child(SharedString::from(n.to_string()))
}

/// An 8 px status dot: working = accent, done-but-unseen = green, idle = ring, none = faint ring.
fn status_dot(state: AgentState) -> gpui::Div {
    let t = theme::get();
    let d = div().flex_none().size(px(DOT)).rounded_full();
    match state {
        AgentState::Blocked => d.bg(t.status_conflict),
        AgentState::Working => d.bg(t.primary),
        AgentState::Unseen => d.bg(t.status_added),
        AgentState::Idle => d.border_1().border_color(t.status_added),
        AgentState::None => d.border_1().border_color(t.line_number),
    }
}

/// A path's last component, for labels.
/// One line of the Clean Up window: a checkbox, the title with something quieter beside it,
/// and a status line under it (red when acting on it loses work). The caller adds the click.
#[cfg(feature = "terminal")]
fn cleanup_row(
    id: SharedString,
    checked: bool,
    title: String,
    aside: String,
    line: String,
    danger: bool,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    let fs = t.ui_font_size;
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .hover(|d| d.bg(t.bg_mid))
        .child(div().pt(px(2.0)).child(crate::checkbox_box(checked)))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(div().min_w_0().truncate().child(title))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(fs - 1.0))
                                .text_color(t.line_number)
                                .child(aside),
                        ),
                )
                .child(
                    div()
                        .text_size(px(fs - 1.0))
                        .text_color(if danger {
                            t.status_conflict
                        } else {
                            t.secondary_text
                        })
                        .child(line),
                ),
        )
}

fn leaf_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A narrowing sidebar shortens the tab labels, then tucks tabs behind the ▾ — never
    /// the one you're on.
    #[test]
    fn sidebar_tabs_shorten_then_overflow_keeping_the_active_one() {
        // Code, Issues, Pull Requests | PRs, CI.
        let w = [(50.0, 50.0), (60.0, 60.0), (110.0, 45.0), (30.0, 30.0)];
        assert_eq!(fit_tabs(300.0, &w, 3, 2.0, 22.0), (vec![0, 1, 2, 3], false));
        assert_eq!(fit_tabs(200.0, &w, 3, 2.0, 22.0), (vec![0, 1, 2, 3], true));
        // Room for two short tabs beside the ▾: CI (active) stays, PRs + Issues go.
        assert_eq!(fit_tabs(110.0, &w, 3, 2.0, 22.0), (vec![0, 3], true));
    }
}
