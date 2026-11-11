//! Agents-view settings, persisted as `agents.json` next to keymap.json.
//!
//! The Agents view is a herdr-style workspace for AI coding agents: **spaces** (starting
//! folders) each own a set of **worktrees** — the space's own folder is always the first
//! (the default), plus any linked git worktrees created from it — and every worktree runs
//! one or more terminal tabs that start with the space's **start command** (`claude` by
//! default). This file holds that layout plus the idle-tab cleanup policy; the live tabs
//! themselves are runtime-only.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The command a new agent tab runs when neither the space nor the user configured one.
pub const DEFAULT_COMMAND: &str = "claude";

/// One space: a starting folder and the linked worktrees created from it.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug, Default)]
pub struct Space {
    /// The starting folder. Doubles as the space's default worktree.
    pub path: PathBuf,
    /// Linked worktrees created from (or added to) this space, in creation order.
    #[serde(default)]
    pub worktrees: Vec<PathBuf>,
    /// Its worktrees are folded away in the sidebar.
    #[serde(default)]
    pub collapsed: bool,
    /// Where the space's own checkout sits among its worktrees in the sidebar (0 = first).
    #[serde(default)]
    pub main_pos: usize,
    /// The built-in Scratch space ([`scratch_dir`]): its rows are throwaway folders, one per
    /// ad-hoc agent, and its own folder is never a row.
    #[serde(default)]
    pub scratch: bool,
    /// The [`Group`] it's listed under, by [`Group::id`] (`None`: ungrouped — below the
    /// groups).
    #[serde(default)]
    pub group: Option<u32>,
}

/// A named group of spaces in the sidebar ("Work", "Personal"): it folds away and drags as
/// one.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug, Default)]
pub struct Group {
    /// What its spaces' [`Space::group`] names it by — stable across renames.
    pub id: u32,
    /// What the sidebar calls it.
    pub name: String,
    /// Its spaces are folded away.
    #[serde(default)]
    pub collapsed: bool,
}

impl Space {
    /// The sidebar's rows for this space, in order: its linked worktrees with its own
    /// checkout at [`Self::main_pos`] — Scratch's are just its folders.
    ///
    /// ```
    /// use kyde_config::agents::Space;
    /// let s = Space { path: "/r".into(), worktrees: vec!["/a".into(), "/b".into()], main_pos: 1, ..Space::default() };
    /// assert_eq!(s.rows(), ["/a", "/r", "/b"].map(std::path::PathBuf::from));
    /// let scratch = Space { scratch: true, ..s };
    /// assert_eq!(scratch.rows(), ["/a", "/b"].map(std::path::PathBuf::from));
    /// ```
    #[must_use]
    pub fn rows(&self) -> Vec<PathBuf> {
        let mut rows = self.worktrees.clone();
        if !self.scratch {
            rows.insert(self.main_pos.min(rows.len()), self.path.clone());
        }
        rows
    }

    /// Set the row order (as [`Self::rows`] gives it): where the checkout goes, and the
    /// worktrees' order.
    pub fn set_rows(&mut self, rows: Vec<PathBuf>) {
        self.main_pos = rows.iter().position(|w| *w == self.path).unwrap_or(0);
        self.worktrees = rows.into_iter().filter(|w| *w != self.path).collect();
    }
}

/// A tab to reopen on the next launch — reattached to its still-running agent host, or (when
/// that's gone) an agent tab resumes its Claude Code session.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug, Default)]
pub struct SavedTab {
    /// Runs the start command (vs a plain shell).
    #[serde(default)]
    pub agent: bool,
    /// Its Claude Code session id, once known — reopened with `--resume <id>`.
    #[serde(default)]
    pub session: Option<String>,
    /// Its id: names its agent host + state files under [`agents_dir`].
    #[serde(default)]
    pub id: Option<String>,
    /// A name you gave it (else it's titled by its agent).
    #[serde(default)]
    pub name: Option<String>,
}

