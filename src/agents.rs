//! Pure model behind the Agents view (spaces → worktrees → tabs): worktree naming, the
//! per-tab status dot, and which tabs the idle cleanup closes. No gpui / PTY here, so it is
//! unit-testable without spawning a shell — the PTY-backed tabs and the rendering live in
//! `views/agents.rs` (behind the `terminal` feature). A crate-root module, like
//! `term_panel.rs`, so these tests always run.

use std::time::{Duration, Instant};

/// Output within this window marks a tab as working. Agent TUIs repaint their spinner many
/// times a second while busy and go silent when they wait for you.
pub(crate) const BUSY_WINDOW: Duration = Duration::from_millis(1500);

/// The status dot shown for a tab — and, aggregated, for its worktree / space.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub(crate) enum AgentState {
    /// No live tab.
    None,
    /// Live, quiet, nothing new since you last looked.
    Idle,
    /// Produced output while off screen and has gone quiet — done, waiting for you.
    Unseen,
    /// Producing output right now.
    Working,
    /// Waiting on you: a permission prompt or a question (from the Claude Code hook).
    Blocked,
}

/// After a hook said "working", output this quiet means Claude was interrupted (Esc reports
/// no Stop) — its spinner repaints every second while it really works.
pub(crate) const WORKING_STALE: Duration = Duration::from_secs(10);
/// You typed after a permission prompt and it's been this quiet since: you answered it
/// (Esc at a prompt reports nothing either).
pub(crate) const ANSWERED_STALE: Duration = Duration::from_secs(2);

/// What a tab's agent last reported through the Claude Code hook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hook {
    /// Session started, waiting for its first prompt.
    Ready,
    /// Working on a prompt.
    Working,
    /// Waiting on you — why ("Bash permission", "a question").
    Blocked(String),
    /// Finished its turn.
    Done,
    /// The agent exited.
    Ended,
}

/// One hook event: what it means for the tab, the Claude Code session it came from, and the
/// folder the agent is working in (it moves with Claude Code's `EnterWorktree`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HookReport {
    pub hook: Hook,
    pub session: Option<String>,
    pub cwd: Option<std::path::PathBuf>,
}

/// Read one Claude Code hook event (the JSON the hook wrote). `None` for events that don't
/// change the tab's state (e.g. an auth notification) or unreadable text.
pub(crate) fn parse_hook(json: &str) -> Option<HookReport> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let s = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or("");
    let tool = s("tool_name");
    let hook = match s("hook_event_name") {
        "SessionStart" => Hook::Ready,
        "UserPromptSubmit" | "PostToolUse" => Hook::Working,
        // Claude asks before these run — a question for you, not work.
        "PreToolUse" if tool == "AskUserQuestion" => Hook::Blocked("has a question".into()),
        "PreToolUse" if tool == "ExitPlanMode" => Hook::Blocked("plan ready to approve".into()),
        "PreToolUse" => Hook::Working,
        "PermissionRequest" => Hook::Blocked(if tool.is_empty() {
            "needs permission".into()
        } else {
            format!("{tool} permission")
        }),
        "Notification" => match s("notification_type") {
            "permission_prompt" | "elicitation_dialog" => Hook::Blocked(s("message").into()),
            "idle_prompt" => Hook::Done,
            "" if s("message").contains("permission") => Hook::Blocked(s("message").into()),
            _ => return None,
        },
        "Stop" => Hook::Done,
        "SessionEnd" => Hook::Ended,
        _ => return None,
    };
    let session = Some(s("session_id").to_string()).filter(|id| !id.is_empty());
    let cwd = Some(s("cwd")).filter(|c| !c.is_empty()).map(Into::into);
    Some(HookReport { hook, session, cwd })
}

/// Everything a tab's dot is decided from.
#[derive(Clone, Debug, Default)]
pub(crate) struct Signals {
    /// The latest hook report, and when it was written.
    pub hook: Option<(Hook, Instant)>,
    pub last_output: Option<Instant>,
    /// When you last typed into it.
    pub last_input: Option<Instant>,
    /// It finished while you were elsewhere.
    pub unseen: bool,
}

impl AgentState {
    /// A tab's state: the agent's own report (hook) when there is one — sanity-checked
    /// against its output, since an interrupt reports nothing — else the output clock.
    pub(crate) fn of_tab(s: &Signals, now: Instant) -> Self {
        let quiet = |d: Duration| {
            s.last_output
                .is_none_or(|t| now.saturating_duration_since(t) >= d)
        };
        let rest = if s.unseen { Self::Unseen } else { Self::Idle };
        match &s.hook {
            None => Self::of(s.last_output, s.unseen, now),
            Some((Hook::Blocked(_), at)) => {
                if s.last_input.is_some_and(|i| i > *at) && quiet(ANSWERED_STALE) {
                    rest
                } else {
                    Self::Blocked
                }
            }
            Some((Hook::Working, _)) if !quiet(WORKING_STALE) => Self::Working,
            Some(_) => rest,
        }
    }

    /// How urgently it wants you, for "needs attention first" ordering: needs input, then
    /// finished-unseen, then working, idle, nothing.
    pub(crate) fn urgency(self) -> u8 {
        match self {
            Self::Blocked => 4,
            Self::Unseen => 3,
            Self::Working => 2,
            Self::Idle => 1,
            Self::None => 0,
        }
    }

