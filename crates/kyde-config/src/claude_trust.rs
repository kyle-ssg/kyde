//! Folder trust in Claude Code's own config (`~/.claude.json`).
//!
//! Claude Code asks "do you trust this folder?" the first time it starts in one, unless the
//! folder or one above it was trusted before. Scratch rows are fresh folders Kyde makes for
//! ad-hoc agents, so Kyde trusts their parent ([`crate::agents::scratch_dir`]) once — else
//! every new scratch tab would open on that question.
//!
//! [`merge`] is the pure part (unit-tested): it sets the one key and leaves the rest of the
//! file — key order included — as it was.

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// Why trusting a folder failed.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// The config file couldn't be read or written.
    #[error("{action} {path}: {source}")]
    Io {
        /// "reading" / "writing".
        action: &'static str,
        /// The config file.
        path: PathBuf,
        /// The OS error.
        source: std::io::Error,
    },
    /// The config file isn't a JSON object — left untouched.
    #[error("{path} isn't valid JSON ({source}) — left untouched")]
    Parse {
        /// The config file.
        path: PathBuf,
        /// What's wrong with it.
        source: serde_json::Error,
    },
}

/// Claude Code's config file: `$CLAUDE_CONFIG_DIR/.claude.json`, else `~/.claude.json`.
#[must_use]
pub fn config_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(dir).join(".claude.json");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".claude.json")
}

/// `config` (the config file's text) with folder `dir` trusted — `None` when it already is.
///
/// ```
/// use kyde_config::claude_trust::merge;
/// let out = merge(r#"{"theme":"dark","projects":{}}"#, "/s").unwrap().unwrap();
/// assert!(out.starts_with("{\n  \"theme\"") && out.contains("\"hasTrustDialogAccepted\": true"));
/// assert_eq!(merge(&out, "/s").unwrap(), None);
/// ```
///
/// # Errors
/// When `config` isn't a JSON object.
pub fn merge(config: &str, dir: &str) -> Result<Option<String>, serde_json::Error> {
    let mut root: Map<String, Value> = serde_json::from_str(config)?;
    let projects = root
        .entry("projects")
        .or_insert_with(|| Value::Object(Map::new()));
    if !projects.is_object() {
        *projects = Value::Object(Map::new());
    }
    let Some(project) = projects.as_object_mut().map(|p| {
        p.entry(dir.to_string())
            .or_insert_with(|| Value::Object(Map::new()))
    }) else {
        return Ok(None);
    };
    if project["hasTrustDialogAccepted"] == Value::Bool(true) {
        return Ok(None);
    }
    if !project.is_object() {
        *project = Value::Object(Map::new());
    }
    if let Some(p) = project.as_object_mut() {
        p.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
    }
    serde_json::to_string_pretty(&root).map(Some)
}

/// Trust folder `dir` in the config file at `path`, writing only when it isn't yet (and
/// never creating the file — that's Claude Code's first run). Returns whether it wrote.
///
/// # Errors
/// When the file can't be read or written, or isn't JSON (it's never overwritten then).
pub fn apply(path: &Path, dir: &Path) -> Result<bool, TrustError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(TrustError::Io {
                action: "reading",
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let merged = merge(&text, &dir.to_string_lossy()).map_err(|source| TrustError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    let Some(new) = merged else {
        return Ok(false);
    };
    let io = |source| TrustError::Io {
        action: "writing",
        path: path.to_path_buf(),
        source,
    };
    // Write beside it, then rename — Claude never reads a half-written file. The copy gets
    // the file's permissions before any content does (it's private: 0600).
    let tmp = path.with_extension("json.kyde-tmp");
    let perms = std::fs::metadata(path).map_err(io)?.permissions();
    std::fs::write(&tmp, "").map_err(io)?;
    std::fs::set_permissions(&tmp, perms).map_err(io)?;
    std::fs::write(&tmp, format!("{new}\n")).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)?;
    Ok(true)
}