/// What each worktree row in the sidebar shows (besides its status dot).
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[allow(clippy::struct_excessive_bools)] // independent on/off switches, one per field
pub struct RowFields {
    /// Title rows by their agent's AI title (else by folder / branch name).
    #[serde(default = "yes")]
    pub ai_title: bool,
    /// The git badges (changes / to push / clean).
    #[serde(default = "yes")]
    pub git: bool,
    /// What the agent needs: "needs input", "done" (working is the blue dot).
    #[serde(default = "yes")]
    pub status: bool,
    /// How long since its agent last did anything.
    #[serde(default)]
    pub activity: bool,
}

impl Default for RowFields {
    fn default() -> Self {
        Self {
            ai_title: true,
            git: true,
            status: true,
            activity: false,
        }
    }
}

/// Agents-view settings (persisted to `agents.json`).
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct AgentsCfg {
    /// The first-run setup (start command + cleanup) was confirmed; until then, entering the
    /// Agents view shows the setup overlay.
    #[serde(default)]
    pub setup_done: bool,
    /// The window shows only the Agents view — no IDE rail, toolbars or status bar.
    #[serde(default)]
    pub agents_only: bool,

    /// The command every new agent tab starts with.
    #[serde(default = "default_command")]
    pub command: String,
    /// What a new agent tab does when its first task doesn't fit the work already in its
    /// worktree.
    #[serde(default)]
    pub new_task: NewTask,
    /// What the cleanup does with tabs its rules find finished.
    #[serde(default)]
    pub cleanup_mode: CleanupMode,
    /// Which signals mark a tab finished.
    #[serde(default)]
    pub cleanup: CleanupRules,
    /// Hours a tab may sit idle (no output, not on screen) before the idle rule closes it
    /// (clamped to 1..=720).
    #[serde(default = "default_idle_hours")]
    pub idle_hours: u32,
    /// The spaces — in sidebar order within each group, and among the ungrouped.
    #[serde(default)]
    pub spaces: Vec<Space>,
    /// The groups of spaces, in sidebar order (above the ungrouped spaces).
    #[serde(default)]
    pub groups: Vec<Group>,
    /// The worktree last on screen — reopened on the next launch.
    #[serde(default)]
    pub selected: Option<PathBuf>,
    /// Each worktree's open tabs, reopened (sessions resumed) when it's next selected.
    #[serde(default)]
    pub tabs: std::collections::BTreeMap<PathBuf, Vec<SavedTab>>,
    /// Worktrees you pinned — long-running work the cleanup leaves alone: it never closes
    /// their tabs, and never offers to remove them.
    #[serde(default)]
    pub pinned: std::collections::BTreeSet<PathBuf>,
    /// Keep the Claude Code hook installed that reports each agent's state (needs input /
    /// working / done) and session to Kyde.
    #[serde(default = "yes")]
    pub claude_hooks: bool,
    /// Play a sound when an agent finishes or needs input while you're elsewhere.
    #[serde(default = "yes")]
    pub sound: bool,
    /// Bounce the Dock icon (Linux: show a desktop notification) for the same, while Kyde
    /// isn't frontmost.
    #[serde(default = "yes")]
    pub dock_alerts: bool,
    /// List what needs you first: worktrees by attention (needs input, done, working, idle)
    /// and spaces by their most urgent worktree.
    #[serde(default)]
    pub sort_attention: bool,
    /// Hide a worktree's tab strip while it has a single tab.
    #[serde(default)]
    pub hide_single_tab: bool,
    /// What each worktree row shows.
    #[serde(default)]
    pub row: RowFields,
    /// Names you gave spaces / worktrees (else they're titled automatically).
    #[serde(default)]
    pub names: std::collections::BTreeMap<PathBuf, String>,
    /// What a worktree was made for ("Review #12: …", "#34 …") — its title until its agent
    /// has one.
    #[serde(default)]
    pub origins: std::collections::BTreeMap<PathBuf, String>,
    /// The prompt an issue's agent starts with ([`fill`] placeholders). Blank = the default.
    #[serde(default)]
    pub issue_prompt: String,
    /// The prompt a pull request's review agent starts with. Blank = the default.
    #[serde(default)]
    pub review_prompt: String,
    /// The prompt for reviewing a pull request again. Blank = the default.
    #[serde(default)]
    pub rereview_prompt: String,
}