    /// The row's status word ("" when the dot says it all — working is just the blue dot).
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Blocked => "needs input",
            Self::Unseen => "done",
            Self::Working | Self::Idle | Self::None => "",
        }
    }
    /// A tab's state from its clock: working while output is fresh, else unseen/idle.
    pub(crate) fn of(last_output: Option<Instant>, unseen: bool, now: Instant) -> Self {
        if last_output.is_some_and(|t| now.saturating_duration_since(t) < BUSY_WINDOW) {
            Self::Working
        } else if unseen {
            Self::Unseen
        } else {
            Self::Idle
        }
    }

    /// The most attention-worthy of several tabs' states (needs input > working > unseen >
    /// idle > none).
    pub(crate) fn rollup(states: impl IntoIterator<Item = Self>) -> Self {
        states.into_iter().max().unwrap_or(Self::None)
    }
}

/// What the cleanup knows about one tab when it decides.
#[derive(Clone, Debug, Default)]
pub(crate) struct TabFacts {
    /// It's the tab on screen (never closed).
    pub on_screen: bool,
    /// It finished while you were away and you haven't looked yet (kept until you do).
    pub unseen: bool,
    /// Since its last output or last time on screen.
    pub idle: Duration,
    /// Nothing runs in its shell — the agent quit (or a shell sits at its prompt).
    pub at_prompt: bool,
    /// Nothing was ever typed into it (no prompt, no seeded task).
    pub unused: bool,
    /// Its worktree's branch has a merged pull request.
    pub merged: bool,
    /// Its worktree has no uncommitted changes.
    pub clean: bool,
    /// The AI check said its work is done — and why.
    pub ai_done: Option<String>,
}

/// Why `f`'s tab may be closed under `rules` (`None` = keep it). Every rule waits for the tab
/// to be quiet for `rules.quiet()` first; the tab on screen and unseen output are always kept.
pub(crate) fn closable(
    f: &TabFacts,
    rules: kyde_config::agents::CleanupRules,
    idle_hours: u32,
) -> Option<String> {
    if f.on_screen || f.unseen || f.idle < rules.quiet() {
        return None;
    }
    if rules.agent_quit && f.at_prompt {
        return Some("the agent has quit".into());
    }
    if rules.unused && f.unused {
        return Some("never used".into());
    }
    if rules.merged && f.merged {
        return Some("its pull request is merged".into());
    }
    if let (true, Some(why)) = (rules.ai_done, &f.ai_done) {
        return Some(format!("done — {why}"));
    }
    let idle_for = Duration::from_secs(u64::from(idle_hours.clamp(1, 720)) * 60 * 60);
    if rules.idle && f.clean && f.idle >= idle_for {
        return Some(format!("idle {idle_hours}h+"));
    }
    None
}

/// One tab's AI check: (worktree, spawn time), its last output when asked, `Some(why)` if done.
pub(crate) type AiAnswer = (
    (std::path::PathBuf, std::time::SystemTime),
    Option<Instant>,
    Option<String>,
);

/// What the cleanup's background probe found, for the foreground to decide with. Tabs are
/// keyed by (worktree, spawn time).
#[derive(Default)]
pub(crate) struct SweepFound {
    /// Per tab: nothing runs in its shell any more.
    pub at_prompt: Vec<((std::path::PathBuf, std::time::SystemTime), bool)>,
    /// Per worktree: no uncommitted changes.
    pub clean: std::collections::HashMap<std::path::PathBuf, bool>,
    /// Per space whose cache was stale: its merged PRs' head branches.
    pub merged: Vec<(std::path::PathBuf, std::collections::HashSet<String>)>,
    /// Per tab the AI was asked about: its last output when asked, and `Some(why)` if done.
    pub ai: Vec<AiAnswer>,
}

/// Whether nothing runs under shell `pid` — the agent it started has quit (or it's a shell at
/// its prompt). `pgrep -P` lists its children. Spawns a process: call it off the UI thread.
pub(crate) fn shell_is_idle(pid: u32) -> bool {
    std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .is_ok_and(|o| o.stdout.iter().all(u8::is_ascii_whitespace))
}

/// The `claude` CLI: the usual install spots first (a GUI-launched app has a bare `PATH`),
/// else `claude` on `PATH`.
fn claude_bin() -> std::path::PathBuf {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    home.iter()
        .map(|h| h.join(".local/bin/claude"))
        .chain(home.iter().map(|h| h.join(".claude/local/claude")))
        .chain(["/opt/homebrew/bin/claude", "/usr/local/bin/claude"].map(Into::into))
        .find(|p| p.exists())
        .unwrap_or_else(|| "claude".into())
}

/// The AI check's question about one tab, from what's known of it.
pub(crate) fn done_prompt(
    title: &str,
    task: &str,
    latest: &str,
    git: &str,
    terminal_tail: &str,
) -> String {
    format!(
        "You're deciding whether an AI coding agent's terminal tab can be closed. Done means: \
         its task is finished (or abandoned), nothing is waiting on the user (no question, \
         approval or decision pending), and its work is committed. A session with no real \
         task in it (only slash commands like /caveman or /resume, a greeting, nothing \
         asked) is done too. Not done: mid-task, waiting for an answer, or uncommitted \
         unfinished work.\n\n\
         Title: {title}\nTask: {task}\nLatest prompt: {latest}\nGit: {git}\n\
         Last lines of the terminal:\n{terminal_tail}\n\n\
         Reply with ONLY JSON: {{\"done\": true|false, \"reason\": \"<8 words>\"}}"
    )
}

