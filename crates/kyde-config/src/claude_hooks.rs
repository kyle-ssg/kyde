//! The Claude Code hook behind the Agents view's live states.
//!
//! Kyde starts every agent tab with `KYDE_AGENT_STATE=<file>` in its environment. Claude Code
//! runs this hook on its lifecycle events (prompt submitted, tool use, permission prompt,
//! turn finished, …) and the hook writes the event's JSON to that file — Kyde reads it to show
//! "needs input" / "working" / "done" and to learn the tab's exact session id. Outside a Kyde
//! tab the variable is unset and the hook does nothing.
//!
//! On a prompt it also hands Claude a one-shot note when Kyde left one ([`brief_command`]):
//! what's already going on in the tab's worktree, so an unrelated task can move to a
//! worktree of its own.
//!
//! [`merge`] is the pure part (unit-tested): it adds or removes Kyde's entries in Claude
//! Code's `settings.json`, leaving everything else — other tools' hooks included — as is.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// Marks Kyde's hook command, so it's found again to update or remove.
pub const MARKER: &str = "# kyde-agent-state";

/// The Claude Code events Kyde listens to.
pub const EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Notification",
    "Stop",
    "SessionEnd",
];

/// The hook command: the event JSON (stdin) → `$KYDE_AGENT_STATE`, written atomically; always
/// exits 0 so it can never block Claude.
#[must_use]
pub fn command() -> String {
    format!(
        "if [ -n \"$KYDE_AGENT_STATE\" ]; then cat > \"$KYDE_AGENT_STATE.$$\" && \
         mv -f \"$KYDE_AGENT_STATE.$$\" \"$KYDE_AGENT_STATE\"; else cat > /dev/null; fi; \
         exit 0 {MARKER}"
    )
}

/// The prompt hook that hands Claude Kyde's note about the tab's worktree: prints
/// `$KYDE_AGENT_STATE.brief` (Claude adds a prompt hook's output to its context) and deletes
/// it, so only the first real prompt sees it — a slash command (`/clear`, `/caveman`; not a
/// prompt opening with a path like `/Users/…`) leaves it for the next one. Always exits 0.
#[must_use]
pub fn brief_command() -> String {
    format!(
        "f=\"$KYDE_AGENT_STATE.brief\"; if [ -n \"$KYDE_AGENT_STATE\" ] && [ -f \"$f\" ] && \
         ! grep -q '\"prompt\" *: *\"/[A-Za-z0-9:_-]*[ \"\\]'; then cat \"$f\"; rm -f \"$f\"; \
         else cat > /dev/null; fi; \
         exit 0 {MARKER}"
    )
}

/// A failure reading or writing Claude Code's settings.
#[derive(Debug, thiserror::Error)]
pub enum HooksError {
    /// The settings file exists but couldn't be read or written.
    #[error("{action} {path}: {source}")]
    Io {
        /// "reading" / "writing".
        action: &'static str,
        /// The settings file.
        path: PathBuf,
        /// The OS error.
        source: std::io::Error,
    },
    /// The settings file isn't a JSON object — left untouched.
    #[error("{path} isn't valid JSON ({source}) — left untouched")]
    Parse {
        /// The settings file.
        path: PathBuf,
        /// What's wrong with it.
        source: serde_json::Error,
    },
}

/// Claude Code's user settings: `~/.claude/settings.json`.
#[must_use]
pub fn settings_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".claude").join("settings.json")
}

fn is_ours(entry: &Value) -> bool {
    entry["hooks"].as_array().is_some_and(|hs| {
        hs.iter()
            .any(|h| h["command"].as_str().is_some_and(|c| c.contains(MARKER)))
    })
}

