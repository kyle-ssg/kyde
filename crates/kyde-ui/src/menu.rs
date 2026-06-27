//! Context-menu row icon, keyed off the row label (so call sites stay `item("…")`).

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
        "Rename" => "icons/pencil.svg",
        "Delete" => "icons/trash.svg",
        "Git History" => "icons/history.svg",
        "Reveal in Finder" => "icons/folder.svg",
        "View Diff" | "Show Diff" => "icons/file-lines.svg",
        _ if l.starts_with("Close") => "icons/x.svg",
        _ if l.starts_with("Compare") => "icons/git-branch.svg",
        _ => return None,
    })
}
