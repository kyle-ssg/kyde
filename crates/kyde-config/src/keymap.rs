//! Configurable keymap with `WebStorm` and `VSCode` presets, persisted as JSON.
//!
//! The actual renderer `Action` types live in `main.rs`; this module owns only the
//! *configuration* (which keystroke triggers which named action) and the preset
//! defaults. `main::apply_keymap` reads `key_for()` and binds the real actions.
//!
//! Keys are written the macOS way on every OS (`cmd` = ⌘), so presets and a saved
//! `keymap.json` are the same everywhere; [`platform_key`] turns one into the keystroke this OS
//! binds (`cmd` is Ctrl off macOS).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

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

/// The keystroke a keymap key binds on this OS. A keymap writes the primary shortcut modifier
/// as `cmd` on every OS: it is ⌘ on macOS (the key comes back unchanged) and Ctrl on Linux and
/// Windows, where apps put their shortcuts on Ctrl and Super belongs to the desktop.
///
/// ```
/// use kyde_config::keymap::platform_key;
/// let save = if cfg!(target_os = "macos") { "cmd-s" } else { "ctrl-s" };
/// assert_eq!(platform_key("cmd-s"), save);
/// ```
#[must_use]
pub fn platform_key(key: &str) -> String {
    if cfg!(target_os = "macos") {
        key.to_string()
    } else {
        ctrl_key(key, cfg!(any(target_os = "linux", target_os = "freebsd")))
    }
}

/// What Shift + each symbol key types on the active keyboard layout, named as the Linux backend
/// names keys (`"2"` → `"\""` on a UK layout). `None` until the app has read the layout; the
/// fold then assumes a US layout, as gpui does for a key it can't name.
static LAYOUT_SHIFTED: RwLock<Option<BTreeMap<String, String>>> = RwLock::new(None);

/// Hand the keymap the active layout's Shift + symbol keys (`None` = assume a US layout). The app
/// reads them when it starts and again whenever the layout changes, then re-binds every key.
pub fn set_layout_shifted(map: Option<BTreeMap<String, String>>) {
    *LAYOUT_SHIFTED
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = map;
}

/// What Shift + `key` types on the active layout (see [`set_layout_shifted`]).
fn layout_shifted(key: &str) -> Option<String> {
    let map = LAYOUT_SHIFTED
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match map.as_ref() {
        Some(m) => m.get(key).cloned(),
        None => us_shifted(key).map(str::to_string),
    }
}

/// `key` with `cmd` as Ctrl (written once if both are there). With `fold_shift`, Shift plus a
/// symbol becomes the symbol it types on the active layout (`ctrl-shift-]` → `ctrl-}` on a US
/// layout, `ctrl-shift-2` → `ctrl-"` on a UK one): gpui's Linux backend reports the typed
/// character with Shift dropped, so the unfolded binding never fires.
fn ctrl_key(key: &str, fold_shift: bool) -> String {
    let (mods, key) = match key.strip_suffix("--") {
        Some(mods) => (mods, "-"),
        None => key.rsplit_once('-').unwrap_or(("", key)),
    };
    let mut out: Vec<&str> = Vec::new();
    for m in mods.split('-').filter(|m| !m.is_empty()) {
        let m = if m == "cmd" { "ctrl" } else { m };
        if !out.contains(&m) {
            out.push(m);
        }
    }
    let mut key = key.to_string();
    if fold_shift && out.contains(&"shift") {
        if let Some(typed) = layout_shifted(&key) {
            out.retain(|m| *m != "shift");
            key = typed;
        }
    }
    let mut joined = out.join("-");
    if !joined.is_empty() {
        joined.push('-');
    }
    joined.push_str(&key);
    joined
}

/// The character Shift + `key` types on a US layout, for the symbol and digit keys.
fn us_shifted(key: &str) -> Option<&'static str> {
    const PAIRS: [(&str, &str); 21] = [
        ("`", "~"),
        ("1", "!"),
        ("2", "@"),
        ("3", "#"),
        ("4", "$"),
        ("5", "%"),
        ("6", "^"),
        ("7", "&"),
        ("8", "*"),
        ("9", "("),
        ("0", ")"),
        ("-", "_"),
        ("=", "+"),
        ("[", "{"),
        ("]", "}"),
        ("\\", "|"),
        (";", ":"),
        ("'", "\""),
        (",", "<"),
        (".", ">"),
        ("/", "?"),
    ];
    PAIRS.iter().find(|(k, _)| *k == key).map(|(_, s)| *s)
}

/// Linux defaults for the preset keys a Linux desktop keeps for itself, or that only work on a
/// US layout. Cinnamon and GNOME switch workspaces on Ctrl+Alt+arrows and Cinnamon locks the
/// screen on Ctrl+Alt+L, so those never reach the app; Shift + a bracket types a different
/// character on many layouts. Each replacement is a key both desktops leave free and every
/// layout has: Alt+←/→ for Back/Forward (the browser keys), Ctrl+PageUp/PageDown for tabs,
/// Ctrl+Alt+PageUp/PageDown for agents, and F9 for Sort Lines.
const LINUX_KEYS: [(&str, &str); 7] = [
    ("nav_back", "alt-left"),
    ("nav_forward", "alt-right"),
    ("agent_next", "cmd-alt-pagedown"),
    ("agent_prev", "cmd-alt-pageup"),
    ("agent_tab_next", "cmd-pagedown"),
    ("agent_tab_prev", "cmd-pageup"),
    ("sort_lines", "f9"),
];

