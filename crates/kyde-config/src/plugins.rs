//! Installed language packs ("plugins"), persisted as JSON next to keymap.json.
//!
//! Highlighting is opt-in: a `Lang`'s pack must be installed before the editor
//! parses & colors it. Nothing is enabled by default — the whole point is that
//! opening files of an un-installed type stays fast (no tree-sitter at all).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// The user's installed language-pack list (persisted to `plugins.json`).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Plugins {
    /// Set of installed pack ids (see `highlight::PACKS`).
    #[serde(default)]
    installed: BTreeSet<String>,
}

impl Plugins {
    /// Whether the pack `pack_id` is installed.
    pub fn is_installed(&self, pack_id: &str) -> bool {
        self.installed.contains(pack_id)
    }

    /// Mark `pack_id` installed (its grammar becomes active for that language).
    pub fn install(&mut self, pack_id: &str) {
        self.installed.insert(pack_id.to_string());
    }

    /// Remove a pack from the installed set (its grammar is still compiled in, but the
    /// editor falls back to `PlainText` for that language until re-installed).
    pub fn uninstall(&mut self, pack_id: &str) {
        self.installed.remove(pack_id);
    }

    // ── persistence ───────────────────────────────────────────────
    /// Config file name under the XDG config dir.
    const FILE: &'static str = "plugins.json";

    /// Path to `plugins.json` under the XDG config dir.
    #[must_use]
    pub fn config_path() -> PathBuf {
        crate::store::config_path(Self::FILE)
    }

    /// Load the installed-pack list from `plugins.json` (missing/invalid → empty).
    #[must_use]
    pub fn load() -> Self {
        crate::store::load_or_default(Self::FILE)
    }

    /// Persist the installed-pack list to `plugins.json` (best-effort).
    pub fn save(&self) {
        crate::store::save(Self::FILE, self);
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
