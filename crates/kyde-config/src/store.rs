//! Shared JSON config-file persistence: the XDG config directory + generic
//! load/save. The keymap, plugins, and projects stores all live as JSON under
//! `~/.config/kyde`; without this they each re-implemented the same XDG-path +
//! read-parse-default + create-dirs-write-pretty dance, three slightly different ways.

use serde::{de::DeserializeOwned, Serialize};
use std::path::PathBuf;

/// The `~/.config/kyde` directory that holds every Kyde config file. Honors
/// `XDG_CONFIG_HOME`, falling back to `$HOME/.config` (then `./.config` if even
/// `HOME` is unset — only in degenerate environments).
#[must_use]
pub fn config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME").map_or_else(
        |_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        },
        PathBuf::from,
    );
    base.join("kyde")
}

/// Full path to a named config file (e.g. `"keymap.json"`) under [`config_dir`].
#[must_use]
pub fn config_path(file: &str) -> PathBuf {
    config_dir().join(file)
}

/// Load a JSON config file, falling back to `T::default()` when it's missing or
/// unparseable (a corrupt file should never crash — the store repairs on next save).
#[must_use]
pub fn load_or_default<T: DeserializeOwned + Default>(file: &str) -> T {
    std::fs::read_to_string(config_path(file))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Like [`load_or_default`] but also reports whether the file was absent/invalid —
/// the keymap uses this "was there no usable config?" signal to trigger first-run
/// onboarding.
#[must_use]
pub fn load_reporting_missing<T: DeserializeOwned + Default>(file: &str) -> (T, bool) {
    match std::fs::read_to_string(config_path(file)) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(v) => (v, false),
            Err(_) => (T::default(), true),
        },
        Err(_) => (T::default(), true),
    }
}

/// Persist `value` as pretty JSON to a named config file, creating the config
/// directory if needed. Best-effort: a failed write just means the setting won't
/// survive the next launch, so it's silently ignored rather than surfaced as an error.
pub fn save<T: Serialize>(file: &str, value: &T) {
    let path = config_path(file);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(value) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Sample {
        n: u32,
        s: String,
    }

    #[test]
    fn save_load_round_trip_and_missing_defaults() {
        // Point the config dir at an isolated temp dir (pid keeps parallel `cargo test`
        // runs from colliding on the shared process env).
        let dir = std::env::temp_dir().join(format!("kyde-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        assert_eq!(config_path("x.json"), dir.join("kyde").join("x.json"));

        // Missing file → default (both loaders).
        assert_eq!(load_or_default::<Sample>("sample.json"), Sample::default());
        let (v, missing): (Sample, bool) = load_reporting_missing("sample.json");
        assert_eq!(v, Sample::default());
        assert!(missing, "absent file reports missing=true");

        // Round-trips through disk.
        let want = Sample {
            n: 7,
            s: "hi".into(),
        };
        save("sample.json", &want);
        assert_eq!(load_or_default::<Sample>("sample.json"), want);
        let (v, missing): (Sample, bool) = load_reporting_missing("sample.json");
        assert_eq!(v, want);
        assert!(!missing, "present, valid file reports missing=false");

        // Corrupt file → default, and reports missing=true (drives keymap first-run).
        std::fs::write(config_path("sample.json"), "{ not json").unwrap();
        assert_eq!(load_or_default::<Sample>("sample.json"), Sample::default());
        let (_, missing): (Sample, bool) = load_reporting_missing("sample.json");
        assert!(missing, "corrupt file reports missing=true");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
