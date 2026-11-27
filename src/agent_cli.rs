//! `ky agent …` — the command line agents (and you) use to see and drive Kyde's agents.
//!
//! It talks straight to the agent hosts' sockets and state files in `~/.kyde/agents` (see
//! `agent_host`), so it works whether or not the Kyde window is open. An agent started in a
//! Kyde tab has `KYDE_AGENT_ID` (itself) and `KYDE_BIN` (this binary) in its environment.

use crate::agent_host as host;
use crate::agents::Hook;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The subcommands — `ky agent <one of these>` is the CLI; anything else after `agent` is a
/// folder to open, as before.
pub(crate) const COMMANDS: [&str; 11] = [
    "list", "read", "send", "prompt", "keys", "wait", "new", "move", "restart", "kill", "help",
];

const HELP: &str = "\
ky agent — see and drive the agents running in Kyde (each in its own tab).

Every agent has an id (12 hex digits). You, if you run in a Kyde tab, are $KYDE_AGENT_ID.
Run these as \"$KYDE_BIN\" agent … (or `ky agent …` if Kyde's shell command is installed).

  ky agent list [--json]                 every running agent: id, state, folder (where its
                                         tab is now), name
  ky agent read <id> [--lines N]         its screen + recent scrollback as text (default 80)
  ky agent prompt <id> <text>            type <text> and press Enter (submits a prompt)
  ky agent send <id> <text>              type <text> without Enter
  ky agent keys <id> <key>...            press keys: enter esc tab up down left right
                                         backspace ctrl-c ctrl-d …
  ky agent wait <id> --until <state>[,<state>] [--timeout SECS]
                                         block until it's in a state (exit 1 on timeout):
                                         working, needs-input, done, idle (= ready or done),
                                         ended
  ky agent new [<folder>] [--prompt <text>] [--shell]
                                         start an agent there (default: here) — it shows up
                                         in Kyde when the folder is one of its worktrees
  ky agent move <folder> [--prompt <text>]
                                         move yourself (this tab's agent) to the worktree
                                         <folder> is in, of a project Kyde has: once you
                                         end this turn, Kyde resumes this conversation in
                                         a tab there on <text> (what you'll do next) and
                                         closes this one
  ky agent restart [<id>] [--prompt <text>]
                                         restart an agent (default: yourself) as a fresh
                                         process on the same conversation, in its tab — to
                                         load new settings, MCP servers or a Claude Code
                                         update. Yourself: end this turn, and you resume on
                                         <text> (what you'll do next). Another agent: at
                                         once if it's between turns, else when its turn is
                                         over; it waits for its next prompt unless you
                                         give <text>
  ky agent kill <id>                     end it (its tab closes)

With Kyde closed, an agent runs only while it has work to finish: once it's done (or if it
never gets a prompt) it ends, and Kyde resumes its session when you next open its worktree.

Ids can be shortened to any unique prefix. States come from Claude Code's hook: ready
(waiting for a first prompt), working, needs-input (a permission prompt or a question),
done (finished its turn), ended.
";

/// Run `ky agent <args>`; the process exit code.
pub(crate) fn run(args: &[String]) -> i32 {
    match run_inner(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ky agent: {e}");
            2
        }
    }
}

type Res = Result<i32, String>;

fn run_inner(args: &[String]) -> Res {
    let cmd = args.first().map_or("help", String::as_str);
    let rest = args.get(1..).unwrap_or_default();
    match cmd {
        "list" => list(rest.iter().any(|a| a == "--json")),
        "read" => read(rest),
        "prompt" => type_text(rest, true),
        "send" => type_text(rest, false),
        "keys" => keys(rest),
        "wait" => wait(rest),
        "new" if !asks_help(rest, &["--shell"])? => new(rest),
        "move" if !asks_help(rest, &[])? => move_to(rest),
        "restart" if !asks_help(rest, &[])? => restart(rest),
        "kill" => {
            let id = resolve(rest.first())?;
            host::send(&id, host::KILL, &[]).map_err(|e| e.to_string())?;
            Ok(0)
        }
        _ => {
            print!("{HELP}");
            Ok(0)
        }
    }
}

/// Whether `rest` asks for the help (`--help` / `-h`) — checked before a command that starts
/// something, which takes `--prompt <text>` and `switches`: any other flag is an error, never
/// a reason to go ahead (`agent new --help` once started an agent).
fn asks_help(rest: &[String], switches: &[&str]) -> Result<bool, String> {
    let mut args = rest.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--help" | "-h" => return Ok(true),
            "--prompt" => {
                args.next();
            }
            f if f.starts_with('-') && !switches.contains(&f) => {
                return Err(format!("unknown option {f} (see `ky agent help`)"));
            }
            _ => {}
        }
    }
    Ok(false)
}

