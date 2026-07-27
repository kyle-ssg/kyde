//! Context-menu row icon, keyed off the row label (so call sites stay `item("…")`).

use gpui::prelude::*;
use gpui::{div, px, Div};
use kyde_theme as theme;

/// A subtle full-width separator between context-menu groups (a hairline in the divider
/// colour with a little vertical breathing room). Chain nothing — just `.child(menu_divider())`.
pub fn menu_divider() -> Div {
    div().h(px(1.0)).my_1().mx_2().bg(theme::get().divider)
}

/// Icon path for a context-menu row. Tolerates a leading "✓ " and trailing "…". `None` → no
/// icon (still reserves the slot so labels line up).
pub fn menu_icon(label: &str) -> Option<&'static str> {
    let l = label.trim_start_matches("✓ ").trim_end_matches('…').trim();
    Some(match l {
        "Commit" => "icons/git-commit.svg",
        "Rollback" => "icons/rotate-ccw.svg",
        "Fetch" => "icons/arrow-down-to-line.svg",
        "Pull" => "icons/arrow-down.svg",
        "Push" => "icons/arrow-up.svg",
        "New File" => "icons/file-plus.svg",
        // "New ▸" flyout items (File / Scratch File / Directory).
        "File" => "icons/file-lines.svg",
        "Scratch File" => "icons/file-clock.svg",
        "Directory" => "icons/folder.svg",
        "Rename" => "icons/pencil.svg",
        "Delete" => "icons/trash.svg",
        "Cut" => "icons/scissors.svg",
        "Copy" => "icons/copy.svg",
        "Paste" => "icons/clipboard.svg",
        "Local History" => "icons/file-clock.svg",
        "Git History" => "icons/history.svg",
        "Reveal in Finder" => "icons/folder.svg",
        "View Diff" | "Show Diff" => "icons/file-lines.svg",
        _ if l.starts_with("Close") => "icons/x.svg",
        _ if l.starts_with("Compare") => "icons/git-branch.svg",
        _ => return None,
    })
}
