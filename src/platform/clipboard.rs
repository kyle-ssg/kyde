//! Reading file paths off the OS clipboard, so files **copied in Finder** (or a Linux file
//! manager) can be pasted into Kyde's tree (issue #67). gpui's `read_from_clipboard` handles
//! text + images but not file URLs, so this reaches `NSPasteboard` directly via objc2 on
//! macOS, and the X server's clipboard on Linux. Image data pasting rides gpui's
//! `ClipboardEntry::Image`; only the file-URL case needs this module.

use std::path::PathBuf;

/// File paths currently on the general pasteboard (e.g. files ⌘C-copied in Finder), or an
/// empty vec when the clipboard holds no file URLs. macOS only; empty elsewhere.
#[cfg(target_os = "macos")]
pub(crate) fn file_paths() -> Vec<PathBuf> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL};

    // SAFETY: `generalPasteboard` returns the process-wide shared pasteboard; reading its
    // items and their `public.file-url` string is a pure read (no mutation, no ownership
    // transfer). Every returned Objective-C object is objc2-retained, and we only borrow
    // it to copy out a Rust `String`, so there are no lifetime or threading hazards.
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        let Some(items) = pb.pasteboardItems() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for item in &items {
            if let Some(s) = item.stringForType(NSPasteboardTypeFileURL) {
                // The value is a `file://` URL string; turn it back into a filesystem path.
                if let Some(p) = url_to_path(&s.to_string()) {
                    out.push(p);
                }
            }
        }
        out
    }
}

/// File paths on the clipboard (files Ctrl+C-copied in Nautilus, Nemo, Dolphin, Thunar…),
/// or an empty vec when it holds none. gpui reads only text + images off the Linux
/// clipboard, so this asks the X server for the `text/uri-list` that file managers offer,
/// then GNOME's `x-special/gnome-copied-files`. On a Wayland desktop that is Xwayland, which
/// mirrors the Wayland clipboard; with no X display at all the list is empty.
#[cfg(target_os = "linux")]
pub(crate) fn file_paths() -> Vec<PathBuf> {
    let Ok(clip) = x11_clipboard::Clipboard::new() else {
        return Vec::new();
    };
    let atoms = &clip.getter.atoms;
    for target in ["text/uri-list", "x-special/gnome-copied-files"] {
        let Ok(target) = clip.getter.get_atom(target) else {
            continue;
        };
        // Bounded: a clipboard owner that never answers must not hang the paste.
        let wait = std::time::Duration::from_millis(500);
        if let Ok(list) = clip.load(atoms.clipboard, target, atoms.property, wait) {
            let paths = uri_list_paths(&String::from_utf8_lossy(&list));
            if !paths.is_empty() {
                return paths;
            }
        }
    }
    Vec::new()
}

/// The local files in a `text/uri-list` (`file://` URLs, one per line, `#` comments) or a
/// GNOME `x-special/gnome-copied-files` (the same, after a `copy` / `cut` line). Pure.
#[cfg(target_os = "linux")]
fn uri_list_paths(list: &str) -> Vec<PathBuf> {
    list.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(url_to_path)
        .filter(|p| p.is_absolute())
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn file_paths() -> Vec<PathBuf> {
    Vec::new()
}

/// Turn a `file://` URL into a filesystem path, percent-decoding the path portion.
/// Returns `None` for a non-file URL. Pure — unit-tested below.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // Drop an authority ("//host") if present — local file URLs use an empty authority, so
    // the path starts right after the scheme's "//".
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    Some(PathBuf::from(percent_decode(path)))
}

/// Minimal percent-decoding for file-URL paths (`%20` → space, etc.). Pure.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn file_url_decodes_to_path() {
        assert_eq!(
            url_to_path("file:///Users/me/My%20Docs/a.txt"),
            Some(PathBuf::from("/Users/me/My Docs/a.txt"))
        );
        assert_eq!(
            url_to_path("file://localhost/tmp/x"),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(url_to_path("https://example.com"), None);
    }

    #[test]
    fn percent_decode_passthrough_and_escapes() {
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_file_lists_parse_to_paths() {
        // text/uri-list: CRLF lines, a comment, a non-file URL.
        assert_eq!(
            uri_list_paths(
                "# copied\r\nfile:///home/me/My%20Docs/a.txt\r\nfile:///tmp/b\r\nhttps://x.org\r\n"
            ),
            [
                PathBuf::from("/home/me/My Docs/a.txt"),
                PathBuf::from("/tmp/b")
            ]
        );
        // GNOME / Nemo: the operation first, no trailing newline; UTF-8 escapes decode.
        assert_eq!(
            uri_list_paths("cut\nfile:///tmp/a\nfile:///tmp/%C3%A9"),
            [PathBuf::from("/tmp/a"), PathBuf::from("/tmp/é")]
        );
        // A file URL naming another host is not a local path.
        assert!(uri_list_paths("file://server/share/x").is_empty());
        assert!(uri_list_paths("just some copied text").is_empty());
        assert!(uri_list_paths("").is_empty());
    }
}