/// An agent's id from a full id or a unique prefix.
fn resolve(arg: Option<&String>) -> Result<String, String> {
    let want = arg.ok_or("which agent? (an id from `ky agent list`)")?;
    let hits: Vec<String> = host::live_hosts()
        .into_iter()
        .map(|h| h.id)
        .filter(|id| id.starts_with(want.as_str()))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!("no running agent {want}")),
        _ => Err(format!(
            "{want} matches several agents — give more of the id"
        )),
    }
}

/// What the agent's Claude Code hook last said, as a word.
fn state_of(id: &str) -> &'static str {
    let report = std::fs::read_to_string(crate::agents::state_path(id))
        .ok()
        .and_then(|s| crate::agents::parse_hook(&s));
    match report.map(|r| r.hook) {
        Some(Hook::Ready) => "ready",
        Some(Hook::Working) => "working",
        Some(Hook::Blocked(_)) => "needs-input",
        Some(Hook::Done) => "done",
        Some(Hook::Ended) => "ended",
        None => "running",
    }
}

fn session_of(id: &str) -> Option<String> {
    std::fs::read_to_string(crate::agents::state_path(id))
        .ok()
        .and_then(|s| crate::agents::parse_hook(&s))
        .and_then(|r| r.session)
}

/// Its tab in Kyde, if Kyde has one: the worktree the tab is in, and the tab.
fn tab_of<'a>(
    cfg: &'a kyde_config::agents::AgentsCfg,
    id: &str,
) -> Option<(&'a Path, &'a kyde_config::agents::SavedTab)> {
    cfg.tabs.iter().find_map(|(wt, tabs)| {
        let t = tabs.iter().find(|t| t.id.as_deref() == Some(id))?;
        Some((wt.as_path(), t))
    })
}

/// The name you gave its tab in Kyde, if any.
fn name_of(cfg: &kyde_config::agents::AgentsCfg, id: &str) -> Option<String> {
    tab_of(cfg, id).and_then(|(_, t)| t.name.clone())
}

/// Where it is now, not the folder it `started` in: the worktree its Kyde tab is in (the
/// tab follows its agent between worktrees), else the worktree its session moved to
/// (Claude Code's `EnterWorktree`), else `started`.
fn folder_of(cfg: &kyde_config::agents::AgentsCfg, id: &str, started: &Path) -> PathBuf {
    if let Some((wt, _)) = tab_of(cfg, id) {
        return wt.to_path_buf();
    }
    std::fs::read_to_string(crate::agents::state_path(id))
        .ok()
        .and_then(|s| crate::agents::parse_hook(&s))
        .and_then(|r| crate::agents::session_worktree(r.cwd.as_deref()?, r.transcript.as_deref()?))
        .filter(|wt| !started.starts_with(wt))
        .unwrap_or_else(|| started.to_path_buf())
}

