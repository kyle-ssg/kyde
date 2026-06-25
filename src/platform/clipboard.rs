//! Reading file paths off the system clipboard (e.g. after a Finder ⌘C), so the Browse
//! tree can paste real files into a folder. gpui's clipboard is text/image only, so the
//! file-URL list needs native pasteboard access (macOS `NSPasteboard`). Non-macOS returns
//! empty for now.
//!
//! NOTE: the UI wiring (a `cmd-v` binding + a replace-confirm modal) lives in `main.rs` /
//! `app.rs` / `render.rs` and is intentionally not added yet — those files are being edited
//! in another session. This `allow` is temporary until the paste action is wired in.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Paths of any files currently on the system clipboard (Finder copy puts `public.file-url`
/// items on the general pasteboard). Empty when the clipboard holds no files.
#[cfg(target_os = "macos")]
pub fn read_files() -> Vec<PathBuf> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL};
    let mut out = Vec::new();
    // SAFETY: standard AppKit pasteboard reads; all returned objects are autoreleased
    // Retained handles managed by objc2.
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        let Some(items) = pb.pasteboardItems() else {
            return out;
        };
        for item in items.iter() {
            if let Some(s) = item.stringForType(NSPasteboardTypeFileURL) {
                if let Some(p) = file_url_to_path(&s.to_string()) {
                    out.push(p);
                }
            }
        }
    }
    out
}

#[cfg(not(target_os = "macos"))]
pub fn read_files() -> Vec<PathBuf> {
    Vec::new()
}

/// Turn a `file://` URL into a filesystem path: strip the scheme + optional host, then
/// percent-decode. Returns `None` for non-`file:` strings. Pure → unit-tested.
pub fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // After `file://` comes an optional host (empty for local) then the absolute path
    // beginning at the first `/`. Local Finder URLs are `file:///Users/...`.
    let path = match rest.find('/') {
        Some(i) => &rest[i..],
        None => rest,
    };
    Some(PathBuf::from(percent_decode(path)))
}

/// Minimal `%XX` percent-decoder (UTF-8). Mirrors `url_encode` in `main.rs` in reverse.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Destination path when copying `src` into `dest_dir` (keeps the source's file name).
pub fn dest_for(src: &Path, dest_dir: &Path) -> Option<PathBuf> {
    src.file_name().map(|n| dest_dir.join(n))
}

/// Copy `src` (file or directory, recursively) to `dest_dir/<name>`, overwriting an
/// existing destination. Caller decides whether to ask before clobbering.
pub fn copy_into(src: &Path, dest_dir: &Path) -> std::io::Result<PathBuf> {
    let dst = dest_for(src, dest_dir)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    if src.is_dir() {
        copy_dir_all(src, &dst)?;
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, &dst)?;
    }
    Ok(dst)
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_local_file_urls() {
        assert_eq!(
            file_url_to_path("file:///Users/me/My%20File.txt"),
            Some(PathBuf::from("/Users/me/My File.txt"))
        );
        // Host segment (rare) is skipped; path starts at the first slash after it.
        assert_eq!(
            file_url_to_path("file://localhost/tmp/a.rs"),
            Some(PathBuf::from("/tmp/a.rs"))
        );
        assert_eq!(file_url_to_path("https://example.com"), None);
    }
}