/// The standard prompt an issue's agent starts with.
pub const ISSUE_PROMPT: &str = "Resolve GitHub issue #{number}: {title} ({url}). Your job is to \
fix the bug or build the feature it describes, not just to read it, so start right away \
without asking me what to do. Read the issue and its discussion with `gh issue view {number} \
--comments`. You're on branch {branch} in a fresh worktree off the latest {base}. If the change \
is straightforward, implement it, add focused tests and run them. If it's big or unclear, use \
plan mode and give me an implementation plan to approve before coding.";

/// The standard prompt a pull request's review agent starts with.
pub const REVIEW_PROMPT: &str = "Review pull request #{number}: {title} by {author} ({url}). \
You're in a worktree on its branch ({branch}, into {base}). Read the description and \
discussion with `gh pr view {number} --comments` and the change with `gh pr diff {number}`, \
then review it: 1) correctness bugs, 2) security and data risks, 3) missing or weak tests, 4) \
clarity and maintainability. Cite file:line for each finding and say how sure you are; skip \
nitpicks. End with a verdict: approve, approve with nits, or request changes. Don't push, \
comment on GitHub, or change code unless I ask.";

/// The standard prompt for reviewing a pull request again.
pub const REREVIEW_PROMPT: &str = "Re-review pull request #{number}: {title} by {author} \
({url}). You reviewed it before and the worktree now has its latest commits. Read `gh pr view \
{number} --comments` and `gh pr diff {number}`; focus on what changed since your last review \
and whether the earlier feedback was addressed — correctness bugs first. End with a verdict. \
Don't push, comment on GitHub, or change code unless I ask.";

/// `template` with each `{key}` replaced by its value (unknown placeholders are left as is).
///
/// ```
/// use kyde_config::agents::fill;
/// let p = fill("Review #{number}: {title} {nope}", &[("number", "7"), ("title", "Fix")]);
/// assert_eq!(p, "Review #7: Fix {nope}");
/// ```
#[must_use]
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    values.iter().fold(template.to_string(), |s, (k, v)| {
        s.replace(&format!("{{{k}}}"), v)
    })
}

fn default_command() -> String {
    DEFAULT_COMMAND.into()
}
fn yes() -> bool {
    true
}
fn default_quit_mins() -> u32 {
    10
}

/// What a new agent tab does when you give it a task unrelated to the work already in its
/// worktree — another agent's, uncommitted changes, unpushed commits. Kyde tells the agent
/// what's there through the Claude Code hook; the agent judges whether the task fits.
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum NewTask {
    /// The agent moves itself to a fresh worktree, and its tab follows.
    #[default]
    Move,
    /// The agent asks you before moving.
    Ask,
    /// It stays in the worktree it was opened in.
    Stay,
}

/// What the cleanup does with a tab its rules find finished.
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum CleanupMode {
    /// No cleanup at all.
    Off,
    /// Mark finished tabs and list them in the cleanup window — you close them.
    Suggest,
    /// Close finished tabs automatically (never the one on screen, never output you
    /// haven't seen).
    #[default]
    Auto,
}

/// The signals that mark a tab finished — each can be switched off.
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct CleanupRules {
    /// Its agent has quit — nothing left running but the shell — for `quiet_mins`.
    #[serde(default = "yes")]
    pub agent_quit: bool,
    /// Nothing was ever typed into it (e.g. an agent auto-started by selecting a worktree).
    #[serde(default = "yes")]
    pub unused: bool,
    /// Minutes a tab must be quiet (no output, not on screen) before any rule applies
    /// (clamped to 1..=240).
    #[serde(default = "default_quit_mins")]
    pub quiet_mins: u32,
    /// Its worktree's branch has a merged pull request.
    #[serde(default = "yes")]
    pub merged: bool,
    /// A quick AI check (`claude -p` on a small model) judges its work finished.
    #[serde(default = "yes")]
    pub ai_done: bool,
    /// It's been idle [`AgentsCfg::idle_hours`] and its worktree is clean.
    #[serde(default = "yes")]
    pub idle: bool,
}

impl Default for CleanupRules {
    fn default() -> Self {
        Self {
            agent_quit: true,
            unused: true,
            quiet_mins: default_quit_mins(),
            merged: true,
            ai_done: true,
            idle: true,
        }
    }
}

