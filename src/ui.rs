//! Shared UI toolkit — the reusable widget builders + small helpers used across every view
//! (`btn_primary`/`btn_secondary`, tab pills, context-menu icons, the panic-safe scrollbar
//! thumb maths, colour lerp, plugin-pack metadata). A crate-root child module, re-exported at
//! the root so every feature module gets these via `use super::*`. See the "Buttons" UI
//! principle in CLAUDE.md.

use super::*;

/// Representative file extension for a pack id, so `file_badge` can pick the language
/// monogram chip shown in the plugin manager.
pub(crate) fn pack_ext(id: &str) -> &'static str {
    match id {
        "json" => "json",
        "typescript" => "ts",
        "javascript" => "js",
        "rust" => "rs",
        "markdown" => "md",
        "shell" => "sh",
        "css" => "css",
        "scss" => "scss",
        "yaml" => "yml",
        "toml" => "toml",
        "python" => "py",
        "html" => "html",
        "go" => "go",
        "env" => "env",
        "gitignore" => "gitignore",
        "font" => "ttf",
        _ => "txt",
    }
}

/// Approximate compiled footprint of a pack's grammar (tree-sitter parse tables linked
/// into the binary). These ship in the binary rather than being downloaded, so this is the
/// resident size each adds — a rough, static figure, not an exact per-build measurement.
pub(crate) fn pack_size(id: &str) -> &'static str {
    match id {
        "json" => "~55 KB",
        "typescript" => "~2.6 MB",
        "javascript" => "~1.1 MB",
        "rust" => "~1.6 MB",
        "markdown" => "~210 KB",
        "shell" => "~480 KB",
        "css" => "~260 KB",
        "scss" => "shares CSS grammar",
        "yaml" => "~150 KB",
        "toml" => "~120 KB",
        "python" => "~900 KB",
        "html" => "~120 KB",
        "go" => "~700 KB",
        "env" | "gitignore" => "built-in (no grammar)",
        "font" => "preview only",
        _ => "—",
    }
}

/// Standard **secondary** button (transparent fill + divider border + secondary text).
/// Caller chains `.on_mouse_down(...)`. See the "Buttons" UI principle in CLAUDE.md.
pub(crate) fn btn_secondary(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    div()
        .id(id)
        .px_4()
        // 4px (was 6px) vertical pad → ~4px shorter button universally.
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(t.divider)
        .text_color(t.secondary_text)
        .cursor_pointer()
        .hover(|s| s.bg(t.bg_mid))
        .child(label.into())
}

/// Icon path for a context-menu row, keyed off its label (so call sites stay `item("…")`).
/// Tolerates a leading "✓ " (compare modes) and a trailing "…". `None` → no icon (e.g. tab
/// file-name rows), which still reserves the icon slot so labels line up.
pub(crate) fn menu_icon(label: &str) -> Option<&'static str> {
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

/// One pill of a tab strip (e.g. the git view's Commit/Push tabs), IntelliJ-style: active =
/// subtle filled bg + faint border; inactive = transparent with a hover bg. A `count` badge
/// shows when > 0 (accent-filled on the active tab). Caller chains `.on_mouse_down(...)`.
pub(crate) fn tab_pill(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    count: usize,
    active: bool,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    let mut d = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when(active, |d| {
            d.bg(t.bg_light)
                .border_1()
                .border_color(t.divider)
                .text_color(t.text)
        })
        .when(!active, |d| {
            d.text_color(t.line_number).hover(|d| d.bg(t.bg_mid))
        })
        .child(label.into());
    if count > 0 {
        d = d.child(
            div()
                .flex_none()
                .px(px(5.0))
                .rounded_sm()
                .bg(if active { t.primary } else { t.bg_light })
                .text_size(px(10.0))
                .text_color(if active {
                    t.primary_text
                } else {
                    t.secondary_text
                })
                .child(SharedString::from(count.to_string())),
        );
    }
    d
}

/// Standard **primary** button (accent fill + primary text). Caller chains `.on_mouse_down`.
pub(crate) fn btn_primary(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
) -> gpui::Stateful<gpui::Div> {
    btn_primary_state(id, label, false)
}