/// Ask a small model (`claude -p --model haiku`) whether a tab's work is done: `Some(reason)`
/// when it says so, `None` when not done or when the check couldn't run. Spawns `claude`
/// (a few seconds): call it off the UI thread.
pub(crate) fn ai_says_done(prompt: &str) -> Option<String> {
    let out = std::process::Command::new(claude_bin())
        // Outside any project, so no CLAUDE.md / settings are loaded for the check.
        .current_dir(std::env::temp_dir())
        .args(["-p", "--model", "haiku", prompt])
        .output()
        .ok()?;
    parse_verdict(&String::from_utf8_lossy(&out.stdout))
}

/// `Some(reason)` when the model's reply is `{"done": true, "reason": …}` (fenced or not).
pub(crate) fn parse_verdict(reply: &str) -> Option<String> {
    let json = &reply[reply.find('{')?..=reply.rfind('}')?];
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.get("done")?.as_bool()?.then(|| {
        v.get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("finished")
            .trim()
            .to_string()
    })
}

/// A worktree's git state, as its sidebar row shows it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct WtGit {
    /// Files with uncommitted changes (incl. untracked).
    pub changed: usize,
    /// Commits not yet pushed (a new branch counts from its fork point on the remote mainline).
    pub ahead: usize,
    /// Its commits are unpushed but their changes are all on the remote mainline already
    /// (squash-merged elsewhere) — finished, nothing to push.
    pub landed: bool,
}

/// Read worktree `wt`'s branch + [`WtGit`]; `None` outside git. Runs git: call it off the
/// UI thread.
pub(crate) fn read_git(wt: &std::path::Path) -> Option<(Option<String>, WtGit)> {
    let repo = kyde_git::Repo::discover(wt).ok()?;
    let mut g = WtGit {
        changed: repo.status().map_or(0, |files| files.len()),
        ahead: repo.ahead_count().unwrap_or(0),
        landed: false,
    };
    if g.ahead > 0 && repo.remote_default().is_some_and(|m| repo.landed_on(&m)) {
        g = WtGit {
            ahead: 0,
            landed: true,
            ..g
        };
    }
    Some((repo.current_branch(), g))
}

/// One `git worktree list` for `space`'s repo: the space's own branch, then every OTHER
/// worktree (the main checkout and linked ones alike, whoever made them) with its branch, in
/// git's order. `None` outside git. Runs git: call it off the UI thread.
pub(crate) type RepoWorktrees = (Option<String>, Vec<(std::path::PathBuf, Option<String>)>);
pub(crate) fn repo_worktrees(space: &std::path::Path) -> Option<RepoWorktrees> {
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let me = canon(space);
    let mut own = None;
    let mut others = Vec::new();
    for w in kyde_git::Repo::discover(space).ok()?.worktrees().ok()? {
        if canon(&w.path) == me {
            own = w.branch;
        } else {
            others.push((w.path, w.branch));
        }
    }
    Some((own, others))
}

/// `f` for many worktrees at once — one short-lived thread each (independent git processes /
/// files, so a dozen worktrees cost about one). Blocks: call it off the UI thread.
pub(crate) fn per_worktree<T: Send>(
    wts: Vec<std::path::PathBuf>,
    f: fn(&std::path::Path) -> T,
) -> Vec<(std::path::PathBuf, T)> {
    std::thread::scope(|s| {
        let jobs: Vec<_> = wts
            .into_iter()
            .map(|wt| {
                s.spawn(move || {
                    let r = f(&wt);
                    (wt, r)
                })
            })
            .collect();
        jobs.into_iter().filter_map(|j| j.join().ok()).collect()
    })
}

/// What a worktree's agent is doing, as Claude Code itself records it: its running AI-written
/// title (rewritten as the task changes) and the prompts — read from the newest session file
/// in `~/.claude/projects/<worktree>/`. No instructions to the agent needed.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct AgentContext {
    /// The session's latest AI-written title.
    pub title: Option<String>,
    /// The first prompt you typed (the task it was started with).
    pub first_prompt: Option<String>,
    /// The latest prompt.
    pub last_prompt: Option<String>,
    /// When the session was last written.
    pub updated: Option<std::time::SystemTime>,
    /// The session's id — what `--resume` reopens.
    pub session: Option<String>,
}

/// Claude Code's project folder for a working directory: `~/.claude/projects/` + the path
/// with every character outside `[A-Za-z0-9-]` turned into `-`.
pub(crate) fn claude_project_dir(cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let enc: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    Some(
        std::path::PathBuf::from(home)
            .join(".claude")
            .join("projects")
            .join(enc),
    )
}

/// Claude Code sessions in `wt`'s project dir written at/after `since`: `(id, created)`.
/// File IO: call it off the UI thread.
pub(crate) fn sessions_since(
    wt: &std::path::Path,
    since: std::time::SystemTime,
) -> Vec<(String, std::time::SystemTime)> {
    let Some(Ok(dir)) = claude_project_dir(wt).map(std::fs::read_dir) else {
        return Vec::new();
    };
    dir.filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "jsonl") {
                return None;
            }
            let meta = e.metadata().ok()?;
            let modified = meta.modified().ok()?;
            (modified >= since).then(|| {
                let id = path.file_stem()?.to_string_lossy().into_owned();
                Some((id, meta.created().unwrap_or(modified)))
            })?
        })
        .collect()
}