impl CleanupRules {
    /// How long a tab must be quiet before any rule may close it.
    #[must_use]
    pub fn quiet(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.quiet_mins.clamp(1, 240)) * 60)
    }
}
fn default_idle_hours() -> u32 {
    24
}

impl Default for AgentsCfg {
    fn default() -> Self {
        Self {
            setup_done: false,
            agents_only: false,

            command: default_command(),
            new_task: NewTask::default(),
            cleanup_mode: CleanupMode::default(),
            cleanup: CleanupRules::default(),
            idle_hours: default_idle_hours(),
            spaces: Vec::new(),
            groups: Vec::new(),
            selected: None,
            tabs: std::collections::BTreeMap::new(),
            pinned: std::collections::BTreeSet::new(),
            claude_hooks: true,
            sound: true,
            dock_alerts: true,
            sort_attention: false,
            hide_single_tab: false,
            row: RowFields::default(),
            names: std::collections::BTreeMap::new(),
            origins: std::collections::BTreeMap::new(),
            issue_prompt: String::new(),
            review_prompt: String::new(),
            rereview_prompt: String::new(),
        }
    }
}

impl AgentsCfg {
    /// Load from `agents.json` (missing/corrupt → defaults).
    #[must_use]
    pub fn load() -> Self {
        let mut c = crate::store::load_or_default::<Self>("agents.json");
        c.idle_hours = c.idle_hours.clamp(1, 720);
        c.cleanup.quiet_mins = c.cleanup.quiet_mins.clamp(1, 240);
        c.ensure_scratch();
        c
    }

    /// Add the Scratch space (last) when there isn't one — every layout has it.
    pub fn ensure_scratch(&mut self) {
        if !self.spaces.iter().any(|s| s.scratch) {
            self.spaces.push(Space {
                path: scratch_dir(),
                scratch: true,
                ..Space::default()
            });
        }
    }

    /// Where a newly added project goes: after the last project, so Scratch stays below them.
    #[must_use]
    pub fn new_space_index(&self) -> usize {
        self.spaces
            .iter()
            .rposition(|s| !s.scratch)
            .map_or(0, |i| i + 1)
    }

    /// Whether `wt` is one of Scratch's rows.
    #[must_use]
    pub fn is_scratch(&self, wt: &Path) -> bool {
        self.space_of(wt).is_some_and(|s| s.scratch)
    }

    /// Persist to `agents.json`.
    pub fn save(&self) {
        crate::store::save("agents.json", self);
    }

