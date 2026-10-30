//! `ky agent …` — the command line agents (and you) use to see and drive Kyde's agents.
//!
//! It talks straight to the agent hosts' sockets and state files in `~/.kyde/agents` (see
//! `agent_host`), so it works whether or not the Kyde window is open. An agent started in a
//! Kyde tab has `KYDE_AGENT_ID` (itself) and `KYDE_BIN` (this binary) in its environment.

use crate::agent_host as host;
use crate::agents::Hook;
use std::io::Write;
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

  ky agent list [--json]                 every running agent: id, state, folder, name
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

/// The name you gave its tab in Kyde, if any.
fn name_of(id: &str) -> Option<String> {
    kyde_config::agents::AgentsCfg::load()
        .tabs
        .values()
        .flatten()
        .find(|t| t.id.as_deref() == Some(id))
        .and_then(|t| t.name.clone())
}

fn list(json: bool) -> Res {
    let me = std::env::var("KYDE_AGENT_ID").ok();
    let hosts = host::live_hosts();
    if json {
        let rows: Vec<serde_json::Value> = hosts
            .iter()
            .map(|h| {
                serde_json::json!({
                    "id": h.id,
                    "state": state_of(&h.id),
                    "folder": h.cwd,
                    "agent": h.agent,
                    "name": name_of(&h.id),
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
        let name = name_of(&h.id)
            .map(|n| format!("  \"{n}\""))
            .unwrap_or_default();
        println!(
            "{}  {:<11}  {}{name}{you}",
            h.id,
            state_of(&h.id),
            h.cwd.display()
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
    let folder = rest
        .first()
        .filter(|a| !a.starts_with("--"))
        .map_or_else(std::env::current_dir, std::fs::canonicalize)
        .map_err(|e| e.to_string())?;
    let shell_only = rest.iter().any(|a| a == "--shell");
    let prompt = flag(rest, "--prompt");
    let cfg = kyde_config::agents::AgentsCfg::load();
    let line = (!shell_only).then(|| match &prompt {
        Some(p) => format!(
            "{} {}",
            cfg.default_command(),
            crate::agents::shell_quote(p)
        ),
        None => cfg.default_command().to_string(),
    });
    let id = crate::agents::tab_id();
    let spec = host::HostSpec {
        id: id.clone(),
        cwd: folder,
        shell: crate::terminal::default_shell(),
        env: crate::agents::tab_env(&id),
        line,
        cols: 120,
        rows: 32,
        agent: !shell_only,
    };
    host::spawn(&spec).map_err(|e| e.to_string())?;
    println!("{id}");
    let _ = std::io::stdout().flush();
    Ok(0)
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
}
