//! Runtime theme, loaded from `~/.config/kyde/theme.json` (hand-editable hex).
//! Defaults are an original hand-authored dark palette (Darcula-family style).
//! Access the loaded theme anywhere via `theme::get()`; it loads lazily on first use
//! and writes a default file if none exists.

use gpui::Rgba;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::RwLock;

/// 0xRRGGBB → opaque Rgba (compile-time-friendly).
const fn c(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// All themeable colors, flat for easy hand-editing. Serialized as `"#RRGGBB"`.
/// `Copy` (all fields are `Rgba`/`f32`) so `get()` can hand out cheap snapshots from behind
/// the `RwLock` without lifetime juggling.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Theme {
    // Surfaces
    /// Window frame / gaps behind the rounded island panels (darkest surface).
    #[serde(with = "hex")]
    pub frame_bg: Rgba,
    #[serde(with = "hex")]
    pub main_bg: Rgba,
    #[serde(with = "hex")]
    pub panel_bg: Rgba,
    #[serde(with = "hex")]
    pub bg_mid: Rgba,
    #[serde(with = "hex")]
    pub bg_light: Rgba,
    /// General divider / hr / border colour.
    #[serde(with = "hex")]
    pub divider: Rgba,
    /// Native title strip behind the macOS traffic lights. Distinct from `frame_bg` so a
    /// light theme can give the (system-drawn) inactive traffic lights a grey backing —
    /// gpui can't force the window's NSAppearance, so on a near-white strip they wash out.
    #[serde(with = "hex")]
    pub titlebar_bg: Rgba,

    // Text
    /// General text colour (everything except the primary button).
    #[serde(with = "hex")]
    pub text: Rgba,
    #[serde(with = "hex")]
    pub secondary_text: Rgba,
    #[serde(with = "hex")]
    pub line_number: Rgba,

    // Editor
    #[serde(with = "hex")]
    pub caret: Rgba,
    #[serde(with = "hex")]
    pub caret_row: Rgba,
    /// Selected sidebar/menu row background.
    #[serde(with = "hex")]
    pub selected_bg: Rgba,

    // Buttons
    #[serde(with = "hex")]
    pub primary: Rgba,
    #[serde(with = "hex")]
    pub primary_text: Rgba,

    // Git file status
    #[serde(with = "hex")]
    pub status_added: Rgba,
    #[serde(with = "hex")]
    pub status_modified: Rgba,
    #[serde(with = "hex")]
    pub status_deleted: Rgba,
    #[serde(with = "hex")]
    pub status_untracked: Rgba,
    #[serde(with = "hex")]
    pub status_conflict: Rgba,

    // Diff hunk backgrounds
    #[serde(with = "hex")]
    pub diff_inserted_bg: Rgba,
    #[serde(with = "hex")]
    pub diff_deleted_bg: Rgba,
    #[serde(with = "hex")]
    pub diff_modified_bg: Rgba,
    #[serde(with = "hex")]
    pub diff_separator_bg: Rgba,
    // Stronger word-level tint inside a modified line (the exact changed words).
    #[serde(with = "hex")]
    pub diff_word_old_bg: Rgba,
    #[serde(with = "hex")]
    pub diff_word_new_bg: Rgba,

    // Syntax
    #[serde(with = "hex")]
    pub syn_keyword: Rgba,
    #[serde(with = "hex")]
    pub syn_string: Rgba,
    #[serde(with = "hex")]
    pub syn_number: Rgba,
    #[serde(with = "hex")]
    pub syn_comment: Rgba,
    #[serde(with = "hex")]
    pub syn_function: Rgba,
    #[serde(with = "hex")]
    pub syn_field: Rgba,
    #[serde(with = "hex")]
    pub syn_constant: Rgba,
    #[serde(with = "hex")]
    pub syn_identifier: Rgba,
    #[serde(with = "hex")]
    pub syn_operator: Rgba,

    // Font sizes (px). Not colours — plain numbers, hand-editable like the rest.
    /// Code surfaces: editor + diff panes + commit box.
    pub editor_font_size: f32,
    /// UI chrome: tree rows, finder, status bar, menus.
    pub ui_font_size: f32,
    /// File-tree row height (px). small = 22, medium = 30, large = 38.
    pub tree_row_height: f32,
}

/// Theme keys that are plain numbers, not `#RRGGBB` colours (so `merge` validates them as
/// numbers rather than hex).
const NUMERIC_KEYS: &[&str] = &["editor_font_size", "ui_font_size", "tree_row_height"];

