//! Scratch files — throwaway files scoped to a project but stored OUTSIDE its folder
//! (under `~/.config/kyde/scratches/<project>/`), so they never show up in git.
//! Pure Rust; the UI in `main.rs` lists/creates/opens them by absolute path.

use std::path::{Path, PathBuf};

/// Languages offered by the "New Scratch File" picker: (label, extension).
pub const LANGS: &[(&str, &str)] = &[
    ("Plain text", "txt"),
    ("JSON", "json"),
    ("TypeScript", "ts"),
    ("JavaScript", "js"),
    ("Rust", "rs"),
    ("Markdown", "md"),
    ("YAML", "yml"),
    ("Shell", "sh"),
    ("CSS", "css"),
];

fn base() -> PathBuf {
    let root = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        });
    root.join("kyde").join("scratches")
}

/// Stable, filesystem-safe key for a project (its folder name; non-alphanumerics → `_`).
fn project_key(project: &Path) -> String {
    let name = project
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into());
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The scratch directory for a project (not created until first use).
pub fn dir_for(project: &Path) -> PathBuf {
    base().join(project_key(project))
}

/// Absolute paths of a project's scratch files, sorted by name.
pub fn list(project: &Path) -> Vec<PathBuf> {
    let dir = dir_for(project);
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

/// Create the next free `scratchN.<ext>` in the project's scratch dir and return its path.
pub fn create(project: &Path, ext: &str) -> std::io::Result<PathBuf> {
    let dir = dir_for(project);
    std::fs::create_dir_all(&dir)?;
    for n in 1..10_000 {
        let path = dir.join(format!("scratch{n}.{ext}"));
        if !path.exists() {
            std::fs::write(&path, "")?;
            return Ok(path);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "too many scratch files",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn key_is_filesystem_safe() {
        assert_eq!(project_key(Path::new("/Users/k/my-app")), "my-app");
        assert_eq!(
            project_key(Path::new("/Users/k/we ird@proj")),
            "we_ird_proj"
        );
    }

    #[test]
    fn dir_is_under_scratches() {
        let d = dir_for(Path::new("/x/demo"));
        assert!(d.ends_with("scratches/demo"));
    }
}