/// Match agent tabs to the sessions they started: each tab (by spawn time) takes the
/// unclaimed candidate created closest to — and not before — its spawn. `None` = no match yet.
pub(crate) fn assign_sessions(
    spawns: &[std::time::SystemTime],
    candidates: &[(String, std::time::SystemTime)],
    claimed: &std::collections::HashSet<String>,
) -> Vec<Option<String>> {
    let mut taken = claimed.clone();
    spawns
        .iter()
        .map(|&spawn| {
            let pick = candidates
                .iter()
                .filter(|(id, created)| *created >= spawn && !taken.contains(id))
                .min_by_key(|(_, created)| created.duration_since(spawn).unwrap_or_default())
                .map(|(id, _)| id.clone());
            if let Some(id) = &pick {
                taken.insert(id.clone());
            }
            pick
        })
        .collect()
}

/// Bytes read from a session file's start (first prompt) and end (latest title/prompt) —
/// session files run to megabytes; these ends hold what we need.
const HEAD_BYTES: u64 = 128 * 1024;
const TAIL_BYTES: u64 = 256 * 1024;

/// [`AgentContext`] for worktree `wt`'s newest Claude Code session; `None` without one.
/// Reads only the file's head and tail. File IO: call it off the UI thread.
pub(crate) fn read_agent_context(wt: &std::path::Path) -> Option<AgentContext> {
    let (updated, path) = std::fs::read_dir(claude_project_dir(wt)?)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(t, _)| *t)?;
    let tail = read_end(&path, TAIL_BYTES, true);
    let head = read_end(&path, HEAD_BYTES, false);
    let mut ctx = context_from(&head, &tail, Some(updated));
    ctx.session = path.file_stem().map(|s| s.to_string_lossy().into_owned());
    Some(ctx)
}

/// Parse the context out of a session file's `head` and `tail` (JSONL; a line cut by the
/// byte window just fails to parse and is skipped).
pub(crate) fn context_from(
    head: &str,
    tail: &str,
    updated: Option<std::time::SystemTime>,
) -> AgentContext {
    let latest = |ty: &str, key: &str| {
        tail.lines()
            .rev()
            .filter(|l| l.contains(&format!("\"type\":\"{ty}\"")))
            .find_map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).ok()?;
                let s = v.get(key)?.as_str()?.trim();
                (!s.is_empty()).then(|| s.to_string())
            })
    };
    AgentContext {
        title: latest("ai-title", "aiTitle"),
        last_prompt: latest("last-prompt", "lastPrompt"),
        first_prompt: head
            .lines()
            .filter(|l| l.contains("\"type\":\"user\""))
            .find_map(human_prompt),
        updated,
        session: None,
    }
}

/// The text of a session line if it's a prompt a human typed — not a slash command, a
/// skill's injected text (`isMeta`), or a tool result.
fn human_prompt(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("isMeta").and_then(serde_json::Value::as_bool) == Some(true) {
        return None;
    }
    let content = v.get("message")?.get("content")?;
    let text = match content {
        serde_json::Value::String(s) => s.as_str(),
        serde_json::Value::Array(parts) => parts.iter().find_map(|p| {
            (p.get("type")?.as_str()? == "text")
                .then(|| p.get("text")?.as_str())
                .flatten()
        })?,
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<')).then(|| text.to_string())
}

/// Up to `n` bytes from the start (or the `end`) of a file, lossily as text; empty on error.
fn read_end(path: &std::path::Path, n: u64, end: bool) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let start = if end { len.saturating_sub(n) } else { 0 };
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.take(n).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Where a space's own checkout stands against the remote mainline after [`sync_main`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum MainSync {
    /// Fast-forwarded to (or already at) this remote branch, e.g. `origin/main`.
    Synced(String),
    /// Checked out on another branch — left alone.
    OffBranch(String),
    /// Couldn't sync — git's reason (offline, local changes, diverged…).
    Failed(String),
}

/// Keep a space's own checkout (its default worktree) up to date with the remote mainline:
/// fetch `origin/<default>` and fast-forward onto it — only when that checkout is ON the
/// default branch and clean (a feature branch or work in progress is left alone). ff-only:
/// never a merge commit, never loses work. `None` outside git / without a remote mainline.
/// Runs git + network: call it off the UI thread.
pub(crate) fn sync_main(space: &std::path::Path) -> Option<MainSync> {
    let repo = kyde_git::Repo::discover(space).ok()?;
    let remote = repo.remote_default()?;
    let local = remote.split_once('/').map_or(remote.as_str(), |(_, b)| b);
    let current = repo.current_branch();
    if current.as_deref() != Some(local) {
        return Some(MainSync::OffBranch(
            current.unwrap_or_else(|| "a detached HEAD".into()),
        ));
    }
    if !repo.status().is_ok_and(|f| f.is_empty()) {
        return Some(MainSync::Failed("uncommitted changes".into()));
    }
    let res = repo
        .fetch_remote_branch(&remote)
        .and_then(|()| repo.fast_forward(&remote));
    Some(match res {
        Ok(()) => MainSync::Synced(remote),
        Err(e) => MainSync::Failed(git_reason(&e.to_string()).to_string()),
    })
}

