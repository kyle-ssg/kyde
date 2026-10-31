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
}

impl Space {
    /// The sidebar's rows for this space, in order: its linked worktrees with its own
    /// checkout at [`Self::main_pos`].
    ///
    /// ```
    /// use kyde_config::agents::Space;
    /// let s = Space { path: "/r".into(), worktrees: vec!["/a".into(), "/b".into()], main_pos: 1, ..Space::default() };
    /// assert_eq!(s.rows(), ["/a", "/r", "/b"].map(std::path::PathBuf::from));
    /// ```
    #[must_use]
    pub fn rows(&self) -> Vec<PathBuf> {
        let mut rows = self.worktrees.clone();
        rows.insert(self.main_pos.min(rows.len()), self.path.clone());
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
    /// The spaces, in sidebar order.
    #[serde(default)]
    pub spaces: Vec<Space>,
    /// The worktree last on screen — reopened on the next launch.
    #[serde(default)]
    pub selected: Option<PathBuf>,
    /// Each worktree's open tabs, reopened (sessions resumed) when it's next selected.
    #[serde(default)]
    pub tabs: std::collections::BTreeMap<PathBuf, Vec<SavedTab>>,
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
pub const ISSUE_PROMPT: &str = "Work on GitHub issue #{number}: {title} ({url}). Read it, and \
its discussion, with `gh issue view {number} --comments`. You're on branch {branch} in a fresh \
worktree off the latest {base}. If the change is straightforward, implement it with tests and \
check them; if it's bigger or unclear, don't start coding — use plan mode and produce an \
implementation plan first.";

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
            cleanup_mode: CleanupMode::default(),
            cleanup: CleanupRules::default(),
            idle_hours: default_idle_hours(),
            spaces: Vec::new(),
            selected: None,
            tabs: std::collections::BTreeMap::new(),
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
        c
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
    /// worktrees include it.
    #[must_use]
    pub fn space_of(&self, wt: &Path) -> Option<&Space> {
        self.spaces
            .iter()
            .find(|s| s.path == wt || s.worktrees.iter().any(|w| w == wt))
    }
}

fn or_default<'a>(mine: &'a str, default: &'a str) -> &'a str {
    if mine.trim().is_empty() {
        default
    } else {
        mine
    }
}

/// Where agent hosts keep their sockets and each agent's state files: `~/.kyde/agents`.
#[must_use]
pub fn agents_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kyde").join("agents")
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
