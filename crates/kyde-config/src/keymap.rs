//! Configurable keymap with `WebStorm` and `VSCode` presets, persisted as JSON.
//!
//! The actual renderer `Action` types live in `main.rs`; this module owns only the
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
        label: "Settings…",
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
    ActionDef {
        name: "sort_lines",
        webstorm: "cmd-alt-l",
        vscode: "cmd-alt-l",
        label: "Sort Lines",
    },
    ActionDef {
        name: "sort_keys",
        webstorm: "cmd-alt-s",
        vscode: "cmd-alt-s",
        label: "Sort Object Keys",
    },
    ActionDef {
        name: "nav_back",
        webstorm: "cmd-alt-left",
        vscode: "cmd-alt-left",
        label: "Back (previous file)",
    },
    ActionDef {
        name: "nav_forward",
        webstorm: "cmd-alt-right",
        vscode: "cmd-alt-right",
        label: "Forward (next file)",
    },
    // Git: `IntelliJ`'s Push (⌘⇧K) and Update Project (⌘T). `VSCode` ships no default keys for
    // either, so that preset leaves them unbound (bindable via `keymap.json` overrides).
    ActionDef {
        name: "push",
        webstorm: "cmd-shift-k",
        vscode: "",
        label: "Push…",
    },
    ActionDef {
        name: "pull",
        webstorm: "cmd-t",
        vscode: "",
        label: "Pull (Update Project)",
    },
    // The Agents view (active while it's on screen).
    ActionDef {
        name: "agent_next",
        webstorm: "cmd-alt-down",
        vscode: "cmd-alt-down",
        label: "Next Agent",
    },
    ActionDef {
        name: "agent_prev",
        webstorm: "cmd-alt-up",
        vscode: "cmd-alt-up",
        label: "Previous Agent",
    },
    ActionDef {
        name: "agent_attention",
        webstorm: "cmd-alt-j",
        vscode: "cmd-alt-j",
        label: "Jump to the Agent That Needs You",
    },
    ActionDef {
        name: "agent_tab_next",
        webstorm: "cmd-shift-]",
        vscode: "cmd-shift-]",
        label: "Next Agent Tab",
    },
    ActionDef {
        name: "agent_tab_prev",
        webstorm: "cmd-shift-[",
        vscode: "cmd-shift-[",
        label: "Previous Agent Tab",
    },
    ActionDef {
        name: "agent_new_worktree",
        webstorm: "cmd-alt-n",
        vscode: "cmd-alt-n",
        label: "New Worktree",
    },
    ActionDef {
        name: "agent_rename",
        webstorm: "f2",
        vscode: "f2",
        label: "Rename Agent Tab",
    },
    ActionDef {
        name: "agent_scrollback",
        webstorm: "cmd-alt-e",
        vscode: "cmd-alt-e",
        label: "Open Scrollback in Editor",
    },
    ActionDef {
        name: "agent_toggle_sidebar",
        webstorm: "cmd-alt-b",
        vscode: "cmd-alt-b",
        label: "Toggle Agents Sidebar",
    },
];

/// A keystroke string in canonical form — modifiers in `cmd-ctrl-alt-shift-fn` order, then
/// the key — so two spellings of the same shortcut compare equal.
///
/// ```
/// use kyde_config::keymap::normalize_key;
/// assert_eq!(normalize_key("shift-cmd-k"), "cmd-shift-k");
/// assert_eq!(normalize_key("alt-cmd-left"), "cmd-alt-left");
/// assert_eq!(normalize_key("cmd--"), "cmd--");
/// ```
#[must_use]
pub fn normalize_key(key: &str) -> String {
    // The key itself may be `-` (`cmd--`), so split it off before reading the modifiers.
    let (mods, key) = match key.strip_suffix("--") {
        Some(mods) => (mods, "-"),
        None => key.rsplit_once('-').unwrap_or(("", key)),
    };
    let mut out = String::new();
    for m in ["cmd", "ctrl", "alt", "shift", "fn"] {
        if mods.split('-').any(|x| x == m) {
            out.push_str(m);
            out.push('-');
        }
    }
    out.push_str(key);
    out
}

/// A configurable action: its stable name, the default keystroke under each preset, and a
/// human label for the settings UI.
pub struct ActionDef {
    /// Stable action id (used as the override-map key + renderer action name).
    pub name: &'static str,
    /// Default keystroke under the `WebStorm`/`IntelliJ` preset.
    pub webstorm: &'static str,
    /// Default keystroke under the `VSCode` preset (`""` = unbound by default).
    pub vscode: &'static str,
    /// Human-readable label shown in Settings.
    pub label: &'static str,
}

/// A keybinding preset (or `Custom` once the user overrides keys).
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    /// `WebStorm` / `IntelliJ`-style bindings.
    WebStorm,
    /// `VSCode`-style bindings.
    VSCode,
    /// A user-customised binding set.
    Custom,
}

impl Preset {
    /// Human-readable preset name for the settings UI.
    pub fn label(self) -> &'static str {
        match self {
            Preset::WebStorm => "WebStorm / IntelliJ",
            Preset::VSCode => "VSCode",
            Preset::Custom => "Custom",
        }
    }
}

