//! "Install shell command" — drop a launcher symlink into `~/.local/bin` so Kyde
//! opens from any terminal (`ky`, or `kyde` if `ky` is taken), VSCode-style.
//!
//! No shell-rc editing: `~/.local/bin` is already on PATH and user-writable, so
//! there's no dotfile surgery and no sudo. The symlink points at our own
//! executable (`current_exe`), which `cargo build` overwrites in place, so it
//! stays valid across rebuilds (and points into the bundle for a shipped `.app`).
//! A Linux `AppImage` is the exception: there it points at the `.AppImage` file
//! itself (see `launcher_target`).
//!
//! Pure + unit-tested. The gpui checkbox in `render_onboarding` only calls
//! `state()` (to render) and `install()` (on Continue).

use std::path::{Path, PathBuf};

/// Command names we try, in order of preference (short first).
pub const NAMES: &[&str] = &["ky", "kyde"];

/// Install target: `~/.local/bin` (created on install if missing).
fn install_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("bin"))
}

/// What the launcher symlink points at: our own executable — except inside a Linux
/// `AppImage`, where that is a path in the image's temporary mount (`/tmp/.mount_…`), new
/// on every launch and gone once Kyde quits. The `AppImage` runtime exports the image file's
/// own path as `$APPIMAGE`, which stays put, so link to that instead.
fn launcher_target() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(appimage_or(
        std::env::var_os("APPIMAGE"),
        std::env::var_os("APPDIR"),
        exe,
    ))
}

/// Pure core of `launcher_target`: the `.AppImage` file when running from one (Linux
/// only), else `exe`. "From one" = `exe` lives under the image's mount (`$APPDIR`): shells
/// in Kyde's terminal inherit `$APPIMAGE`, so a dev build started from one must still link
/// to itself.
fn appimage_or(
    appimage: Option<std::ffi::OsString>,
    appdir: Option<std::ffi::OsString>,
    exe: PathBuf,
) -> PathBuf {
    if cfg!(target_os = "linux") && appdir.is_some_and(|d| exe.starts_with(d)) {
        if let Some(image) = appimage.map(PathBuf::from).filter(|p| p.is_file()) {
            return image;
        }
    }
    exe
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// A launcher pointing at us already exists, under this name.
    Installed(String),
    /// Not installed yet; this name is free to claim.
    Available(String),
    /// Both names are occupied by *other* commands on PATH — we won't clobber.
    NameTaken,
    /// Can't resolve HOME or our own executable path.
    Unavailable,
}

/// Is there already an entry for `name` in any of `dirs`? `Some(true)` = it's
/// our own symlink (already installed), `Some(false)` = some other command (a
/// conflict we must not clobber), `None` = nothing there (free to claim).
/// First directory wins — callers put the install dir first.
fn existing_in(name: &str, exe: &Path, dirs: &[PathBuf]) -> Option<bool> {
    for d in dirs {
        let p = d.join(name);
        if std::fs::symlink_metadata(&p).is_ok() {
            let is_us = std::fs::read_link(&p).is_ok_and(|t| t == exe);
            return Some(is_us);
        }
    }
    None
}

/// Pure core: decide the state from our exe path and the dirs to scan. A name
/// already pointing at us wins (Installed); otherwise the first name with
/// nothing on PATH is offered (Available); if every name is taken by something
/// else, `NameTaken`.
fn state_in(exe: &Path, scan: &[PathBuf]) -> State {
    for n in NAMES {
        if existing_in(n, exe, scan) == Some(true) {
            return State::Installed((*n).to_string());
        }
    }
    for n in NAMES {
        if existing_in(n, exe, scan).is_none() {
            return State::Available((*n).to_string());
        }
    }
    State::NameTaken
}

/// Install dir first (authoritative — it's where we write and it's on PATH),
/// then the rest of PATH so we honour a `ky` that lives elsewhere.
fn scan_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut v = vec![dir.to_path_buf()];
    v.extend(path_dirs());
    v
}

/// Inspect the current state without changing anything.
pub fn state() -> State {
    let (Some(exe), Some(dir)) = (launcher_target().ok(), install_dir()) else {
        return State::Unavailable;
    };
    state_in(&exe, &scan_dirs(&dir))
}