impl Default for Theme {
    fn default() -> Self {
        Self {
            frame_bg: c(0x262729),
            main_bg: c(0x191A1C),
            panel_bg: c(0x191A1C),
            bg_mid: c(0x26282B),
            bg_light: c(0x323438),
            divider: c(0x26272B),
            titlebar_bg: c(0x262729), // = frame_bg (no visual change in dark)

            text: c(0xD1D3D9),
            secondary_text: c(0xD1D3D9),
            line_number: c(0x4B5059),

            caret: c(0xCED0D6),
            caret_row: c(0x1F2023),
            selected_bg: c(0x2E436E),

            primary: c(0x3574F0),
            primary_text: c(0xFFFFFF),

            status_added: c(0x73BD79),
            status_modified: c(0x70AEFF),
            status_deleted: c(0x6F737A),
            // Untracked = a new file; checking it on commit `git add`s it, so it reads as
            // "new" (green) like a staged addition rather than a scary red.
            status_untracked: c(0x73BD79),
            status_conflict: c(0xDE6A66),

            diff_inserted_bg: c(0x294436),
            diff_deleted_bg: c(0x484A4A),
            diff_modified_bg: c(0x385570),
            diff_separator_bg: c(0x2B2D30),
            // A deeper blue than the modified-line tint (#385570) so the exact changed
            // words read as emphasis — same on both sides.
            diff_word_old_bg: c(0x1A4269),
            diff_word_new_bg: c(0x1A4269),

            syn_keyword: c(0xCF8E6D),
            syn_string: c(0x6AAB73),
            syn_number: c(0x2AACB8),
            syn_comment: c(0x7A7E85),
            syn_function: c(0x56A8F5),
            syn_field: c(0xC77DBB),
            syn_constant: c(0xC77DBB),
            syn_identifier: c(0xD1D3D9),
            syn_operator: c(0xD1D3D9),

            editor_font_size: 14.0,
            ui_font_size: 13.0,
            tree_row_height: 26.0,
        }
    }
}

impl Theme {
    /// **Kyde Light** — light counterpart to the default dark palette. Surfaces per spec
    /// (`#F6F6F7` frame, white islands, `#1D1D1F` text); accents/status/syntax take their
    /// cues from the Hoxton Mix account-app palette (action blue `#1977EC`, brand purple
    /// `#A42F89`, success/danger, the `light*`/`dark*` greys). On-white-readable variants of
    /// each colour (darkened greens/reds, grey gutter) so contrast holds.
    pub fn light() -> Self {
        Self {
            // Surfaces
            frame_bg: c(0xF6F6F7),
            main_bg: c(0xFFFFFF),
            panel_bg: c(0xFFFFFF),
            bg_mid: c(0xEDEDEF),      // light300 — hover
            bg_light: c(0xE4E4E6),    // light400
            divider: c(0xDBDBDE),     // light500
            titlebar_bg: c(0xE4E4E6), // grey strip so the inactive traffic lights read

            // Text
            text: c(0x1D1D1F),           // dark500
            secondary_text: c(0x6E6E73), // text-muted
            line_number: c(0xA5A5A5),    // dark200 — gutter / inactive icons

            // Editor
            caret: c(0x1D1D1F),
            caret_row: c(0xF5F5F6),   // subtle current-line tint on white
            selected_bg: c(0xD1E4FB), // action100 — selected row

            // Buttons
            primary: c(0x1977EC), // action
            primary_text: c(0xFFFFFF),

            // Git status (darkened for contrast on white)
            status_added: c(0x21A848),
            status_modified: c(0x1977EC),
            status_deleted: c(0x777779), // dark300
            status_untracked: c(0x21A848),
            status_conflict: c(0xD70015), // danger900

            // Diff hunk backgrounds (faint tints on white)
            diff_inserted_bg: c(0xE3F6E9),
            diff_deleted_bg: c(0xECECEE),
            diff_modified_bg: c(0xE1ECFC),
            diff_separator_bg: c(0xF0F0F2),
            // Word-level emphasis — same both sides, a stronger blue (mirrors the dark theme).
            diff_word_old_bg: c(0xADCFF7),
            diff_word_new_bg: c(0xADCFF7),

            // Syntax (Hoxton-tinted, dark enough to read on white)
            syn_keyword: c(0xA42F89),  // brand purple
            syn_string: c(0x138A3E),   // dark green
            syn_number: c(0x1B7FA8),   // dark teal (info)
            syn_comment: c(0x8B8B8F),  // light800 grey
            syn_function: c(0x1559B8), // dark action blue
            syn_field: c(0xA42F89),
            syn_constant: c(0xA42F89),
            syn_identifier: c(0x1D1D1F),
            syn_operator: c(0x1D1D1F),

            // Sizes inherited from the dark default; callers preserve the user's live values.
            editor_font_size: 14.0,
            ui_font_size: 13.0,
            tree_row_height: 26.0,
        }
    }

