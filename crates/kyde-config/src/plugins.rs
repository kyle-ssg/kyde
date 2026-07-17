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
    /// Pack ids with error highlighting opted OUT. Error highlighting is ON by
    /// default for every installed pack (the pack's parse already runs; the
    /// error walk is the cheap part) — this records the per-pack "no thanks",
    /// so an empty set (and any pre-existing plugins.json) means all on.
    #[serde(default)]
    errors_disabled: BTreeSet<String>,
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
    /// Also drops the pack's error-highlighting opt-out (reinstall = fresh defaults).
    pub fn uninstall(&mut self, pack_id: &str) {
        self.installed.remove(pack_id);
        self.errors_disabled.remove(pack_id);
    }

    /// Whether error highlighting is on for `pack_id` — the default unless the
    /// user opted out. Only meaningful when the pack is installed — callers gate
    /// on `is_installed` first.
    pub fn errors_on(&self, pack_id: &str) -> bool {
        !self.errors_disabled.contains(pack_id)
    }

    /// Turn error highlighting on/off for `pack_id` (default on; `false` records
    /// a per-pack opt-out).
    pub fn set_errors(&mut self, pack_id: &str, on: bool) {
        if on {
            self.errors_disabled.remove(pack_id);
        } else {
            self.errors_disabled.insert(pack_id.to_string());
        }
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

    #[test]
    fn errors_default_on_and_opt_out_round_trips() {
        let mut p = Plugins::default();
        p.install("json");
        assert!(p.errors_on("json"), "error highlighting must default ON");
        // Opt out → persists across a save/load round trip.
        p.set_errors("json", false);
        assert!(!p.errors_on("json"));
        let back: Plugins = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert!(!back.errors_on("json"));
        // Opting back in clears the opt-out.
        p.set_errors("json", true);
        assert!(p.errors_on("json"));
        // Old plugins.json without the field still parses → default on.
        let old: Plugins = serde_json::from_str(r#"{"installed":["json"]}"#).unwrap();
        assert!(old.is_installed("json") && old.errors_on("json"));
        // Uninstall drops the opt-out → reinstall returns to the default (on).
        let mut q = Plugins::default();
        q.install("json");
        q.set_errors("json", false);
        q.uninstall("json");
        q.install("json");
        assert!(q.errors_on("json"), "reinstall must return to default-on");
    }
}