/// The persisted keymap: a [`Preset`] plus any per-action overrides.
#[derive(Clone, Serialize, Deserialize)]
pub struct Keymap {
    /// The active preset.
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
    /// Preset default keystroke for a named action — `None` when the preset leaves it
    /// unbound.
    pub fn preset_key(&self, name: &str) -> Option<&'static str> {
        ACTIONS
            .iter()
            .find(|a| a.name == name)
            .map(|a| match self.preset {
                Preset::VSCode => a.vscode,
                _ => a.webstorm, // Custom falls back to WebStorm defaults for unset actions
            })
            .filter(|k| !k.is_empty())
    }

    /// Effective keystroke for a named action (override wins over preset); `None` = unbound.
    /// An empty override is an explicit unbind of a key the preset would give it.
    pub fn key_for(&self, name: &str) -> Option<String> {
        match self.overrides.get(name) {
            Some(k) if k.is_empty() => None,
            Some(k) => Some(k.clone()),
            None => self.preset_key(name).map(str::to_string),
        }
    }

    /// Whether `name`'s shortcut differs from its preset default.
    pub fn is_customised(&self, name: &str) -> bool {
        self.overrides.contains_key(name)
    }

    /// Bind `name` to `key`. Any other action on the same keystroke is unbound — one key never
    /// fires two actions — and its label returned so the UI can say so. Binding an action back
    /// to its preset default drops the override.
    pub fn rebind(&mut self, name: &str, key: &str) -> Vec<&'static str> {
        let key = normalize_key(key);
        let mut freed = Vec::new();
        for a in ACTIONS {
            if a.name != name
                && self
                    .key_for(a.name)
                    .is_some_and(|k| normalize_key(&k) == key)
            {
                self.unbind(a.name);
                freed.push(a.label);
            }
        }
        if self
            .preset_key(name)
            .is_some_and(|k| normalize_key(k) == key)
        {
            self.overrides.remove(name);
        } else {
            self.overrides.insert(name.to_string(), key);
        }
        freed
    }

    /// Remove `name`'s shortcut (an empty override when the preset would bind it).
    pub fn unbind(&mut self, name: &str) {
        if self.preset_key(name).is_some() {
            self.overrides.insert(name.to_string(), String::new());
        } else {
            self.overrides.remove(name);
        }
    }

    /// Put `name` back on its preset default, taking that key from any action now using it
    /// (their labels are returned, as with [`Keymap::rebind`]).
    pub fn reset(&mut self, name: &str) -> Vec<&'static str> {
        if let Some(k) = self.preset_key(name) {
            self.rebind(name, k)
        } else {
            self.overrides.remove(name);
            Vec::new()
        }
    }

    /// Switch to `preset`, clearing any per-action overrides.
    pub fn set_preset(&mut self, preset: Preset) {
        self.preset = preset;
        self.overrides.clear();
    }

    // ── persistence ───────────────────────────────────────────────
    /// Config file name under the XDG config dir.
    const FILE: &'static str = "keymap.json";

    /// Path to `keymap.json` under the XDG config dir.
    #[must_use]
    pub fn config_path() -> PathBuf {
        crate::store::config_path(Self::FILE)
    }

    /// Load from disk. Returns (keymap, `first_run`) — `first_run` is true when no
    /// usable config existed yet (used to trigger onboarding).
    #[must_use]
    pub fn load() -> (Self, bool) {
        crate::store::load_reporting_missing(Self::FILE)
    }

    /// Persist the keymap to `keymap.json` (best-effort).
    pub fn save(&self) {
        crate::store::save(Self::FILE, self);
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

    /// Push / Pull follow the preset: `IntelliJ` keys under `WebStorm`, unbound (not an empty
    /// keystroke) under `VSCode` until the user binds one.
    #[test]
    fn git_actions_follow_the_preset() {
        let mut km = Keymap::default();
        assert_eq!(km.key_for("push").as_deref(), Some("cmd-shift-k"));
        assert_eq!(km.key_for("pull").as_deref(), Some("cmd-t"));
        km.set_preset(Preset::VSCode);
        assert_eq!(km.key_for("push"), None);
        km.overrides.insert("push".into(), "cmd-alt-p".into());
        assert_eq!(km.key_for("push").as_deref(), Some("cmd-alt-p"));
    }

    /// The keymap editor's model: taking another action's key frees it (one key never fires
    /// two actions), a per-action reset takes the default back, binding the default drops the
    /// override, and resetting to a preset clears every customisation.
    #[test]
    fn rebind_frees_a_taken_key_and_reset_takes_it_back() {
        let mut km = Keymap::default();
        assert_eq!(km.rebind("push", "shift-cmd-o"), ["Go to File"]);
        assert_eq!(km.key_for("push").as_deref(), Some("cmd-shift-o"));
        assert_eq!(km.key_for("go_to_file"), None);

        assert_eq!(km.reset("go_to_file"), ["Push…"]);
        assert_eq!(km.key_for("go_to_file").as_deref(), Some("cmd-shift-o"));
        assert_eq!(
            km.key_for("push"),
            None,
            "push lost the key, and isn't re-bound"
        );

        assert!(km.rebind("push", "cmd-shift-k").is_empty());
        assert!(
            !km.is_customised("push"),
            "binding the default drops the override"
        );
        km.rebind("save", "cmd-alt-p");
        km.set_preset(Preset::WebStorm);
        assert!(km.overrides.is_empty());
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
