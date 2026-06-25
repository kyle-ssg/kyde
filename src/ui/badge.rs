//! File/language badge: the `Badge` model + its renderer + path→badge mapping.
use crate::*;

/// How a file's icon renders in the Browse tree.
pub(crate) enum Badge {
    /// A short colored monogram, e.g. "rs", "{}".
    Tag(&'static str, gpui::Rgba),
    /// An SVG icon (path served by `Assets`), tinted with the given color.
    Icon(&'static str, gpui::Rgba),
    /// A filled rounded "brand" box with bold letters (TS/JS-style), `(text, fg, bg)`.
    Mono(&'static str, gpui::Rgba, gpui::Rgba),
}

/// The inner element for a badge, at a consistent visual size. Callers wrap it in their
/// own fixed-width box / alignment. `bump` adds 2px (file explorer + bottom bar use it;
/// the tabs / commit list stay at the base size).
pub(crate) fn badge_inner(b: Badge, grow: f32) -> gpui::AnyElement {
    let d = grow;
    match b {
        Badge::Tag(label, color) => div()
            .text_size(px(10.0 + d))
            .text_color(color)
            .child(label)
            .into_any_element(),
        Badge::Icon(path, color) => svg()
            .path(path)
            .size(px(14.0 + d))
            .text_color(color)
            .into_any_element(),
        Badge::Mono(text, fg, bg) => div()
            .w(px(16.0 + d))
            .h(px(14.0 + d))
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .bg(bg)
            .child(
                div()
                    .text_size(px(8.0 + d))
                    .font_weight(FontWeight::BOLD)
                    .text_color(fg)
                    .child(text),
            )
            .into_any_element(),
    }
}

pub(crate) fn file_badge(path: &std::path::Path) -> Badge {
    let rgb = |v: u32| gpui::rgb(v);
    // Ignore files (.gitignore, .dockerignore, .prettierignore, …) → a "ban" circle-slash.
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if name.ends_with("ignore") {
            return Badge::Icon("icons/ban.svg", rgb(0x9AA0A6));
        }
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Badge::Tag("rs", rgb(0xDEA584)),
        // Brand-style filled boxes, like the real-world TS/JS icons.
        "ts" | "tsx" => Badge::Mono("TS", rgb(0xFFFFFF), rgb(0x3178C6)),
        "js" | "jsx" | "mjs" | "cjs" => Badge::Mono("JS", rgb(0x1A1A1A), rgb(0xF7DF1E)),
        // JSON: plain braces, no filled box.
        "json" => Badge::Tag("{}", rgb(0xCBCB41)),
        "md" | "markdown" => Badge::Tag("md", rgb(0x7E9BF0)),
        "css" => Badge::Tag("css", rgb(0x3178C6)),
        "scss" | "sass" => Badge::Tag("css", rgb(0xCF649A)),
        "html" | "htm" => Badge::Tag("<>", rgb(0xE44D26)),
        "sh" | "bash" | "zsh" => Badge::Tag("sh", rgb(0x89E051)),
        "yml" | "yaml" => Badge::Tag("yml", rgb(0xD46A6A)),
        "toml" => Badge::Tag("tml", rgb(0x9C4221)),
        "py" | "pyi" => Badge::Tag("py", rgb(0x3572A5)),
        "go" => Badge::Tag("go", rgb(0x00ADD8)),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" | "tiff" | "tif"
        | "svg" => Badge::Icon("icons/image.svg", rgb(0xB180D7)),
        // Generic file (incl. no extension): IntelliJ-style plain-text lines icon.
        _ => Badge::Icon("icons/file-lines.svg", rgb(0x9AA0A6)),
    }
}
