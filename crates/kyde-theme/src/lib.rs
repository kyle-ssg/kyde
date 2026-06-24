//! Runtime theme, loaded from `~/.config/kyde/theme.json` (hand-editable hex).
//! Defaults are an original hand-authored dark palette (Darcula-family style).
//! Access the loaded theme anywhere via `theme::get()`; it loads lazily on first use
//! and writes a default file if none exists.

use gpui::Rgba;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;

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
#[derive(Clone, Serialize, Deserialize)]
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
}

/// Theme keys that are plain numbers, not `#RRGGBB` colours (so `merge` validates them as
/// numbers rather than hex).
const NUMERIC_KEYS: &[&str] = &["editor_font_size", "ui_font_size"];

impl Default for Theme {
    fn default() -> Self {
        Self {
            frame_bg: c(0x262729),
            main_bg: c(0x191A1C),
            panel_bg: c(0x191A1C),
            bg_mid: c(0x26282B),
            bg_light: c(0x323438),
            divider: c(0x26272B),

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
        }
    }
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
    let default_val = serde_json::to_value(&default).expect("theme serializes");
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

static THEME: OnceLock<Theme> = OnceLock::new();

/// The loaded theme. Loads lazily on first call (and writes defaults if absent).
pub fn get() -> &'static Theme {
    THEME.get_or_init(load)
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