/// `key`, or its [`LINUX_KEYS`] replacement on a Linux desktop.
fn desktop_free_key(name: &str, key: &'static str, linux: bool) -> &'static str {
    if !linux {
        return key;
    }
    LINUX_KEYS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(key, |(_, k)| *k)
}

/// Whether two keymap keys are the same keystroke on this OS (`cmd-k` and `ctrl-k` are one key
/// off macOS).
fn same_key(a: &str, b: &str) -> bool {
    platform_key(&normalize_key(a)) == platform_key(&normalize_key(b))
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
            .map(|k| {
                desktop_free_key(
                    name,
                    k,
                    cfg!(any(target_os = "linux", target_os = "freebsd")),
                )
            })
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
            if a.name != name && self.key_for(a.name).is_some_and(|k| same_key(&k, &key)) {
                self.unbind(a.name);
                freed.push(a.label);
            }
        }
        if self.preset_key(name).is_some_and(|k| same_key(k, &key)) {
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

    /// Off macOS `cmd` is Ctrl, and Shift + a symbol is the symbol gpui's Linux backend
    /// reports (`ctrl-}` for Ctrl+Shift+]); on macOS a key binds exactly as written.
    #[test]
    fn cmd_is_ctrl_off_macos() {
        for (key, linux) in [
            ("cmd-s", "ctrl-s"),
            ("cmd-shift-o", "ctrl-shift-o"),
            ("cmd-alt-left", "ctrl-alt-left"),
            ("cmd-shift-]", "ctrl-}"),
            ("cmd-shift-[", "ctrl-{"),
            ("cmd--", "ctrl--"),
            ("cmd-ctrl-x", "ctrl-x"),
            ("ctrl-shift-g", "ctrl-shift-g"),
            ("f2", "f2"),
        ] {
            assert_eq!(ctrl_key(key, true), linux, "{key}");
        }
        assert_eq!(
            ctrl_key("cmd-shift-]", false),
            "ctrl-shift-]",
            "no fold off Linux"
        );
        if cfg!(target_os = "macos") {
            for a in ACTIONS {
                assert_eq!(platform_key(a.webstorm), a.webstorm);
                assert_eq!(platform_key(a.vscode), a.vscode);
            }
        }
    }

    /// Every preset key stays one action's only key once `cmd` and `ctrl` are the same key
    /// (Linux), and none of them lands on Super.
    #[test]
    fn presets_keep_one_action_per_key_off_macos() {
        for preset in [Preset::WebStorm, Preset::VSCode] {
            let km = Keymap {
                preset,
                overrides: BTreeMap::new(),
            };
            let mut seen = std::collections::HashSet::new();
            for a in ACTIONS {
                let Some(k) = km.key_for(a.name) else {
                    continue;
                };
                let bound = ctrl_key(&normalize_key(&k), true);
                assert!(!bound.split('-').any(|m| m == "cmd"), "{bound}");
                assert!(
                    seen.insert(bound.clone()),
                    "{bound} bound twice in {preset:?}"
                );
            }
        }
    }

    /// Off macOS a key recorded as Ctrl (`cmd-shift-g`) is the VS Code preset's Commit view
    /// key (`ctrl-shift-g`), so taking it frees that action; on macOS they are two keys.
    #[test]
    fn rebind_sees_ctrl_and_cmd_as_one_key_off_macos() {
        let mut km = Keymap::default();
        km.set_preset(Preset::VSCode);
        let freed = km.rebind("push", "cmd-shift-g");
        if cfg!(target_os = "macos") {
            assert!(freed.is_empty());
            assert_eq!(km.key_for("mode_commit").as_deref(), Some("ctrl-shift-g"));
        } else {
            assert_eq!(freed, ["Go to Commit view"]);
            assert_eq!(km.key_for("mode_commit"), None);
        }
        // Recording the preset key back drops the override.
        let mut km = Keymap::default();
        let tab = km.preset_key("agent_tab_next").unwrap();
        km.rebind("agent_tab_next", tab);
        assert!(!km.is_customised("agent_tab_next"));
        // On Linux a Shift + symbol key is recorded as gpui reports it (`cmd-}`), and it is
        // the same key as the one written with Shift, so taking it frees the other action.
        if cfg!(any(target_os = "linux", target_os = "freebsd")) {
            km.rebind("agent_tab_next", "cmd-shift-]");
            assert_eq!(km.rebind("agent_tab_prev", "cmd-}"), ["Next Agent Tab"]);
            assert_eq!(km.key_for("agent_tab_next"), None);
        }
    }

    /// On a Linux desktop no preset key lands on a key Cinnamon or GNOME keeps (Ctrl+Alt+arrows,
    /// Ctrl+Alt+L) or on Shift + a symbol; on macOS every preset key is unchanged.
    #[test]
    fn linux_defaults_leave_the_desktop_keys_alone() {
        const TAKEN: [&str; 5] = [
            "ctrl-alt-left",
            "ctrl-alt-right",
            "ctrl-alt-up",
            "ctrl-alt-down",
            "ctrl-alt-l",
        ];
        for a in ACTIONS {
            for key in [a.webstorm, a.vscode].into_iter().filter(|k| !k.is_empty()) {
                assert_eq!(desktop_free_key(a.name, key, false), key);
                let linux = desktop_free_key(a.name, key, true);
                let bound = ctrl_key(&normalize_key(linux), false);
                assert!(!TAKEN.contains(&bound.as_str()), "{} -> {bound}", a.name);
                let (mods, last) = bound.rsplit_once('-').unwrap_or(("", &bound));
                let symbol = last.chars().count() == 1 && !last.chars().all(char::is_alphanumeric);
                assert!(!(symbol && mods.contains("shift")), "{} -> {bound}", a.name);
            }
        }
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