    /// Built-in named palettes for the Settings theme picker. Colours only — callers apply a
    /// preset with [`apply_palette`], which preserves the user's font sizes / row height.
    pub fn presets() -> [(&'static str, Theme); 2] {
        [
            ("Kyde Dark", Theme::default()),
            ("Kyde Light", Theme::light()),
        ]
    }

    /// True if `self`'s colours match `other`'s (ignores font sizes / row height). Used to
    /// mark the active preset in Settings.
    pub fn same_palette(&self, other: &Theme) -> bool {
        let sizes_off = |t: &Theme| Theme {
            editor_font_size: 0.0,
            ui_font_size: 0.0,
            tree_row_height: 0.0,
            ..*t
        };
        serde_json::to_value(sizes_off(self)).ok() == serde_json::to_value(sizes_off(other)).ok()
    }
}

/// Switch to a named palette, preserving the user's font sizes / tree-row height. Applies live
/// (next `get()` sees it) and persists to `theme.json`, like [`update`].
pub fn apply_palette(palette: Theme) {
    update(|t| {
        let (ef, uf, rh) = (t.editor_font_size, t.ui_font_size, t.tree_row_height);
        *t = palette;
        t.editor_font_size = ef;
        t.ui_font_size = uf;
        t.tree_row_height = rh;
    });
}

fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        });
    base.join("kyde").join("theme.json")
}

fn valid_hex(v: &serde_json::Value) -> bool {
    v.as_str().is_some_and(|s| {
        let h = s.trim_start_matches('#');
        h.len() == 6 && u32::from_str_radix(h, 16).is_ok()
    })
}

/// A sane font-size number (px). Guards against garbage / absurd values in the config.
fn valid_size(v: &serde_json::Value) -> bool {
    v.as_f64().is_some_and(|n| (6.0..=40.0).contains(&n))
}

/// Pure merge: given the file contents (if any), return the theme and whether the file
/// needs rewriting. Only valid per-key overrides are kept (missing/invalid → default,
/// unknown keys → dropped), so editing one color never loses the rest. Side-effect-free
/// for testing.
fn merge(file: Option<&str>) -> (Theme, bool) {
    let default = Theme::default();
    let default_val = serde_json::to_value(default).expect("theme serializes");
    let mut obj = default_val.as_object().expect("theme is an object").clone();

    let mut repaired = true; // assume repair unless we read a clean, complete file
    if let Some(s) = file {
        if let Ok(serde_json::Value::Object(file)) = serde_json::from_str::<serde_json::Value>(s) {
            let mut clean = true;
            for (key, slot) in obj.iter_mut() {
                let numeric = NUMERIC_KEYS.contains(&key.as_str());
                let ok =
                    file.get(key).is_some_and(
                        |v| {
                            if numeric {
                                valid_size(v)
                            } else {
                                valid_hex(v)
                            }
                        },
                    );
                match (ok, file.get(key)) {
                    (true, Some(v)) => *slot = v.clone(),
                    _ => clean = false, // missing or invalid → keep default, mark repair
                }
            }
            if file.keys().any(|k| !obj.contains_key(k)) {
                clean = false; // unknown extra keys → tidy on rewrite
            }
            repaired = !clean;
        }
    }
    let theme = serde_json::from_value(serde_json::Value::Object(obj)).unwrap_or(default);
    (theme, repaired)
}

/// Load the theme, repairing the file as needed (missing file → write defaults; missing or
/// invalid keys → filled from defaults; unknown keys → dropped). Editing one color never
/// loses the rest.
fn load() -> Theme {
    let (theme, repaired) = merge(std::fs::read_to_string(config_path()).ok().as_deref());
    if repaired {
        theme.save();
    }
    theme
}