fn list(json: bool) -> Res {
    let me = std::env::var("KYDE_AGENT_ID").ok();
    let hosts = host::live_hosts();
    let cfg = kyde_config::agents::AgentsCfg::load();
    if json {
        let rows: Vec<serde_json::Value> = hosts
            .iter()
            .map(|h| {
                serde_json::json!({
                    "id": h.id,
                    "state": state_of(&h.id),
                    "folder": folder_of(&cfg, &h.id, &h.cwd),
                    "started_in": h.cwd,
                    "agent": h.agent,
                    "name": name_of(&cfg, &h.id),
                    "session": session_of(&h.id),
                    "started": h.started,
                    "you": me.as_deref() == Some(h.id.as_str()),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?
        );
        return Ok(0);
    }
    if hosts.is_empty() {
        println!("No agents running.");
        return Ok(0);
    }
    for h in &hosts {
        let you = if me.as_deref() == Some(h.id.as_str()) {
            "  (you)"
        } else {
            ""
        };
        let name = name_of(&cfg, &h.id)
            .map(|n| format!("  \"{n}\""))
            .unwrap_or_default();
        println!(
            "{}  {:<11}  {}{name}{you}",
            h.id,
            state_of(&h.id),
            folder_of(&cfg, &h.id, &h.cwd).display()
        );
    }
    Ok(0)
}

/// Its recent output, rendered the way the screen shows it.
fn read(rest: &[String]) -> Res {
    let id = resolve(rest.first())?;
    let lines = flag(rest, "--lines")
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(80);
    let (hello, bytes) = host::snapshot(&id).map_err(|e| e.to_string())?;
    print!("{}", render(&bytes, hello.cols, hello.rows, lines));
    Ok(0)
}

/// Raw terminal output → the last `max` lines of text, via a headless terminal of the
/// agent's size.
fn render(bytes: &[u8], cols: u16, rows: u16, max: usize) -> String {
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::{test::TermSize, Config, Term};
    let mut term = Term::new(
        Config::default(),
        &TermSize::new(usize::from(cols.max(20)), usize::from(rows.max(5))),
        alacritty_terminal::event::VoidListener,
    );
    let mut parser: alacritty_terminal::vte::ansi::Processor =
        alacritty_terminal::vte::ansi::Processor::new();
    parser.advance(&mut term, bytes);
    let grid = term.grid();
    let top = -i32::try_from(grid.history_size()).unwrap_or(0);
    let bottom = i32::try_from(grid.screen_lines()).unwrap_or(0);
    let mut out: Vec<String> = (top..bottom)
        .map(|l| {
            let row = &grid[Line(l)];
            (0..grid.columns())
                .map(|c| row[Column(c)].c)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    let start = out.len().saturating_sub(max);
    let mut text = out[start..].join("\n");
    text.push('\n');
    text
}

fn type_text(rest: &[String], enter: bool) -> Res {
    let id = resolve(rest.first())?;
    let text = rest.get(1..).unwrap_or_default().join(" ");
    if text.is_empty() {
        return Err("nothing to type".into());
    }
    // Several lines go in as one paste, so the first newline doesn't submit early.
    let bytes = if text.contains('\n') {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    };
    host::send(&id, host::INPUT, &bytes).map_err(|e| e.to_string())?;
    if enter {
        // A beat later, or a TUI may take the Enter as part of the typed text.
        std::thread::sleep(Duration::from_millis(150));
        host::send(&id, host::INPUT, b"\r").map_err(|e| e.to_string())?;
    }
    Ok(0)
}

fn keys(rest: &[String]) -> Res {
    let id = resolve(rest.first())?;
    let mut bytes = Vec::new();
    for k in rest.get(1..).unwrap_or_default() {
        let (control, key) = match k.strip_prefix("ctrl-") {
            Some(key) => (true, key),
            None => (false, k.as_str()),
        };
        let key = match key {
            "esc" => "escape",
            "return" => "enter",
            other => other,
        };
        let b = crate::terminal::key_bytes(key, control, (key.chars().count() == 1).then_some(key))
            .ok_or_else(|| format!("unknown key {k}"))?;
        bytes.extend(b);
    }
    host::send(&id, host::INPUT, &bytes).map_err(|e| e.to_string())?;
    Ok(0)
}

fn wait(rest: &[String]) -> Res {
    let id = resolve(rest.first())?;
    let until: Vec<String> = flag(rest, "--until")
        .ok_or("--until <state>[,<state>] (working, needs-input, done, idle, ended)")?
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let timeout = flag(rest, "--timeout")
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs);
    let start = Instant::now();
    loop {
        let gone = !host::live_hosts().iter().any(|h| h.id == id);
        let state = if gone { "ended" } else { state_of(&id) };
        let hit = until.iter().any(|u| {
            u == state
                || (u == "idle" && matches!(state, "ready" | "done"))
                || u == "blocked" && state == "needs-input"
        });
        if hit {
            println!("{state}");
            return Ok(0);
        }
        if gone {
            println!("ended");
            return Ok(1);
        }
        if timeout.is_some_and(|t| start.elapsed() >= t) {
            println!("{state}");
            return Ok(1);
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn new(rest: &[String]) -> Res {
    let spec = new_spec(rest, &kyde_config::agents::AgentsCfg::load())?;
    host::spawn(&spec).map_err(|e| e.to_string())?;
    host::wait_registered(&spec.id);
    println!("{}", spec.id);
    let _ = std::io::stdout().flush();
    Ok(0)
}

/// The host `ky agent new <rest>` starts. Its agent gets the note a seeded Kyde tab's does
/// (`agents::seeded_brief`), left for its first prompt: without it, it makes a worktree with
/// `git worktree add` and works there by `cd`, and its tab stays where it started.
fn new_spec(
    rest: &[String],
    cfg: &kyde_config::agents::AgentsCfg,
) -> Result<host::HostSpec, String> {
    let folder = rest
        .first()
        .filter(|a| !a.starts_with("--"))
        .map_or_else(std::env::current_dir, std::fs::canonicalize)
        .map_err(|e| e.to_string())?;
    let shell_only = rest.iter().any(|a| a == "--shell");
    let prompt = flag(rest, "--prompt");
    let line = (!shell_only).then(|| match &prompt {
        Some(p) => crate::agents::seeded_line(cfg.default_command(), p),
        None => cfg.default_command().to_string(),
    });
    let id = crate::agents::tab_id();
    let note = (!shell_only)
        .then(|| crate::agents::seeded_brief(cfg, &folder))
        .flatten();
    if let Some(note) = note {
        let _ = std::fs::create_dir_all(kyde_config::agents::agents_dir());
        std::fs::write(crate::agents::brief_path(&id), note).map_err(|e| e.to_string())?;
        // If it moves out, Claude Code's worktree mustn't make this checkout read as dirty.
        crate::agents::exclude_claude_worktrees(&folder);
    }
    Ok(host::HostSpec {
        id: id.clone(),
        cwd: folder,
        shell: crate::terminal::default_shell(),
        env: crate::agents::tab_env(&id),
        line,
        cols: 120,
        rows: 32,
        agent: !shell_only,
    })
}

/// `ky agent move`: queue this tab's agent's move (its host carries it out once its turn is
/// over — `agent_host::MoveRequest`).
fn move_to(rest: &[String]) -> Res {
    let id = std::env::var("KYDE_AGENT_ID")
        .map_err(|_| "run it from an agent's tab in Kyde: it moves that agent".to_string())?;
    let host = host::live_hosts()
        .into_iter()
        .find(|h| h.id == id)
        .ok_or_else(|| format!("agent {id} isn't running in an agent host"))?;
    if host.version < 3 {
        return Err("this tab's agent host is from an older Kyde: reopen the tab first".into());
    }
    let cfg = kyde_config::agents::AgentsCfg::load();
    let from = folder_of(&cfg, &id, &host.cwd);
    let session = std::fs::read_to_string(crate::agents::state_path(&id))
        .ok()
        .and_then(|s| crate::agents::last_session(&s))
        .ok_or("no Claude Code session to move yet (its hook hasn't reported one)")?;
    let req = move_request(rest, &cfg, &from, session)?;
    let to = req.spec.cwd.clone();
    let json = serde_json::to_string(&req).map_err(|e| e.to_string())?;
    std::fs::write(crate::agents::move_path(&id), json).map_err(|e| e.to_string())?;
    // If it moves on from the project's own checkout, Claude Code's worktree there mustn't
    // make that checkout read as dirty.
    crate::agents::exclude_claude_worktrees(&to);
    println!(
        "Moving to {} once this turn is over. End your turn now, saying in a line where \
         you're going: Kyde then resumes this conversation there (agent {}) and closes this \
         tab.",
        to.display(),
        req.spec.id
    );
    Ok(0)
}

/// The move `ky agent move <rest>` asks for, of an agent in worktree `from` on Claude Code
/// session `(id, transcript)`: to the worktree `<folder>` is in, of a project Kyde has (or
/// its tab would have nowhere to go), resuming the session on `--prompt`. Runs git.
fn move_request(
    rest: &[String],
    cfg: &kyde_config::agents::AgentsCfg,
    from: &Path,
    (session, transcript): (String, PathBuf),
) -> Result<host::MoveRequest, String> {
    let folder = rest
        .first()
        .filter(|a| !a.starts_with('-'))
        .ok_or("where to? `ky agent move <folder>`")?;
    let repo = kyde_git::Repo::discover(folder)
        .map_err(|_| format!("{folder} isn't in a git repository"))?;
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let to = canon(repo.root());
    if to == canon(from) {
        return Err(format!("this agent is in {} already", to.display()));
    }
    let spaces = || cfg.spaces.iter().filter(|s| !s.scratch);
    let common = |p: &Path| Some(canon(&kyde_git::Repo::discover(p).ok()?.common_dir().ok()?));
    let space = spaces()
        .find(|s| canon(&s.path) == to || s.worktrees.iter().any(|w| canon(w) == to))
        .or_else(|| {
            let ours = common(&to)?;
            spaces().find(|s| common(&s.path).as_ref() == Some(&ours))
        })
        .ok_or_else(|| {
            format!(
                "Kyde has no project at {}: ask the user to add it in Kyde first",
                to.display()
            )
        })?;
    // As Kyde spells it, so its tab lands in that row.
    let to = std::iter::once(&space.path)
        .chain(&space.worktrees)
        .find(|w| canon(w) == to)
        .cloned()
        .unwrap_or(to);
    let prompt = crate::agents::moved_prompt(from, &to, flag(rest, "--prompt").as_deref());
    let start = format!(
        "{} --resume {}",
        cfg.default_command(),
        crate::agents::shell_quote(&session)
    );
    let id = crate::agents::tab_id();
    Ok(host::MoveRequest {
        spec: host::HostSpec {
            id: id.clone(),
            cwd: to.clone(),
            shell: crate::terminal::default_shell(),
            env: crate::agents::tab_env(&id),
            line: Some(crate::agents::seeded_line(&start, &prompt)),
            cols: 120,
            rows: 32,
            agent: true,
        },
        transcript: Some(transcript),
        brief: crate::agents::moved_brief(cfg, &to),
        space: space.path.clone(),
        idle_ok: false,
    })
}

/// `ky agent restart`: queue a restart of this tab's agent, or of `<id>`'s.
fn restart(rest: &[String]) -> Res {
    let me = std::env::var("KYDE_AGENT_ID").ok();
    let id = match rest.first().filter(|a| !a.starts_with('-')) {
        Some(_) => resolve(rest.first())?,
        None => me.clone().ok_or(
            "which agent? Give an id from `ky agent list` — or run it from an agent's tab \
             in Kyde, to restart that agent",
        )?,
    };
    let own = me.as_deref() == Some(id.as_str());
    let at = queue_restart(&id, flag(rest, "--prompt").as_deref(), own)?;
    if own {
        println!(
            "Restarting once this turn is over. End your turn now, saying in a line that \
             you're restarting: Kyde then resumes this conversation as a fresh process, in \
             this tab's place."
        );
    } else {
        println!(
            "Restarting agent {id} in {}: now if it's between turns, else once its turn is \
             over.",
            at.display()
        );
    }
    Ok(0)
}

/// Queue a restart of agent `id` (its host carries it out — `agent_host::MoveRequest`): a
/// fresh process resumes its Claude Code session in the folder it's in, on `prompt` — once
/// its turn is over, or at once if it's between turns. `own` = the agent asked for it itself.
/// Returns the folder. File IO: call it off the UI thread.
pub(crate) fn queue_restart(id: &str, prompt: Option<&str>, own: bool) -> Result<PathBuf, String> {
    let host = host::live_hosts()
        .into_iter()
        .find(|h| h.id == id)
        .ok_or_else(|| format!("agent {id} isn't running in an agent host"))?;
    if !host.agent {
        return Err("that tab is a plain shell: it has no agent to restart".into());
    }
    // Waiting out its own turn is as old as `move`; going while it's between turns is new.
    if host.version < if own { 3 } else { host::VERSION } {
        return Err("this tab's agent host is from an older Kyde: reopen the tab first".into());
    }
    let cfg = kyde_config::agents::AgentsCfg::load();
    let at = folder_of(&cfg, id, &host.cwd);
    let session = std::fs::read_to_string(crate::agents::state_path(id))
        .ok()
        .and_then(|s| crate::agents::last_session(&s))
        // One that never got a prompt has no transcript: `--resume` on it fails.
        .filter(|(_, transcript)| transcript.exists())
        .ok_or("no Claude Code conversation to resume yet (it hasn't had a prompt)")?;
    let req = restart_request(&cfg, &at, session, prompt, own);
    let json = serde_json::to_string(&req).map_err(|e| e.to_string())?;
    std::fs::write(crate::agents::move_path(id), json).map_err(|e| e.to_string())?;
    Ok(at)
}

/// The restart of an agent in folder `at` on Claude Code session `(id, transcript)`: a move
/// to where it is already. Its own restart is mid-task, so it resumes on `prompt` — else to
/// carry on; one restarted from outside just waits for its next prompt, unless handed one.
fn restart_request(
    cfg: &kyde_config::agents::AgentsCfg,
    at: &Path,
    (session, transcript): (String, PathBuf),
    prompt: Option<&str>,
    own: bool,
) -> host::MoveRequest {
    let start = format!(
        "{} --resume {}",
        cfg.default_command(),
        crate::agents::shell_quote(&session)
    );
    let prompt = prompt.map(str::trim).filter(|p| !p.is_empty());
    let line = if own || prompt.is_some() {
        crate::agents::seeded_line(&start, &crate::agents::restarted_prompt(prompt))
    } else {
        start
    };
    let id = crate::agents::tab_id();
    host::MoveRequest {
        spec: host::HostSpec {
            id: id.clone(),
            cwd: at.to_path_buf(),
            shell: crate::terminal::default_shell(),
            env: crate::agents::tab_env(&id),
            line: Some(line),
            cols: 120,
            rows: 32,
            agent: true,
        },
        transcript: Some(transcript),
        // It has heard Kyde's note already, earlier in this conversation.
        brief: None,
        space: cfg
            .space_of(at)
            .map_or_else(|| at.to_path_buf(), |s| s.path.clone()),
        // Asked mid-turn (the agent's own), its latest report says it's at work: it waits
        // for that turn to end either way.
        idle_ok: true,
    }
}

/// The value after `name` (`--lines 40`).
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_renders_what_the_screen_shows() {
        // Overwritten text, colours and a cursor move come out as the visible text.
        let raw = b"hello\x1b[31m world\x1b[0m\r\nabc\x1b[2Dxy\r\n";
        assert_eq!(render(raw, 40, 10, 80), "hello world\naxy\n");
        assert_eq!(render(raw, 40, 10, 1), "axy\n");
    }

    /// An agent `ky agent new` starts hears with its first prompt how to move to another
    /// worktree (so its tab follows it), like a tab Kyde opens; a plain shell hears nothing.
    #[test]
    fn a_new_agent_hears_how_to_move_worktrees() {
        crate::gpui_smoke_tests::isolate_history(); // a throwaway agents dir
        let dir = std::env::temp_dir().join(format!("kyde-cli-new-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = kyde_config::agents::AgentsCfg::default();
        let args = |a: &[&str]| -> Vec<String> {
            std::iter::once(dir.to_string_lossy().into_owned())
                .chain(a.iter().map(|s| (*s).to_string()))
                .collect()
        };
        let agent = new_spec(&args(&["--prompt", "add a route"]), &cfg).unwrap();
        let note = std::fs::read_to_string(crate::agents::brief_path(&agent.id)).unwrap();
        assert!(note.contains("EnterWorktree"), "{note}");
        let shell = new_spec(&args(&["--shell"]), &cfg).unwrap();
        assert!(!crate::agents::brief_path(&shell.id).exists());
    }

    /// `new` (and `move`) never go ahead on a flag they don't know: `--help` asks for the
    /// help, anything else is an error. A `--prompt`'s text is text.
    #[test]
    fn only_known_flags_start_an_agent() {
        let args = |a: &[&str]| -> Vec<String> { a.iter().map(|s| (*s).to_string()).collect() };
        assert_eq!(asks_help(&args(&["--help"]), &["--shell"]), Ok(true));
        assert_eq!(asks_help(&args(&["/r", "-h"]), &["--shell"]), Ok(true));
        assert!(asks_help(&args(&["/r", "--name", "x"]), &["--shell"]).is_err());
        assert_eq!(
            asks_help(
                &args(&["/r", "--prompt", "--help", "--shell"]),
                &["--shell"]
            ),
            Ok(false)
        );
    }

    /// `move` takes an agent to the worktree its folder is in, of a project Kyde has, resuming
    /// its session there; a folder of a project Kyde doesn't have is refused.
    #[test]
    fn a_move_goes_to_a_project_kyde_has() {
        crate::gpui_smoke_tests::isolate_history();
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("kyde-cli-move-{}", std::process::id()));
        let (known, other) = (root.join("known"), root.join("other"));
        for repo in [&known, &other] {
            std::fs::create_dir_all(repo.join("sub")).unwrap();
            let ok = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(repo)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        }
        let mut cfg = kyde_config::agents::AgentsCfg::default();
        cfg.spaces.push(kyde_config::agents::Space {
            path: known.clone(),
            ..Default::default()
        });
        let session = || ("sess-1".to_string(), PathBuf::from("/p/sess-1.jsonl"));
        let to = |dir: &Path| vec![dir.join("sub").to_string_lossy().into_owned()];
        let req = move_request(&to(&known), &cfg, Path::new("/elsewhere"), session()).unwrap();
        assert_eq!((&req.spec.cwd, &req.space), (&known, &known));
        let line = req.spec.line.unwrap();
        assert!(line.contains("claude --resume 'sess-1'"), "{line}");
        assert!(move_request(&to(&other), &cfg, Path::new("/elsewhere"), session()).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A restart (#252) resumes the session in the folder it's in, without waiting for a
    /// turn when there's none. Its own, mid-task, carries on with what it said was next;
    /// one asked from outside just waits for its next prompt.
    #[test]
    fn a_restart_resumes_the_session_where_it_is() {
        let cfg = kyde_config::agents::AgentsCfg::default();
        let at = Path::new("/r/.kyde/worktrees/r/task");
        let session = || ("sess-1".to_string(), PathBuf::from("/p/sess-1.jsonl"));
        let own = restart_request(&cfg, at, session(), Some("check the new tools"), true);
        assert_eq!((own.spec.cwd.as_path(), own.idle_ok), (at, true));
        let line = own.spec.line.unwrap();
        assert!(line.contains("claude --resume 'sess-1'"), "{line}");
        assert!(line.contains("check the new tools"), "{line}");
        let other = restart_request(&cfg, at, session(), None, false);
        assert_eq!(other.spec.line.as_deref(), Some("claude --resume 'sess-1'"));
    }

    /// `list` shows an agent where its Kyde tab followed it, not the folder it started in.
    #[test]
    fn list_shows_an_agent_where_its_tab_is_now() {
        let mut cfg = kyde_config::agents::AgentsCfg::default();
        let tab = kyde_config::agents::SavedTab {
            id: Some("acb541da6d50".into()),
            ..Default::default()
        };
        let task = PathBuf::from("/r/.kyde/worktrees/r/task");
        cfg.tabs.insert(task.clone(), vec![tab]);
        assert_eq!(folder_of(&cfg, "acb541da6d50", Path::new("/r")), task);
    }
}
