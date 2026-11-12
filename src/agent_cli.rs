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
pub(crate) const COMMANDS: [&str; 9] = [
    "list", "read", "send", "prompt", "keys", "wait", "new", "kill", "help",
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
  ky agent kill <id>                     end it (its tab closes)

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
        "new" => new(rest),
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