impl Theme {
    pub fn save(&self) {
        let path = config_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Live theme, behind an `RwLock` so settings can mutate it at runtime (font sizes, colours)
/// and every `get()` after sees the change — no app restart. `None` until first load.
static THEME: RwLock<Option<Theme>> = RwLock::new(None);

/// A snapshot of the loaded theme. Loads lazily on first call (and writes defaults if absent).
/// Returns by value (cheap — `Theme: Copy`), so `theme::get().primary` and
/// `let t = theme::get();` both work unchanged.
pub fn get() -> Theme {
    if let Some(t) = *THEME.read().unwrap() {
        return t;
    }
    let loaded = load();
    *THEME.write().unwrap() = Some(loaded);
    loaded
}

/// Mutate the live theme and persist it to `theme.json`. The change is visible to the next
/// `get()` immediately, so the UI reflects it on the next frame (no restart).
pub fn update(f: impl FnOnce(&mut Theme)) {
    let mut guard = THEME.write().unwrap();
    let mut t = (*guard).unwrap_or_else(load);
    f(&mut t);
    *guard = Some(t);
    t.save();
}

/// Apply a palette to the live theme **without persisting** to `theme.json` (preserving the
/// user's font sizes / row height, like [`apply_palette`]). Used by the screenshot harness: a
/// one-off light-theme shot must NOT leave a `theme.json` that taints the later dark shots,
/// which share its throwaway config dir and reload the theme on each launch.
pub fn set_palette_ephemeral(palette: Theme) {
    let mut guard = THEME.write().unwrap();
    let (ef, uf, rh) = (*guard)
        .map(|t| (t.editor_font_size, t.ui_font_size, t.tree_row_height))
        .unwrap_or_else(|| {
            let d = load();
            (d.editor_font_size, d.ui_font_size, d.tree_row_height)
        });
    let mut t = palette;
    t.editor_font_size = ef;
    t.ui_font_size = uf;
    t.tree_row_height = rh;
    *guard = Some(t);
}

/// Corner radius of the island panels (tree / editor), and the frame gap between them.
pub const ISLAND_RADIUS: f32 = 10.0;
pub const FRAME_GAP: f32 = 8.0;

/// Fonts (no colour — separate from the themeable palette). Both bundled + OFL-licensed,
/// registered at startup in `main::load_fonts`.
pub mod font {
    /// Code font: diff + editor. JetBrains Mono.
    pub const FAMILY: &str = "JetBrains Mono";
    /// UI chrome font: trees, buttons, labels, overlays. Inter.
    pub const UI_FAMILY: &str = "Inter";
    pub const LINE_HEIGHT: f32 = 1.2;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: gpui::Rgba, hex: u32) -> bool {
        let b = c(hex);
        (a.r - b.r).abs() < 0.01 && (a.g - b.g).abs() < 0.01 && (a.b - b.b).abs() < 0.01
    }

    #[test]
    fn missing_file_uses_defaults_and_repairs() {
        let (t, repaired) = merge(None);
        assert!(repaired);
        assert!(approx(t.primary, 0x3574F0));
    }

    #[test]
    fn partial_file_keeps_override_and_fills_rest() {
        let (t, repaired) = merge(Some(r##"{ "primary": "#FF0000" }"##));
        assert!(repaired); // missing keys → needs rewrite
        assert!(approx(t.primary, 0xFF0000)); // override kept
        assert!(approx(t.main_bg, 0x191A1C)); // default filled
    }

    #[test]
    fn invalid_color_falls_back_to_default() {
        let (t, repaired) = merge(Some(r##"{ "primary": "not-a-color" }"##));
        assert!(repaired);
        assert!(approx(t.primary, 0x3574F0));
    }

    #[test]
    fn complete_valid_file_is_not_repaired() {
        let full = serde_json::to_string(&Theme::default()).unwrap();
        let (_t, repaired) = merge(Some(&full));
        assert!(!repaired);
    }
}

/// serde adapter: Rgba <-> "#RRGGBB".
mod hex {
    use gpui::Rgba;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(c: &Rgba, s: S) -> Result<S::Ok, S::Error> {
        let to = |f: f32| (f.clamp(0.0, 1.0) * 255.0).round() as u8;
        s.serialize_str(&format!("#{:02X}{:02X}{:02X}", to(c.r), to(c.g), to(c.b)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Rgba, D::Error> {
        let s = String::deserialize(d)?;
        let h = s.trim_start_matches('#');
        let v = u32::from_str_radix(h, 16).map_err(serde::de::Error::custom)?;
        Ok(Rgba {
            r: ((v >> 16) & 0xff) as f32 / 255.0,
            g: ((v >> 8) & 0xff) as f32 / 255.0,
            b: (v & 0xff) as f32 / 255.0,
            a: 1.0,
        })
    }
}