/// git's own reason from a (possibly multi-line) `GitError` message, for a one-line note:
/// the first non-empty line, minus the `git [argv] failed:` / `fatal:` prefixes.
pub(crate) fn git_reason(e: &str) -> &str {
    let line = e
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(e);
    let line = line.split_once(" failed: ").map_or(line, |(_, why)| why);
    line.strip_prefix("fatal: ").unwrap_or(line)
}

/// Whether a worktree has no uncommitted changes (tracked or untracked) — the cleanup only
/// closes idle tabs there, so work in progress keeps its agent. A folder outside git counts
/// as clean; a failing `git status` doesn't. Runs git: call it off the UI thread.
pub(crate) fn worktree_is_clean(wt: &std::path::Path) -> bool {
    match kyde_git::Repo::discover(wt) {
        Err(_) => true,
        Ok(repo) => repo.status().is_ok_and(|files| files.is_empty()),
    }
}

/// Folder + branch for an issue's worktree, like the `is` skill: `<type>/<n>-<slug>` with a
/// conventional-commit type (`fix` for bug-ish issues, else `feat`) and a 3–5-word slug.
pub(crate) fn issue_branch(number: u64, title: &str, labels: &[String]) -> (String, String) {
    let lower = title.to_lowercase();
    let buggy = labels.iter().any(|l| l.to_lowercase().contains("bug"))
        || ["fix", "bug", "broken", "crash", "error", "regression"]
            .iter()
            .any(|w| lower.contains(w));
    let slug = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("-");
    let folder = if slug.is_empty() {
        format!("{number}")
    } else {
        format!("{number}-{slug}")
    };
    let branch = format!("{}/{folder}", if buggy { "fix" } else { "feat" });
    (folder, branch)
}