/// Create the launcher symlink. Returns the installed name, or an error string.
/// Idempotent: a no-op (returns the name) if already installed.
pub fn install() -> Result<String, String> {
    let exe = launcher_target().map_err(|e| e.to_string())?;
    let dir = install_dir().ok_or_else(|| "HOME is not set".to_string())?;
    match state_in(&exe, &scan_dirs(&dir)) {
        State::Installed(n) => Ok(n),
        State::Unavailable => Err("couldn't resolve install location".to_string()),
        State::NameTaken => {
            Err("`ky` and `kyde` are both taken by other commands on your PATH".to_string())
        }
        State::Available(name) => {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let link = dir.join(&name);
            // Clear a stale broken symlink under our name, if any, then link.
            let _ = std::fs::remove_file(&link);
            symlink(&exe, &link).map_err(|e| e.to_string())?;
            Ok(name)
        }
    }
}

/// Create a symlink `dst` → `src`. Unix-only feature (the install model is `~/.local/bin`
/// on PATH); on other platforms it's an error so the binary still *compiles* for Windows
/// (which resolves no `HOME`, so `state()` already reports `Unavailable` there anyway).
#[cfg(unix)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}
#[cfg(not(unix))]
fn symlink(_src: &Path, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "installing a shell command is only supported on Unix",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// A fresh, unique temp working dir (no external crates).
    fn workdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "kyde-shellcmd-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn setup() -> (PathBuf, PathBuf) {
        let work = workdir();
        let exe = work.join("kyde-bin");
        std::fs::write(&exe, b"x").unwrap();
        let bin = work.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        (exe, bin)
    }

    #[cfg(unix)]
    #[test]
    fn available_then_installed_roundtrip() {
        let (exe, bin) = setup();
        let scan = vec![bin.clone()];
        // Nothing there → offer the short name.
        assert_eq!(state_in(&exe, &scan), State::Available("ky".into()));
        // Our own symlink → reads as installed under that name.
        std::os::unix::fs::symlink(&exe, bin.join("ky")).unwrap();
        assert_eq!(state_in(&exe, &scan), State::Installed("ky".into()));
    }

    #[test]
    fn falls_back_when_ky_taken_by_other() {
        let (exe, bin) = setup();
        // `ky` is some unrelated command → don't clobber, offer `kyde`.
        std::fs::write(bin.join("ky"), b"other").unwrap();
        assert_eq!(state_in(&exe, &[bin]), State::Available("kyde".into()));
    }

    #[test]
    fn name_taken_when_both_occupied_by_others() {
        let (exe, bin) = setup();
        std::fs::write(bin.join("ky"), b"a").unwrap();
        std::fs::write(bin.join("kyde"), b"b").unwrap();
        assert_eq!(state_in(&exe, &[bin]), State::NameTaken);
    }

    #[test]
    fn appimage_launcher_points_at_the_image_file_on_linux_only() {
        let (exe, bin) = setup();
        let image = bin.join("Kyde-x86_64.AppImage");
        std::fs::write(&image, b"img").unwrap();
        let mount = exe.parent().unwrap().as_os_str().to_owned();
        let img = || Some(image.clone().into_os_string());
        // Inside an AppImage (Linux): the stable `.AppImage` path, not the temporary mount.
        let want = if cfg!(target_os = "linux") {
            image.clone()
        } else {
            exe.clone()
        };
        assert_eq!(appimage_or(img(), Some(mount.clone()), exe.clone()), want);
        // Not in an AppImage, or `$APPIMAGE` names no file: our own executable.
        assert_eq!(appimage_or(None, None, exe.clone()), exe);
        let gone = bin.join("moved.AppImage").into_os_string();
        assert_eq!(appimage_or(Some(gone), Some(mount), exe.clone()), exe);
        // `$APPIMAGE` inherited by a build started from Kyde's terminal: it isn't running
        // from the image's mount, so it links to itself.
        let other_mount = bin.join(".mount_Kyde").into_os_string();
        assert_eq!(appimage_or(img(), Some(other_mount), exe.clone()), exe);
        assert_eq!(appimage_or(img(), None, exe.clone()), exe);
    }

    #[cfg(unix)]
    #[test]
    fn our_symlink_elsewhere_on_path_counts_as_installed() {
        let (exe, bin) = setup();
        // Install dir empty, but a *later* PATH dir already has our `ky`.
        let other = bin.parent().unwrap().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(&exe, other.join("ky")).unwrap();
        let scan = vec![bin, other];
        assert_eq!(state_in(&exe, &scan), State::Installed("ky".into()));
    }
}
