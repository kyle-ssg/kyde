//! Reading file paths off the OS clipboard, so files **copied in Finder** can be pasted
//! into Kyde's tree (issue #67). gpui's `read_from_clipboard` handles text + images but not
//! file URLs, so this reaches `NSPasteboard` directly via objc2. Image data pasting rides
//! gpui's `ClipboardEntry::Image`; only the file-URL case needs this module.

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

#[cfg(not(target_os = "macos"))]
pub(crate) fn file_paths() -> Vec<PathBuf> {
    Vec::new()
}

/// Turn a `file://` URL into a filesystem path, percent-decoding the path portion.
/// Returns `None` for a non-file URL. Pure — unit-tested below.
#[cfg(target_os = "macos")]
fn url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // Drop an authority ("//host") if present — local file URLs use an empty authority, so
    // the path starts right after the scheme's "//".
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    Some(PathBuf::from(percent_decode(path)))
}

/// Minimal percent-decoding for file-URL paths (`%20` → space, etc.). Pure.
#[cfg(target_os = "macos")]
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

#[cfg(all(test, target_os = "macos"))]
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
}