    /// The idle window after which cleanup closes a tab.
    #[must_use]
    pub fn idle(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.idle_hours.clamp(1, 720)) * 60 * 60)
    }

    /// The issue prompt template in use (yours, else [`ISSUE_PROMPT`]).
    #[must_use]
    pub fn issue_prompt(&self) -> &str {
        or_default(&self.issue_prompt, ISSUE_PROMPT)
    }

    /// The review prompt template in use (yours, else [`REVIEW_PROMPT`]).
    #[must_use]
    pub fn review_prompt(&self) -> &str {
        or_default(&self.review_prompt, REVIEW_PROMPT)
    }

    /// The re-review prompt template in use (yours, else [`REREVIEW_PROMPT`]).
    #[must_use]
    pub fn rereview_prompt(&self) -> &str {
        or_default(&self.rereview_prompt, REREVIEW_PROMPT)
    }

    /// The command new agent tabs start with: [`Self::command`], or [`DEFAULT_COMMAND`]
    /// when it's blank (a cleared field must not leave tabs commandless).
    ///
    /// ```
    /// use kyde_config::agents::AgentsCfg;
    /// let mut c = AgentsCfg::default();
    /// assert_eq!(c.default_command(), "claude");
    /// c.command = " claude --resume ".into();
    /// assert_eq!(c.default_command(), "claude --resume");
    /// c.command = "  ".into();
    /// assert_eq!(c.default_command(), "claude");
    /// ```
    #[must_use]
    pub fn default_command(&self) -> &str {
        match self.command.trim() {
            "" => DEFAULT_COMMAND,
            c => c,
        }
    }

    /// The space a worktree belongs to — the space whose folder IS `wt`, or whose linked
    /// worktrees include it. Scratch's own folder isn't a worktree (`None`).
    #[must_use]
    pub fn space_of(&self, wt: &Path) -> Option<&Space> {
        self.spaces
            .iter()
            .find(|s| (s.path == wt && !s.scratch) || s.worktrees.iter().any(|w| w == wt))
    }

    /// The group `space` is listed under (`None`: ungrouped — also when its group is gone).
    #[must_use]
    pub fn group_of(&self, space: &Space) -> Option<&Group> {
        let id = space.group?;
        self.groups.iter().find(|g| g.id == id)
    }

    /// The sidebar's sections, top to bottom: each group with its spaces, then the ungrouped
    /// spaces (`None`). Spaces are indices into [`Self::spaces`], in their order there.
    ///
    /// ```
    /// use kyde_config::agents::{AgentsCfg, Group, Space};
    /// let space = |p: &str, group| Space { path: p.into(), group, ..Space::default() };
    /// let group = |id, name: &str| Group { id, name: name.into(), ..Group::default() };
    /// let c = AgentsCfg {
    ///     spaces: vec![space("/a", None), space("/b", Some(2)), space("/c", Some(1)),
    ///                  space("/d", Some(2)), space("/e", Some(9))],
    ///     groups: vec![group(2, "Work"), group(1, "Personal")],
    ///     ..AgentsCfg::default()
    /// };
    /// let s: Vec<_> = c.sections().into_iter().map(|(g, v)| (g.map(|g| g.id), v)).collect();
    /// // A space naming a group that's gone (/e) is ungrouped.
    /// assert_eq!(s, [(Some(2), vec![1, 3]), (Some(1), vec![2]), (None, vec![0, 4])]);
    /// ```
    #[must_use]
    pub fn sections(&self) -> Vec<(Option<&Group>, Vec<usize>)> {
        let mut out: Vec<(Option<&Group>, Vec<usize>)> =
            self.groups.iter().map(|g| (Some(g), Vec::new())).collect();
        let mut loose = Vec::new();
        for (i, s) in self.spaces.iter().enumerate() {
            match s
                .group
                .and_then(|id| self.groups.iter().position(|g| g.id == id))
            {
                Some(gi) => out[gi].1.push(i),
                None => loose.push(i),
            }
        }
        out.push((None, loose));
        out
    }

    /// The spaces in sidebar order (see [`Self::sections`]), as indices into [`Self::spaces`].
    #[must_use]
    pub fn space_order(&self) -> Vec<usize> {
        self.sections().into_iter().flat_map(|(_, v)| v).collect()
    }

    /// Move space `from` to where space `onto` is in the sidebar: into `onto`'s group, above
    /// it when moving up, below it when moving down. `false`: nothing moved.
    ///
    /// ```
    /// use kyde_config::agents::{AgentsCfg, Group, Space};
    /// use std::path::{Path, PathBuf};
    /// let space = |p: &str, group| Space { path: p.into(), group, ..Space::default() };
    /// let mut c = AgentsCfg {
    ///     spaces: vec![space("/a", Some(1)), space("/b", None), space("/c", None)],
    ///     groups: vec![Group { id: 1, name: "Work".into(), ..Group::default() }],
    ///     ..AgentsCfg::default()
    /// };
    /// // Work [/a], then /b and /c. Drag /c up onto /a: it joins Work, above /a.
    /// assert!(c.move_space(Path::new("/c"), Path::new("/a")));
    /// let order: Vec<_> = c.space_order().into_iter().map(|i| c.spaces[i].path.clone()).collect();
    /// assert_eq!(order, ["/c", "/a", "/b"].map(PathBuf::from));
    /// assert_eq!(c.spaces[0].group, Some(1));
    /// ```
    pub fn move_space(&mut self, from: &Path, onto: &Path) -> bool {
        let order = self.space_order();
        let at = |p: &Path| order.iter().position(|&i| self.spaces[i].path == p);
        let (Some(df), Some(dt)) = (at(from), at(onto)) else {
            return false;
        };
        if df == dt {
            return false;
        }
        let group = self.group_of(&self.spaces[order[dt]]).map(|g| g.id);
        let mut s = self.spaces.remove(order[df]);
        s.group = group;
        let o = self
            .spaces
            .iter()
            .position(|x| x.path == onto)
            .unwrap_or(self.spaces.len());
        self.spaces
            .insert(if df < dt { o + 1 } else { o }.min(self.spaces.len()), s);
        true
    }

    /// Move space `path` into group `group` (`None`: out of any group) — first in it.
    /// `false`: no such space.
    pub fn move_space_to_group(&mut self, path: &Path, group: Option<u32>) -> bool {
        let Some(f) = self.spaces.iter().position(|s| s.path == path) else {
            return false;
        };
        let group = group.filter(|id| self.groups.iter().any(|g| g.id == *id));
        let mut s = self.spaces.remove(f);
        s.group = group;
        let at = self
            .spaces
            .iter()
            .position(|o| self.group_of(o).map(|g| g.id) == group)
            .unwrap_or(f);
        self.spaces.insert(at, s);
        true
    }

    /// Add a group named `name`, below the others. Returns its id.
    pub fn add_group(&mut self, name: &str) -> u32 {
        let id = self
            .groups
            .iter()
            .map(|g| g.id)
            .max()
            .map_or(1, |m| m.saturating_add(1));
        self.groups.push(Group {
            id,
            name: name.into(),
            collapsed: false,
        });
        id
    }

    /// Remove group `id`. Its spaces stay, ungrouped.
    pub fn remove_group(&mut self, id: u32) {
        self.groups.retain(|g| g.id != id);
        for s in &mut self.spaces {
            if s.group == Some(id) {
                s.group = None;
            }
        }
    }

    /// Move group `from` to where group `onto` is. `false`: nothing moved.
    pub fn move_group(&mut self, from: u32, onto: u32) -> bool {
        let pos = |id: u32| self.groups.iter().position(|g| g.id == id);
        let (Some(f), Some(t)) = (pos(from), pos(onto)) else {
            return false;
        };
        if f == t {
            return false;
        }
        let g = self.groups.remove(f);
        self.groups.insert(t, g);
        true
    }
}

