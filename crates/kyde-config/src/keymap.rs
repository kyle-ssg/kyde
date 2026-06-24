//! Configurable keymap with WebStorm and VSCode presets, persisted as JSON.
//!
//! The actual gpui `Action` types live in `main.rs`; this module owns only the
//! *configuration* (which keystroke triggers which named action) and the preset
//! defaults. `main::apply_keymap` reads `key_for()` and binds the real actions.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Configurable, user-facing actions. The `&str` name is the stable config key.
/// (ws default, vscode default, human label) per action.
pub const ACTIONS: &[ActionDef] = &[
    ActionDef {
        name: "go_to_file",
        webstorm: "cmd-shift-o",
        vscode: "cmd-p",
        label: "Go to File",
    },
    ActionDef {
        name: "find_in_files",
        webstorm: "cmd-shift-f",
        vscode: "cmd-shift-f",
        label: "Find in Files",
    },
    ActionDef {
        name: "save",
        webstorm: "cmd-s",
        vscode: "cmd-s",
        label: "Save File",
    },
    ActionDef {
        name: "commit",
        webstorm: "cmd-k",
        vscode: "cmd-enter",
        label: "Commit",
    },
    ActionDef {
        name: "mode_commit",
        webstorm: "cmd-9",
        vscode: "ctrl-shift-g",
        label: "Go to Commit view",
    },
    ActionDef {
        name: "mode_browse",
        webstorm: "cmd-1",
        vscode: "cmd-shift-e",
        label: "Go to Browse view",
    },
    ActionDef {
        name: "open_keymap",
        webstorm: "cmd-,",
        vscode: "cmd-,",
        label: "Keymap / Onboarding",
    },
    ActionDef {
        name: "actions",
        webstorm: "cmd-shift-a",
        vscode: "cmd-shift-a",
        label: "Find Action",
    },
    ActionDef {
        name: "new_scratch",
        webstorm: "cmd-shift-n",
        vscode: "cmd-shift-n",
        label: "New Scratch File",
    },
];

pub struct ActionDef {
    pub name: &'static str,
    pub webstorm: &'static str,
    pub vscode: &'static str,
    pub label: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    WebStorm,
    VSCode,
    Custom,
}

impl Preset {
    pub fn label(self) -> &'static str {
        match self {
            Preset::WebStorm => "WebStorm / IntelliJ",
            Preset::VSCode => "VSCode",
            Preset::Custom => "Custom",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Keymap {
    pub preset: Preset,
    /// Per-action keystroke overrides (action name → keystroke). Empty = use preset.
    #[serde(default)]
    pub overrides: BTreeMap<String, String>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            preset: Preset::WebStorm,
            overrides: BTreeMap::new(),
        }
    }
}

impl Keymap {
    /// Preset default keystroke for a named action.
    pub fn preset_key(&self, name: &str) -> Option<&'static str> {
        ACTIONS
            .iter()
            .find(|a| a.name == name)
            .map(|a| match self.preset {
                Preset::VSCode => a.vscode,
                _ => a.webstorm, // Custom falls back to WebStorm defaults for unset actions
            })
    }

    /// Effective keystroke for a named action (override wins over preset).
    pub fn key_for(&self, name: &str) -> Option<String> {
        if let Some(k) = self.overrides.get(name) {
            return Some(k.clone());
        }
        self.preset_key(name).map(str::to_string)
    }

    pub fn set_preset(&mut self, preset: Preset) {
        self.preset = preset;
        self.overrides.clear();
    }

    // ── persistence ───────────────────────────────────────────────
    pub fn config_path() -> PathBuf {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(home).join(".config")
            });
        base.join("kyde").join("keymap.json")
    }

    /// Load from disk. Returns (keymap, first_run) — first_run is true when no
    /// config existed yet (used to trigger onboarding).
    pub fn load() -> (Self, bool) {
        let path = Self::config_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => match serde_json::from_str::<Keymap>(&s) {
                Ok(km) => (km, false),
                Err(_) => (Keymap::default(), true),
            },
            Err(_) => (Keymap::default(), true),
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
    fn presets_differ_for_go_to_file() {
        let mut km = Keymap::default();
        assert_eq!(km.key_for("go_to_file").as_deref(), Some("cmd-shift-o"));
        km.set_preset(Preset::VSCode);
        assert_eq!(km.key_for("go_to_file").as_deref(), Some("cmd-p"));
    }

    #[test]
    fn override_wins_and_round_trips() {
        let mut km = Keymap::default();
        km.overrides.insert("save".into(), "cmd-alt-s".into());
        let json = serde_json::to_string(&km).unwrap();
        let back: Keymap = serde_json::from_str(&json).unwrap();
        assert_eq!(back.key_for("save").as_deref(), Some("cmd-alt-s"));
        assert_eq!(back.key_for("go_to_file").as_deref(), Some("cmd-shift-o"));
    }
}