/// `s` as one single-quoted shell word (`'` → `'\''`), for typing a seed prompt after the
/// start command.
pub(crate) fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Seconds since the Unix epoch for an ISO-8601 UTC time (`2026-10-29T14:38:38Z`).
pub(crate) fn iso_secs(iso: &str) -> Option<i64> {
    let (date, time) = iso.trim().split_once('T')?;
    let mut d = date.split('-').map(str::parse::<i64>);
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut tm = time
        .trim_end_matches('Z')
        .split(':')
        .map(|x| x.split('.').next().unwrap_or("0").parse::<i64>());
    let (hh, mm) = (tm.next()?.ok()?, tm.next()?.ok()?);
    let ss = tm.next().and_then(Result::ok).unwrap_or(0);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// "just now" / "5 minutes ago" / "yesterday" / "last week" / "3 months ago" for an age.
pub(crate) fn ago_text(secs: i64) -> String {
    let n = |unit: i64| secs / unit;
    let s = |k: i64| if k == 1 { "" } else { "s" };
    match secs {
        i64::MIN..60 => "just now".into(),
        60..3600 => format!("{} minute{} ago", n(60), s(n(60))),
        3600..86_400 => format!("{} hour{} ago", n(3600), s(n(3600))),
        86_400..172_800 => "yesterday".into(),
        172_800..604_800 => format!("{} days ago", n(86_400)),
        604_800..1_209_600 => "last week".into(),
        1_209_600..2_592_000 => format!("{} weeks ago", n(604_800)),
        2_592_000..31_536_000 => format!("{} month{} ago", n(2_592_000), s(n(2_592_000))),
        _ => format!("{} year{} ago", n(31_536_000), s(n(31_536_000))),
    }
}

/// How long ago an ISO-8601 UTC time was ("" if it doesn't parse).
pub(crate) fn iso_ago(iso: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    iso_secs(iso).map_or_else(String::new, |t| ago_text(now - t))
}

const ADJECTIVES: &[&str] = &[
    "amber", "brave", "calm", "crisp", "dusty", "eager", "fancy", "gentle", "golden", "hidden",
    "icy", "jolly", "keen", "lively", "lucky", "mellow", "misty", "noble", "polar", "quiet",
    "rapid", "rustic", "silver", "solar", "steady", "swift", "tidy", "vivid", "wild", "witty",
    "young", "zesty",
];
const NOUNS: &[&str] = &[
    "brook", "canyon", "cedar", "cloud", "comet", "creek", "dune", "ember", "fern", "field",
    "forest", "glade", "grove", "harbor", "island", "lake", "leaf", "meadow", "mesa", "moon",
    "oak", "orchard", "peak", "pine", "river", "sky", "stone", "storm", "summit", "valley",
    "willow", "wind",
];

/// A friendly, branch-safe worktree name (`rapid-meadow-442e`) derived from `seed`.
pub(crate) fn worktree_name(seed: u64) -> String {
    // `seed % len` < len, so the casts can't truncate.
    #[allow(clippy::cast_possible_truncation)]
    let pick = |words: &[&'static str], s: u64| words[(s % words.len() as u64) as usize];
    format!(
        "{}-{}-{:04x}",
        pick(ADJECTIVES, seed),
        pick(NOUNS, seed >> 8),
        (seed >> 16) & 0xffff
    )
}

/// A fresh seed for [`worktree_name`] — std's per-process random hasher keys mixed with the
/// clock, so two worktrees made in the same second still differ.
/// Agent tabs run in agent hosts (they outlive Kyde) — except under test, while taking
/// screenshots, or with `KYDE_LOCAL_AGENTS` set (each tab then owns its terminal, as before).
pub(crate) fn hosted() -> bool {
    !cfg!(test)
        && std::env::var_os("KYDE_LOCAL_AGENTS").is_none()
        // Screenshot runs: nothing may outlive the shot.
        && std::env::var_os("KYDE_SHOT").is_none()
}

/// The environment every agent tab's shell gets: where its Claude Code hook reports, who it
/// is, this binary, and a capable terminal.
pub(crate) fn tab_env(id: &str) -> Vec<(String, String)> {
    let exe: std::path::PathBuf = std::env::current_exe().unwrap_or_default();
    vec![
        (
            "KYDE_AGENT_STATE".into(),
            crate::agents::state_path(id).to_string_lossy().into_owned(),
        ),
        ("KYDE_AGENT_ID".into(), id.into()),
        ("KYDE_BIN".into(), exe.to_string_lossy().into_owned()),
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
    ]
}

/// Where tab `id`'s Claude Code hook writes its latest event.
pub(crate) fn state_path(id: &str) -> std::path::PathBuf {
    kyde_config::agents::agents_dir().join(format!("{id}.state"))
}

/// Kyde's note for tab `id`'s first prompt (`$KYDE_AGENT_STATE.brief` — the prompt hook
/// hands it to Claude, then deletes it; see `kyde_config::claude_hooks::brief_command`).
pub(crate) fn brief_path(id: &str) -> std::path::PathBuf {
    kyde_config::agents::agents_dir().join(format!("{id}.state.brief"))
}

/// What's already going on in a worktree when a new agent tab opens there.
#[derive(Clone, Debug, Default)]
pub(crate) struct Busy {
    pub branch: Option<String>,
    /// Files with uncommitted changes.
    pub changed: usize,
    /// Commits not yet pushed.
    pub ahead: usize,
    /// It's a project's own checkout, switched off the mainline onto another branch.
    pub off_main: bool,
    /// What the worktree's other (used) agent tabs are on — their titles.
    pub others: Vec<String>,
    /// What its latest Claude Code session was about (title, else latest prompt).
    pub last_task: Option<String>,
}

/// The note a new agent tab's first prompt carries (see [`brief_path`]): what's in its
/// worktree, and — per `mode` — to move an unrelated task to a worktree of its own (Claude
/// Code's `EnterWorktree`; the tab follows, see [`moved_worktree`]) or to ask first. `None`
/// when the worktree is free (no changes, nothing unpushed, no other agent at work, on its
/// mainline) or `mode` is Stay.
pub(crate) fn task_brief(b: &Busy, mode: kyde_config::agents::NewTask) -> Option<String> {
    use kyde_config::agents::NewTask;
    let free = b.changed == 0 && b.ahead == 0 && !b.off_main && b.others.is_empty();
    if free || mode == NewTask::Stay {
        return None;
    }
    let count = |n: usize, what: &str| match n {
        0 => None,
        1 => Some(format!("1 {what}")),
        n => Some(format!("{n} {what}s")),
    };
    let git: Vec<String> = [
        count(b.changed, "uncommitted file"),
        count(b.ahead, "unpushed commit"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut facts = Vec::new();
    match (&b.branch, git.is_empty()) {
        (Some(br), true) => facts.push(format!("- branch `{br}`")),
        (Some(br), false) => facts.push(format!("- branch `{br}`: {}", git.join(", "))),
        (None, false) => facts.push(format!("- {}", git.join(", "))),
        (None, true) => {}
    }
    if b.others.is_empty() {
        if let Some(t) = &b.last_task {
            facts.push(format!("- its last agent worked on: \"{t}\""));
        }
    } else {
        let others: Vec<String> = b.others.iter().map(|t| format!("\"{t}\"")).collect();
        facts.push(format!("- other agents here are on: {}", others.join(", ")));
    }
    let what = if mode == NewTask::Ask {
        "The user has set Kyde to ask before moving a task. If this request starts a new task \
         unrelated to that work, ask the user (one short question) whether to move it to a \
         worktree of its own before you change any file; if they agree, call EnterWorktree"
    } else {
        "The user has set Kyde to give unrelated tasks a worktree of their own. If this request \
         starts a new task unrelated to that work, call EnterWorktree"
    };
    Some(format!(
        "Kyde (the agent manager running this tab): this worktree already has work in it —\n\
         {}\n\n{what} (name: a short slug for the new task) before you change any file — Kyde \
         moves this tab there. If it's related, a follow-up, a question or chit-chat, stay here \
         and ignore this note.\n",
        facts.join("\n")
    ))
}

/// Where a tab's agent has moved itself, from the folder its hook reported: the worktree
/// holding `cwd` when that's ANOTHER worktree of `wt`'s repository (Claude Code's
/// `EnterWorktree`) — canonical; `None` while it's in `wt` (or a folder inside it), or outside
/// the repository. Stats a few folders, and runs git only when the worktree differs: call it
/// off the UI thread.
pub(crate) fn moved_worktree(
    wt: &std::path::Path,
    cwd: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let cwd = canon(cwd);
    let root = cwd.ancestors().find(|d| d.join(".git").exists())?;
    if root == canon(wt) {
        return None;
    }
    let common = |p: &std::path::Path| kyde_git::Repo::discover(p).ok()?.common_dir().ok();
    (common(root)? == common(wt)?).then(|| root.to_path_buf())
}

/// Keep Claude Code's worktrees (`.claude/worktrees/`, inside the checkout) out of git's
/// changes — in the repository's own `info/exclude`, never a tracked file — so the checkout
/// a task moved out of doesn't read as dirty (which would hold back its main sync and
/// cleanup). Adds the line once. Runs git + file IO: call it off the UI thread.
pub(crate) fn exclude_claude_worktrees(wt: &std::path::Path) {
    const LINE: &str = ".claude/worktrees/";
    let Some(dir) = kyde_git::Repo::discover(wt)
        .ok()
        .and_then(|r| r.common_dir().ok())
    else {
        return;
    };
    let path = dir.join("info").join("exclude");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    if text.lines().any(|l| l.trim() == LINE) {
        return;
    }
    let sep = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let _ = std::fs::create_dir_all(dir.join("info"));
    let _ = std::fs::write(&path, format!("{text}{sep}{LINE}\n"));
}

/// A project's worktrees after git lists `listed`: the ones still there keep `current`'s
/// order (you may have dragged them), new ones follow in git's order.
pub(crate) fn keep_order(
    current: &[std::path::PathBuf],
    listed: Vec<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = current
        .iter()
        .filter(|w| listed.contains(w))
        .cloned()
        .collect();
    for w in listed {
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// A new agent tab's id (12 hex digits): names its state file and agent host.
pub(crate) fn tab_id() -> String {
    format!("{:012x}", random_seed() & 0xffff_ffff_ffff)
}

pub(crate) fn random_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        h.write_u128(d.as_nanos());
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_reads_latest_title_and_first_human_prompt() {
        let head = concat!(
            r#"{"type":"user","origin":{"kind":"human"},"message":{"content":"<command-name>/caveman</command-name>"}}"#,
            "\n",
            r#"{"type":"user","isMeta":true,"message":{"content":[{"type":"text","text":"skill text"}]}}"#,
            "\n",
            r#"{"type":"user","origin":{"kind":"human"},"message":{"content":[{"type":"text","text":"build an AI tab"},{"type":"image"}]}}"#,
            "\n",
        );
        let tail = concat!(
            r#"ype":"cut off by the byte window"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Old title"}"#,
            "\n",
            r#"{"type":"last-prompt","lastPrompt":"continue"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"AI tab IDE interface"}"#,
            "\n",
        );
        let c = context_from(head, tail, None);
        assert_eq!(c.title.as_deref(), Some("AI tab IDE interface"));
        assert_eq!(c.first_prompt.as_deref(), Some("build an AI tab"));
        assert_eq!(c.last_prompt.as_deref(), Some("continue"));
        assert_eq!(
            claude_project_dir(std::path::Path::new("/Users/k/.herdr/wt-a"))
                .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            Some(Some("-Users-k--herdr-wt-a".into()))
        );
    }

    #[test]
    fn issue_branch_follows_the_is_skill_convention() {
        assert_eq!(
            issue_branch(1234, "Null invoice date crashes PDF export!", &[]),
            (
                "1234-null-invoice-date-crashes-pdf".into(),
                "fix/1234-null-invoice-date-crashes-pdf".into()
            )
        );
        assert_eq!(
            issue_branch(5678, "CSV export", &["enhancement".into()]).1,
            "feat/5678-csv-export"
        );
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn sessions_go_to_the_tab_that_started_them() {
        use std::time::{Duration, UNIX_EPOCH};
        let at = |s| UNIX_EPOCH + Duration::from_secs(s);
        let found = [
            ("old".to_string(), at(50)), // before both tabs
            ("b".to_string(), at(210)),
            ("a".to_string(), at(110)),
        ];
        let claimed = std::collections::HashSet::new();
        assert_eq!(
            assign_sessions(&[at(100), at(200), at(300)], &found, &claimed),
            vec![Some("a".into()), Some("b".into()), None]
        );
    }

    #[test]
    fn hook_events_drive_the_dot_and_an_interrupt_falls_back_to_output() {
        let ev = |json: &str| parse_hook(json).map(|r| r.hook);
        assert_eq!(
            parse_hook(r#"{"hook_event_name":"Stop","session_id":"s1"}"#),
            Some(HookReport {
                hook: Hook::Done,
                session: Some("s1".into()),
                cwd: None,
            })
        );
        assert_eq!(
            ev(r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#),
            Some(Hook::Blocked("Bash permission".into()))
        );
        assert_eq!(
            ev(r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion"}"#),
            Some(Hook::Blocked("has a question".into()))
        );
        assert_eq!(
            ev(r#"{"hook_event_name":"Notification","notification_type":"auth_success"}"#),
            None
        );

        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut s = Signals {
            hook: Some((Hook::Working, at(0))),
            last_output: Some(at(5)),
            ..Signals::default()
        };
        assert_eq!(AgentState::of_tab(&s, at(6)), AgentState::Working);
        // Esc mid-turn: no Stop, but the spinner stopped → not working any more.
        assert_eq!(AgentState::of_tab(&s, at(20)), AgentState::Idle);
        // A permission prompt holds until you answer it…
        s.hook = Some((Hook::Blocked("Bash permission".into()), at(10)));
        assert_eq!(AgentState::of_tab(&s, at(60)), AgentState::Blocked);
        // …typing after it, then quiet, means you did (even if Claude reported nothing).
        s.last_input = Some(at(61));
        assert_eq!(AgentState::of_tab(&s, at(70)), AgentState::Idle);
    }

    #[test]
    fn a_new_tab_hears_about_a_busy_worktree_only() {
        use kyde_config::agents::NewTask;
        let free = Busy {
            branch: Some("main".into()),
            last_task: Some("old work".into()),
            ..Busy::default()
        };
        assert_eq!(task_brief(&free, NewTask::Move), None);
        let busy = Busy {
            branch: Some("feat/linux".into()),
            changed: 3,
            ahead: 1,
            others: vec!["Linux AppImage packaging".into()],
            ..free
        };
        let note = task_brief(&busy, NewTask::Move).unwrap();
        assert!(note.contains("branch `feat/linux`: 3 uncommitted files, 1 unpushed commit"));
        assert!(note.contains("\"Linux AppImage packaging\""));
        assert!(note.contains("call EnterWorktree") && !note.contains("ask the user"));
        assert!(task_brief(&busy, NewTask::Ask)
            .unwrap()
            .contains("ask the user"));
        assert_eq!(task_brief(&busy, NewTask::Stay), None);
    }

    #[test]
    fn an_agent_that_enters_another_worktree_is_found_there() {
        let base = std::env::temp_dir().join(format!("kyde-moved-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8(out.stdout).unwrap()
        };
        git(&["init", "-q"]);
        std::fs::write(repo.join("src/a.rs"), "").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ]);
        // Where Claude Code's EnterWorktree puts one.
        git(&[
            "worktree",
            "add",
            "-q",
            ".claude/worktrees/style",
            "-b",
            "style",
        ]);
        let moved = repo.join(".claude/worktrees/style").canonicalize().unwrap();
        let hook = |cwd: &std::path::Path| {
            let json = serde_json::json!({ "hook_event_name": "Stop", "cwd": cwd }).to_string();
            parse_hook(&json).and_then(|r| r.cwd).unwrap()
        };
        // Working in a folder of its own worktree isn't a move; entering the other one is.
        assert_eq!(moved_worktree(&repo, &hook(&repo.join("src"))), None);
        assert_eq!(
            moved_worktree(&repo, &hook(&moved.join("src"))),
            Some(moved.clone())
        );
        // Another repository entirely isn't one of its worktrees.
        assert_eq!(moved_worktree(&base.join("elsewhere"), &moved), None);
        // The worktree left behind doesn't read as dirty for it.
        assert!(git(&["status", "--porcelain"]).contains(".claude/"));
        exclude_claude_worktrees(&repo);
        exclude_claude_worktrees(&repo);
        assert_eq!(git(&["status", "--porcelain"]), "");
        let exclude = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
        assert_eq!(exclude.matches(".claude/worktrees/").count(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_dragged_worktree_order_survives_git_refreshes() {
        let p = |s: &str| std::path::PathBuf::from(s);
        // You put c first; git lists a, b, c (alphabetical), b was removed, d is new.
        let mine = [p("c"), p("a"), p("b")];
        assert_eq!(
            keep_order(&mine, vec![p("a"), p("c"), p("d")]),
            [p("c"), p("a"), p("d")]
        );
    }

    #[test]
    fn iso_times_read_as_ages() {
        assert_eq!(iso_secs("1970-01-02T00:00:00Z"), Some(86_400));
        assert_eq!(iso_secs("2000-03-01T12:00:00Z"), Some(951_912_000));
        assert_eq!(ago_text(30), "just now");
        assert_eq!(ago_text(2 * 3600), "2 hours ago");
        assert_eq!(ago_text(90_000), "yesterday");
        assert_eq!(ago_text(9 * 86_400), "last week");
    }

    #[test]
    fn cleanup_rules_close_finished_tabs_but_keep_visible_and_unseen() {
        let rules = kyde_config::agents::CleanupRules::default();
        let quiet = TabFacts {
            idle: Duration::from_hours(1),
            ..TabFacts::default()
        };
        let quit = TabFacts {
            at_prompt: true,
            ..quiet.clone()
        };
        assert_eq!(
            closable(&quit, rules, 24).as_deref(),
            Some("the agent has quit")
        );
        // Never the tab on screen, never output you haven't seen, never while still busy.
        for keep in [
            TabFacts {
                on_screen: true,
                ..quit.clone()
            },
            TabFacts {
                unseen: true,
                ..quit.clone()
            },
            TabFacts {
                idle: Duration::from_secs(5),
                ..quit.clone()
            },
        ] {
            assert_eq!(closable(&keep, rules, 24), None);
        }
        let unused = TabFacts {
            unused: true,
            ..quiet.clone()
        };
        assert_eq!(closable(&unused, rules, 24).as_deref(), Some("never used"));
        let merged = TabFacts {
            merged: true,
            ..quiet.clone()
        };
        assert!(closable(&merged, rules, 24).is_some());
        let ai = TabFacts {
            ai_done: Some("PR merged".into()),
            ..quiet.clone()
        };
        assert_eq!(
            closable(&ai, rules, 24).as_deref(),
            Some("done — PR merged")
        );
        // A rule switched off doesn't fire; plain quiet isn't enough on its own.
        let off = kyde_config::agents::CleanupRules {
            agent_quit: false,
            ..rules
        };
        assert_eq!(closable(&quit, off, 24), None);
        assert_eq!(closable(&quiet, rules, 24), None);
        assert_eq!(
            parse_verdict("```json\n{\"done\": true, \"reason\": \"merged\"}\n```").as_deref(),
            Some("merged")
        );
        assert_eq!(
            parse_verdict(r#"{"done": false, "reason": "waiting"}"#),
            None
        );
    }
}
