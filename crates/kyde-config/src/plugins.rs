//! Installed language packs ("plugins"), persisted as JSON next to keymap.json.
//!
//! Highlighting is opt-in: a `Lang`'s pack must be installed before the editor
//! parses & colors it. Nothing is enabled by default — the whole point is that
//! opening files of an un-installed type stays fast (no tree-sitter at all).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Plugins {
    /// Set of installed pack ids (see `highlight::PACKS`).
    #[serde(default)]
    installed: BTreeSet<String>,
}

impl Plugins {
    pub fn is_installed(&self, pack_id: &str) -> bool {
        self.installed.contains(pack_id)
    }

    pub fn install(&mut self, pack_id: &str) {
        self.installed.insert(pack_id.to_string());
    }

    /// Remove a pack from the installed set (its grammar is still compiled in, but the
    /// editor falls back to PlainText for that language until re-installed).
    pub fn uninstall(&mut self, pack_id: &str) {
        self.installed.remove(pack_id);
    }

    // ── persistence ───────────────────────────────────────────────
    pub fn config_path() -> PathBuf {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(home).join(".config")
            });
        base.join("kyde").join("plugins.json")
    }

    pub fn load() -> Self {
        match std::fs::read_to_string(Self::config_path()) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Plugins::default(),
        }
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_round_trips() {
        let mut p = Plugins::default();
        assert!(!p.is_installed("json"));
        p.install("json");
        assert!(p.is_installed("json"));
        let json = serde_json::to_string(&p).unwrap();
        let back: Plugins = serde_json::from_str(&json).unwrap();
        assert!(back.is_installed("json"));
        assert!(!back.is_installed("rust"));
    }
}