fn or_default<'a>(mine: &'a str, default: &'a str) -> &'a str {
    if mine.trim().is_empty() {
        default
    } else {
        mine
    }
}

/// Where agent hosts keep their sockets and each agent's state files: `~/.kyde/agents`
/// (`$KYDE_AGENTS_DIR` when set).
#[must_use]
pub fn agents_dir() -> PathBuf {
    // Tests point it at a throwaway folder.
    if let Some(dir) = std::env::var_os("KYDE_AGENTS_DIR") {
        return dir.into();
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kyde").join("agents")
}

/// Scratch's folder, `~/.kyde/scratch` — beside [`agents_dir`] (so tests' throwaway
/// `$KYDE_AGENTS_DIR` moves it too). Each scratch row is a folder in it.
#[must_use]
pub fn scratch_dir() -> PathBuf {
    agents_dir().with_file_name("scratch")
}

/// Where a new linked worktree named `name` for `space` is checked out:
/// `~/.kyde/worktrees/<space folder name>/<name>`.
#[must_use]
pub fn worktree_path(space: &Path, name: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let group = space
        .file_name()
        .map_or_else(|| "space".into(), |n| n.to_string_lossy().into_owned());
    PathBuf::from(home)
        .join(".kyde")
        .join("worktrees")
        .join(group)
        .join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_keeps_spaces_and_fills_defaults() {
        let c: AgentsCfg =
            serde_json::from_str(r#"{"spaces":[{"path":"/r","worktrees":["/w"]}]}"#).unwrap();
        assert_eq!(c.command, "claude");
        assert!(
            !c.setup_done,
            "a pre-setup file still shows the setup overlay"
        );
        assert_eq!(c.cleanup_mode, CleanupMode::Auto);
        assert!(c.cleanup.agent_quit && c.cleanup.merged && c.cleanup.ai_done);
        assert_eq!(c.idle_hours, 24);
        assert_eq!(
            c.space_of(Path::new("/w")).map(|s| s.path.as_path()),
            Some(Path::new("/r"))
        );
    }
}