/// `btn_primary` with an explicit `disabled` state. gpui allows only ONE `.hover()` per
/// element (a second one panics: "hover style already set"), so the disabled look — dimmed,
/// and *staying* dimmed on hover — must be baked into the single hover here rather than
/// chained on by the caller.
pub(crate) fn btn_primary_state(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    disabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let t = theme::get();
    let b = div()
        .id(id)
        .px_4()
        // 4px (was 6px) vertical pad → ~4px shorter button universally.
        .py_1()
        .rounded_md()
        .bg(t.primary)
        .text_color(t.primary_text);
    let b = if disabled {
        b.opacity(0.6).cursor_default().hover(|s| s.opacity(0.6))
    } else {
        b.cursor_pointer().hover(|s| s.opacity(0.9))
    };
    b.child(label.into())
}

/// Linearly interpolate two `0xRRGGBB` colors (`t` in 0..1) → opaque `Rgba`. Used for the
/// welcome-screen ASCII shimmer.
pub(crate) fn lerp_rgb(a: u32, b: u32, t: f32) -> gpui::Rgba {
    let t = t.clamp(0.0, 1.0);
    let chan = |hex: u32, shift: u32| ((hex >> shift) & 0xFF) as f32 / 255.0;
    let mix = |x: f32, y: f32| x + (y - x) * t;
    gpui::Rgba {
        r: mix(chan(a, 16), chan(b, 16)),
        g: mix(chan(a, 8), chan(b, 8)),
        b: mix(chan(a, 0), chan(b, 0)),
        a: 1.0,
    }
}

/// Scrollbar thumb length + position for a track, kept **panic-safe** at any window size.
/// `track` = usable track length (px), `max` = max scroll offset (px), `off` = current
/// (negative) scroll offset (px), `end` = inset at each track end (px). Returns
/// `(thumb_len, thumb_pos)`, both clamped within the track.
///
/// Why this exists: the thumb length used to be `(…).clamp(28.0, track - 2*end)` inline.
/// `f32::clamp` PANICS when `min > max`, and `track - 2*end` drops below 28 once the window
/// is shrunk past ~44px — so resizing tiny aborted the process (SIGABRT). Pinning the min
/// under the max here makes it impossible. Pure so it can be unit-tested (below).
pub(crate) fn scrollbar_thumb(track: f32, max: f32, off: f32, end: f32) -> (f32, f32) {
    let hi = (track - 2.0 * end).max(8.0);
    let len = if max > 0.0 {
        (track * track / (track + max)).clamp(28.0_f32.min(hi), hi)
    } else {
        hi
    };
    let span = (track - len - 2.0 * end).max(0.0);
    let frac = if max > 0.0 {
        (-off / max).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (len, end + frac * span)
}

#[cfg(test)]
mod tests {
    use super::scrollbar_thumb;

    /// Regression guard for the resize-to-tiny SIGABRT: `scrollbar_thumb` must never panic
    /// and must stay within the track for any size — including tracks far below the thumb
    /// min, zero/huge content, and out-of-range offsets. (The old inline `clamp(28, track-16)`
    /// aborted here.) Keep this — it's the whole reason the helper is pure. See CLAUDE.md.
    #[test]
    fn scrollbar_thumb_never_panics_when_tiny() {
        let tracks = [-50.0, 0.0, 1.0, 10.0, 28.0, 43.9, 44.0, 200.0, 5000.0];
        let maxes = [0.0, 0.5, 1.0, 50.0, 100_000.0];
        let offs = [-1e9, -100.0, 0.0, 50.0, 1e9];
        for &tr in &tracks {
            for &mx in &maxes {
                for &of in &offs {
                    let (len, pos) = scrollbar_thumb(tr, mx, of, 8.0);
                    assert!(len.is_finite() && len > 0.0, "len {len} (track {tr})");
                    assert!(pos.is_finite() && pos >= 0.0, "pos {pos} (track {tr})");
                    // Thumb cannot start past the end of the track.
                    assert!(pos <= tr.max(8.0) + 1.0, "pos {pos} past track {tr}");
                }
            }
        }
    }
}
