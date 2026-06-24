//! Recent-projects store + helpers for the "no project open" landing view.
//! Persisted as JSON next to the other config (`~/.config/kyde/projects.json`).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Recents {
    /// Most-recent first.
    pub paths: Vec<PathBuf>,
}

impl Recents {
    fn config_path() -> PathBuf {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(home).join(".config")
            });
        base.join("kyde").join("projects.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Move `path` to the front (most recent), de-duplicating.
    pub fn touch(&mut self, path: &Path) {
        let path = path.to_path_buf();
        self.paths.retain(|p| p != &path);
        self.paths.insert(0, path);
        self.paths.truncate(50);
    }
}

/// Last path component as the project name.
pub fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Abbreviate a path with `~` for the home directory (display only).
pub fn pretty_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = s.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    s.into_owned()
}

/// Current branch, read straight from `.git/HEAD` (fast, no shell). Not currently shown in the
/// projects list (rows are name + path only) but kept for reuse.
#[allow(dead_code)]
pub fn branch_of(path: &Path) -> Option<String> {
    let head = std::fs::read_to_string(path.join(".git").join("HEAD")).ok()?;
    let head = head.trim();
    head.strip_prefix("ref: refs/heads/")
        .map(str::to_string)
        .or_else(|| Some(format!("{}…", &head.get(0..7).unwrap_or(head))))
}

/// 1–2 letter initials for the project icon.
pub fn initials(name: &str) -> String {
    let parts: Vec<&str> = name
        .split(['-', '_', ' ', '.'])
        .filter(|s| !s.is_empty())
        .collect();
    match parts.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect::<String>().to_uppercase(),
        [a, b, ..] => format!(
            "{}{}",
            a.chars().next().unwrap_or('?'),
            b.chars().next().unwrap_or('?')
        )
        .to_uppercase(),
    }
}

/// Stable icon color (0xRRGGBB) derived from the name — like JetBrains' project chips.
pub fn color_for(name: &str) -> u32 {
    const PALETTE: &[u32] = &[
        0xE8744F, 0x9B7BE8, 0x4F90E8, 0x73BD79, 0xD6A14F, 0xC77DBB, 0x4FB6C7, 0xE85F87,
    ];
    let mut h: u32 = 2166136261;
    for b in name.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    PALETTE[(h as usize) % PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_from_names() {
        assert_eq!(initials("account"), "AC");
        assert_eq!(initials("hoxtonmix-www"), "HW");
        assert_eq!(initials("content-os"), "CO");
    }

    #[test]
    fn touch_moves_to_front_and_dedupes() {
        let mut r = Recents::default();
        r.touch(Path::new("/a"));
        r.touch(Path::new("/b"));
        r.touch(Path::new("/a"));
        assert_eq!(r.paths, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[test]
    fn color_is_stable() {
        assert_eq!(color_for("account"), color_for("account"));
    }
}