/// `settings` (Claude Code's settings.json text) with Kyde's hook installed (`on`) or removed
/// — `None` when nothing needs to change. Other keys and other tools' hooks are kept, in
/// order; an outdated Kyde entry is replaced.
///
/// ```
/// use kyde_config::claude_hooks::merge;
/// let added = merge(r#"{"model":"opus"}"#, true).unwrap().unwrap();
/// assert!(added.contains("kyde-agent-state") && added.starts_with("{\n  \"model\""));
/// assert_eq!(merge(&added, true).unwrap(), None); // already there
/// let removed = merge(&added, false).unwrap().unwrap();
/// assert!(!removed.contains("kyde-agent-state"));
/// ```
///
/// # Errors
/// When `settings` isn't a JSON object.
pub fn merge(settings: &str, on: bool) -> Result<Option<String>, serde_json::Error> {
    let mut root: Map<String, Value> = if settings.trim().is_empty() {
        Map::new()
    } else {
        serde_json::from_str(settings)?
    };
    let before = Value::Object(root.clone());
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    if let Some(hooks) = hooks.as_object_mut() {
        // Drop any Kyde entry (stale or current), then add the current one back.
        for list in hooks.values_mut() {
            if let Some(entries) = list.as_array_mut() {
                entries.retain(|e| !is_ours(e));
            }
        }
        if on {
            let hook = |c: String| json!({ "type": "command", "command": c, "timeout": 5 });
            for event in EVENTS {
                let mut cmds = vec![hook(command())];
                if event == "UserPromptSubmit" {
                    cmds.push(hook(brief_command()));
                }
                let list = hooks
                    .entry(event)
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Some(entries) = list.as_array_mut() {
                    entries.push(json!({ "matcher": "*", "hooks": cmds }));
                }
            }
        }
        hooks.retain(|_, list| list.as_array().is_none_or(|a| !a.is_empty()));
    }
    if root
        .get("hooks")
        .is_some_and(|h| h.as_object().is_some_and(Map::is_empty))
    {
        root.remove("hooks");
    }
    let after = Value::Object(root);
    if after == before {
        return Ok(None);
    }
    serde_json::to_string_pretty(&after).map(Some)
}

/// Whether Kyde's current hook is in `settings`.
#[must_use]
pub fn is_installed(settings: &str) -> bool {
    matches!(merge(settings, true), Ok(None))
}

/// Install (`on`) or remove Kyde's hook in the settings file at `path`, writing only when
/// something changes. Returns whether it wrote.
///
/// # Errors
/// When the file can't be read or written, or isn't JSON (it's never overwritten then).
pub fn apply(path: &Path, on: bool) -> Result<bool, HooksError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(HooksError::Io {
                action: "reading",
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let merged = merge(&text, on).map_err(|source| HooksError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    let Some(new) = merged else {
        return Ok(false);
    };
    let io = |source| HooksError::Io {
        action: "writing",
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    // Write beside it, then rename — Claude never reads a half-written file.
    let tmp = path.with_extension("json.kyde-tmp");
    std::fs::write(&tmp, format!("{new}\n")).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_keeps_other_tools_hooks_and_replaces_a_stale_kyde_entry() {
        let other = r#"{"hooks":{"SessionStart":[{"matcher":"*","hooks":[{"type":"command","command":"herdr.sh"}]}],
            "Stop":[{"hooks":[{"type":"command","command":"old # kyde-agent-state"}]}]}}"#;
        let out = merge(other, true).unwrap().unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        // herdr's hook stays, first; Kyde's follows.
        let start = v["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 2);
        assert_eq!(start[0]["hooks"][0]["command"], "herdr.sh");
        // The stale Kyde Stop hook was replaced by the current one, not duplicated.
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["hooks"][0]["command"], command());
        for e in EVENTS {
            assert!(v["hooks"][e].as_array().unwrap().iter().any(is_ours), "{e}");
        }
        // A prompt also runs the brief hook, beside the state one.
        assert_eq!(
            v["hooks"]["UserPromptSubmit"][0]["hooks"][1]["command"],
            brief_command()
        );
        // Removing leaves only herdr's.
        let off: Value = serde_json::from_str(&merge(&out, false).unwrap().unwrap()).unwrap();
        assert_eq!(off["hooks"].as_object().unwrap().len(), 1);
        assert!(merge("not json", true).is_err());
    }

    /// Kyde's note reaches the first real prompt once; a slash command leaves it waiting.
    #[cfg(unix)]
    #[test]
    fn brief_is_shown_once_on_the_first_real_prompt() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("kyde-brief-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("t.state");
        let brief = dir.join("t.state.brief");
        std::fs::write(&brief, "busy here").unwrap();
        let prompt = |text: &str| {
            let mut sh = std::process::Command::new("sh")
                .args(["-c", &brief_command()])
                .env("KYDE_AGENT_STATE", &state)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let json = serde_json::json!({ "hook_event_name": "UserPromptSubmit", "prompt": text });
            sh.stdin
                .take()
                .unwrap()
                .write_all(json.to_string().as_bytes())
                .unwrap();
            String::from_utf8(sh.wait_with_output().unwrap().stdout).unwrap()
        };
        assert_eq!(prompt("/caveman"), "");
        assert_eq!(prompt("/Users/me/notes.md add a style guide"), "busy here");
        assert_eq!(prompt("and a changelog"), "");
        assert!(!brief.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
