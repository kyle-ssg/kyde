#![deny(missing_docs)]
//! Runtime theme, loaded from `~/.config/kyde/theme.json` (hand-editable hex).
//! Defaults are an original hand-authored dark palette (Darcula-family style).
//! Access the loaded theme anywhere via `theme::get()`; it loads lazily on first use
//! and writes a default file if none exists.

// Colours are `kyde_color::Color` (a UI-framework-free POD). Aliased as `Rgba` so the field
// types + `c()` helper below read unchanged; a UI consumer that needs the renderer colour type
// gets `Color: Into<Rgba/Hsla/Fill>` from `kyde-color`'s optional feature (on in the binary +
// kyde-ui). This crate itself depends on no GUI framework.
use kyde_color::Color as Rgba;
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
    /// Editor island surface (the main content panel).
    #[serde(with = "hex")]
    pub main_bg: Rgba,
    /// Side-panel island surface (tree / sidebars).
    #[serde(with = "hex")]
    pub panel_bg: Rgba,
    /// Mid-tone surface (hover rows, key chips).
    #[serde(with = "hex")]
    pub bg_mid: Rgba,
    /// Lightest surface (active rail button, pressed states).
    #[serde(with = "hex")]
    pub bg_light: Rgba,
    /// General divider / hr / border colour.
    #[serde(with = "hex")]
    pub divider: Rgba,
    /// Native title strip behind the macOS traffic lights. Distinct from `frame_bg` so a
    /// light theme can give the (system-drawn) inactive traffic lights a grey backing —
    /// the app can't force the window's `NSAppearance`, so on a near-white strip they wash out.
    #[serde(with = "hex")]
    pub titlebar_bg: Rgba,

    // Text
    /// General text colour (everything except the primary button).
    #[serde(with = "hex")]
    pub text: Rgba,
    /// Secondary / less-prominent text.
    #[serde(with = "hex")]
    pub secondary_text: Rgba,
    /// Line numbers + other dim/tertiary text.
    #[serde(with = "hex")]
    pub line_number: Rgba,

    // Editor
    /// Text caret colour.
    #[serde(with = "hex")]
    pub caret: Rgba,
    /// Current-line background highlight in the editor.
    #[serde(with = "hex")]
    pub caret_row: Rgba,
    /// Selected sidebar/menu row background.
    #[serde(with = "hex")]
    pub selected_bg: Rgba,

    // Buttons
    /// Primary (filled) button background.
    #[serde(with = "hex")]
    pub primary: Rgba,
    /// Primary button text colour.
    #[serde(with = "hex")]
    pub primary_text: Rgba,

    // Git file status
    /// Added/new file status colour.
    #[serde(with = "hex")]
    pub status_added: Rgba,
    /// Modified file status colour.
    #[serde(with = "hex")]
    pub status_modified: Rgba,
    /// Deleted file status colour.
    #[serde(with = "hex")]
    pub status_deleted: Rgba,
    /// Untracked file status colour.
    #[serde(with = "hex")]
    pub status_untracked: Rgba,
    /// Merge-conflict file status colour.
    #[serde(with = "hex")]
    pub status_conflict: Rgba,

    // Diff hunk backgrounds
    /// Inserted-line background in the diff.
    #[serde(with = "hex")]
    pub diff_inserted_bg: Rgba,
    /// Deleted-line background in the diff.
    #[serde(with = "hex")]
    pub diff_deleted_bg: Rgba,
    /// Modified-line background in the diff.
    #[serde(with = "hex")]
    pub diff_modified_bg: Rgba,
    /// Unresolved-conflict background in the 3-pane merge view.
    #[serde(with = "hex")]
    pub diff_conflict_bg: Rgba,
    /// Background of the center-gutter separator column.
    #[serde(with = "hex")]
    pub diff_separator_bg: Rgba,
    /// Stronger word-level tint on the OLD side of a modified line (the changed words).
    #[serde(with = "hex")]
    pub diff_word_old_bg: Rgba,
    /// Stronger word-level tint on the NEW side of a modified line (the changed words).
    #[serde(with = "hex")]
    pub diff_word_new_bg: Rgba,

    // Syntax
    /// Syntax colour for keywords.
    #[serde(with = "hex")]
    pub syn_keyword: Rgba,
    /// Syntax colour for string literals.
    #[serde(with = "hex")]
    pub syn_string: Rgba,
    /// Syntax colour for numeric literals.
    #[serde(with = "hex")]
    pub syn_number: Rgba,
    /// Syntax colour for comments.
    #[serde(with = "hex")]
    pub syn_comment: Rgba,
    /// Syntax colour for function/method names.
    #[serde(with = "hex")]
    pub syn_function: Rgba,
    /// Syntax colour for fields / properties / attributes.
    #[serde(with = "hex")]
    pub syn_field: Rgba,
    /// Syntax colour for constants.
    #[serde(with = "hex")]
    pub syn_constant: Rgba,
    /// Syntax colour for plain identifiers.
    #[serde(with = "hex")]
    pub syn_identifier: Rgba,
    /// Syntax colour for operators + punctuation.
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

/// Accent overrides for a colour-vision-deficiency variant — the semantic colours that must
/// stay distinct. Applied over a base palette by [`apply_cvd`]; everything else is inherited.
/// `added` doubles as `untracked` (both read as "new"); diff word-emphasis uses `word_bg` on
/// both sides; the minor syntax roles fall back to neutral text.
struct Cvd {
    added: u32,
    modified: u32,
    deleted: u32,
    conflict: u32,
    ins_bg: u32,
    del_bg: u32,
    mod_bg: u32,
    conflict_bg: u32,
    word_bg: u32,
    keyword: u32,
    string: u32,
    number: u32,
    function: u32,
    comment: u32,
}

// Red-green safe (deut/prot) — blue/amber poles, 3rd category split by lightness.
const CVD_DARK_RG: Cvd = Cvd {
    added: 0x3B9EFF,
    modified: 0xE8B33A,
    deleted: 0x8A8F98,
    conflict: 0xB5651D,
    ins_bg: 0x16344E,
    del_bg: 0x33363B,
    mod_bg: 0x4A3A14,
    // Same amber family as `modified` (only two poles survive) — split by lightness.
    conflict_bg: 0x63470F,
    word_bg: 0x1F4E78,
    keyword: 0xE8B33A,
    string: 0x2E9AB8,
    number: 0xC98A2E,
    function: 0x7FB8FF,
    comment: 0x7A7E85,
};
const CVD_LIGHT_RG: Cvd = Cvd {
    added: 0x1A6FD4,
    modified: 0xB07A00,
    deleted: 0x777779,
    conflict: 0x7A4A10,
    ins_bg: 0xC6DCFA,
    del_bg: 0xDFDFE1,
    mod_bg: 0xF6EAC0,
    conflict_bg: 0xEDD79B, // deeper amber than mod_bg (lightness split)
    word_bg: 0xAECBF2,
    keyword: 0x9A6800,
    string: 0x0E6FA0,
    number: 0x6B5210,
    function: 0x1559B8,
    comment: 0x8B8B8F,
};
// Blue-yellow safe (tritan) — green/red poles, 3rd category split by lightness.
const CVD_DARK_TR: Cvd = Cvd {
    added: 0x4FB84F,
    modified: 0xFF8FB5,
    deleted: 0x8A8F98,
    conflict: 0xC81C1C,
    ins_bg: 0x1C3A1C,
    del_bg: 0x33363B,
    mod_bg: 0x4A2230,
    conflict_bg: 0x611A1A, // stronger red than mod_bg (lightness split)
    word_bg: 0x5A2A38,
    keyword: 0xFF8FB5,
    string: 0x5CB85C,
    number: 0xC8506A,
    function: 0x2E9A78,
    comment: 0x7A7E85,
};
const CVD_LIGHT_TR: Cvd = Cvd {
    added: 0x1E8A1E,
    modified: 0xD0407A,
    deleted: 0x777779,
    conflict: 0x8A1010,
    ins_bg: 0xD2EDD2,
    del_bg: 0xDFDFE1,
    mod_bg: 0xF7DCE6,
    conflict_bg: 0xF2C4C4, // stronger red than mod_bg (lightness split)
    word_bg: 0xF0C0D0,
    keyword: 0xC03060,
    string: 0x1E8A1E,
    number: 0x8A2040,
    function: 0x064A2A,
    comment: 0x8B8B8F,
};

/// Overlay CVD accents onto a base palette (see [`Theme::dark_redgreen`] for the rationale).
fn apply_cvd(base: Theme, a: Cvd) -> Theme {
    Theme {
        status_added: c(a.added),
        status_untracked: c(a.added),
        status_modified: c(a.modified),
        status_deleted: c(a.deleted),
        status_conflict: c(a.conflict),
        diff_inserted_bg: c(a.ins_bg),
        diff_deleted_bg: c(a.del_bg),
        diff_modified_bg: c(a.mod_bg),
        diff_conflict_bg: c(a.conflict_bg),
        diff_word_old_bg: c(a.word_bg),
        diff_word_new_bg: c(a.word_bg),
        syn_keyword: c(a.keyword),
        syn_string: c(a.string),
        syn_number: c(a.number),
        syn_function: c(a.function),
        syn_comment: c(a.comment),
        // Minor roles → neutral text (2 poles can't separate 6 chromatic syntax classes).
        syn_field: base.text,
        syn_constant: base.text,
        syn_identifier: base.text,
        syn_operator: base.text,
        ..base
    }
}

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
            // Muted red tuned to the same weight as the green/blue hunk tints, so an
            // unresolved merge conflict reads as "needs attention" without shouting.
            diff_conflict_bg: c(0x4D2F2E),
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
            diff_conflict_bg: c(0xFBE4E4), // faint red tint (mirrors the green/blue tints)
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

    /// **Colour-vision-deficiency variants.** Each takes the matching base palette (dark/light)
    /// and remaps only the *semantic* colours — git status, diff tints, the common syntax roles
    /// — so they stay perceptually distinct under the named deficiency; surfaces / text / caret /
    /// selection are inherited unchanged. Hexes were tuned against a Viénot CVD simulation to a
    /// ΔE target (status & diff pairs ≥ ~20, legibility ≥ 3:1), see `apply_cvd`.
    ///
    /// A CVD eye keeps only **two** chromatic poles (red-green loses red↔green → blue/amber
    /// survive; blue-yellow loses blue↔yellow → red/green survive). A 3rd same-family category
    /// (modified vs conflict, keyword vs function) is therefore separated by *lightness*, not
    /// hue. Six chromatic syntax roles can't all be split on two poles, so the minor tail
    /// (field/constant/identifier/operator) falls back to neutral text — code reads positionally,
    /// and forcing colour there would only reintroduce a clash.
    pub fn dark_redgreen() -> Self {
        // Red-green safe (deuteranopia / protanopia). Poles: blue / amber.
        apply_cvd(Theme::default(), CVD_DARK_RG)
    }
    /// Light palette tuned for red-green colour-vision deficiency.
    pub fn light_redgreen() -> Self {
        apply_cvd(Theme::light(), CVD_LIGHT_RG)
    }
    /// Dark palette tuned for blue-yellow (tritan) colour-vision deficiency.
    pub fn dark_tritan() -> Self {
        // Blue-yellow safe (tritanopia). Poles: green / red.
        apply_cvd(Theme::default(), CVD_DARK_TR)
    }
    /// Light palette tuned for blue-yellow (tritan) colour-vision deficiency.
    pub fn light_tritan() -> Self {
        apply_cvd(Theme::light(), CVD_LIGHT_TR)
    }

    /// Built-in named palettes for the Settings theme picker. Colours only — callers apply a
    /// preset with [`apply_palette`], which preserves the user's font sizes / row height.
    pub fn presets() -> [(&'static str, Theme); 6] {
        [
            ("Kyde Dark", Theme::default()),
            ("Kyde Light", Theme::light()),
            ("Red–Green Dark", Theme::dark_redgreen()),
            ("Red–Green Light", Theme::light_redgreen()),
            ("Blue–Yellow Dark", Theme::dark_tritan()),
            ("Blue–Yellow Light", Theme::light_tritan()),
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
    let base = std::env::var("XDG_CONFIG_HOME").map_or_else(
        |_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        },
        PathBuf::from,
    );
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
    // Serializing the default theme to a JSON object is infallible by construction (every field
    // is a plain number or hex string), but we don't `.expect()` it (rule 1): on the impossible
    // failure, fall back to the in-memory default and request a file rewrite.
    let Some(mut obj) = serde_json::to_value(default)
        .ok()
        .and_then(|v| v.as_object().cloned())
    else {
        return (default, true);
    };

    let mut repaired = true; // assume repair unless we read a clean, complete file
    if let Some(s) = file {
        if let Ok(serde_json::Value::Object(file)) = serde_json::from_str::<serde_json::Value>(s) {
            let mut clean = true;
            for (key, slot) in &mut obj {
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
    /// Serialize this theme to `~/.config/kyde/theme.json` (pretty hex JSON).
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

// Lock accessors that tolerate poisoning instead of `.unwrap()`-ing it. A poisoned `THEME`
// lock carries no broken invariant: `Theme` is `Copy` and every writer fully *replaces* the
// `Option<Theme>` in one assignment, so a panic mid-write cannot leave a torn value — the
// recovered inner guard is always a complete, valid state. We take it back rather than
// propagate the poison (which would cascade a single unrelated panic into every later read).
fn read_theme() -> std::sync::RwLockReadGuard<'static, Option<Theme>> {
    THEME
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn write_theme() -> std::sync::RwLockWriteGuard<'static, Option<Theme>> {
    THEME
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A snapshot of the loaded theme. Loads lazily on first call (and writes defaults if absent).
/// Returns by value (cheap — `Theme: Copy`), so `theme::get().primary` and
/// `let t = theme::get();` both work unchanged.
pub fn get() -> Theme {
    if let Some(t) = *read_theme() {
        return t;
    }
    let loaded = load();
    *write_theme() = Some(loaded);
    loaded
}

/// Mutate the live theme and persist it to `theme.json`. The change is visible to the next
/// `get()` immediately, so the UI reflects it on the next frame (no restart).
pub fn update(f: impl FnOnce(&mut Theme)) {
    let mut guard = write_theme();
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
    let mut guard = write_theme();
    let (ef, uf, rh) = (*guard).map_or_else(
        || {
            let d = load();
            (d.editor_font_size, d.ui_font_size, d.tree_row_height)
        },
        |t| (t.editor_font_size, t.ui_font_size, t.tree_row_height),
    );
    let mut t = palette;
    t.editor_font_size = ef;
    t.ui_font_size = uf;
    t.tree_row_height = rh;
    *guard = Some(t);
}

/// Corner radius (px) of the island panels (tree / editor).
pub const ISLAND_RADIUS: f32 = 10.0;
/// Gap (px) between the island panels and the window frame.
pub const FRAME_GAP: f32 = 8.0;

/// Fonts (no colour — separate from the themeable palette). Both bundled + OFL-licensed,
/// registered at startup in `main::load_fonts`.
pub mod font {
    /// Code font: diff + editor. `JetBrains` Mono.
    pub const FAMILY: &str = "JetBrains Mono";
    /// UI chrome font: trees, buttons, labels, overlays. Inter.
    pub const UI_FAMILY: &str = "Inter";
    /// Line-height multiplier applied to both font families.
    pub const LINE_HEIGHT: f32 = 1.2;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Rgba, hex: u32) -> bool {
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
        let (t, repaired) = merge(Some(r#"{ "primary": "not-a-color" }"#));
        assert!(repaired);
        assert!(approx(t.primary, 0x3574F0));
    }

    #[test]
    fn cvd_presets_keep_semantic_colours_distinct_and_neutral_tail() {
        let approx_eq = |a: Rgba, b: Rgba| {
            (a.r - b.r).abs() < 0.01 && (a.g - b.g).abs() < 0.01 && (a.b - b.b).abs() < 0.01
        };
        for t in [
            Theme::dark_redgreen(),
            Theme::light_redgreen(),
            Theme::dark_tritan(),
            Theme::light_tritan(),
        ] {
            // The three carrying colours must not collapse onto each other.
            assert!(!approx_eq(t.status_added, t.status_modified));
            assert!(!approx_eq(t.status_added, t.status_conflict));
            assert!(!approx_eq(t.status_modified, t.status_conflict));
            // Untracked reads as "new" — same as added.
            assert!(approx_eq(t.status_untracked, t.status_added));
            // Minor syntax roles fall back to neutral text (can't separate 6 on 2 poles).
            assert!(approx_eq(t.syn_field, t.text));
            assert!(approx_eq(t.syn_identifier, t.text));
            assert!(approx_eq(t.syn_operator, t.text));
        }
    }

    // ---- Colour-vision-deficiency simulation (Viénot et al. 1999) -------------------------
    // Guards the design intent of the CVD presets: the *semantic* colours must stay
    // perceptually separated (ΔE) for the deficiency each preset targets, and stay legible on
    // their surface. Without this, an innocent hex edit could compile, pass the cheap
    // RGB-inequality test, and silently destroy CVD-safety. Methodology (reproducible):
    //   sRGB → linear → LMS (Hunt-Pointer-Estevez `RGB2LMS`) → project onto the dichromat
    //   plane (`S_*`) → back to linear RGB → CIE-Lab → ΔE76 between the two simulated colours.
    // The matrices and the per-category ΔE floors come from the tuning that produced the
    // palettes (true minimums measured at status 26.8 / syntax 16.1 / diff 15.0 / contrast
    // 3.02; floors set below those with slack so CI isn't flaky but real regressions trip it).
    type Mat = [[f64; 3]; 3];
    const RGB2LMS: [[f64; 3]; 3] = [
        [17.8824, 43.5161, 4.11935],
        [3.45565, 27.1554, 3.86714],
        [0.0299566, 0.184309, 1.46709],
    ];
    const LMS2RGB: [[f64; 3]; 3] = [
        [8.0944447905e-02, -1.3050440916e-01, 1.1672106644e-01],
        [-1.0248533515e-02, 5.4019326636e-02, -1.1361470821e-01],
        [-3.6529693786e-04, -4.1216146859e-03, 6.9351140486e-01],
    ];
    // Dichromat projection matrices (applied in LMS space).
    const S_DEUT: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.494207, 0.0, 1.24827], [0.0, 0.0, 1.0]];
    const S_PROT: [[f64; 3]; 3] = [[0.0, 2.02344, -2.52581], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    const S_TRIT: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [-0.395913, 0.801109, 0.0]];

    fn matvec(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
    }
    fn srgb_to_lin(c: f64) -> f64 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    fn lin_rgb(c: Rgba) -> [f64; 3] {
        [
            srgb_to_lin(f64::from(c.r)),
            srgb_to_lin(f64::from(c.g)),
            srgb_to_lin(f64::from(c.b)),
        ]
    }
    /// Relative luminance → WCAG contrast ratio between two colours.
    fn contrast(a: Rgba, b: Rgba) -> f64 {
        let l = |c: Rgba| {
            let v = lin_rgb(c);
            0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2]
        };
        let (la, lb) = (l(a), l(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }
    /// Simulate `c` as seen with the dichromacy `s`; returns linear RGB.
    fn simulate(c: Rgba, s: &[[f64; 3]; 3]) -> [f64; 3] {
        let lms = matvec(&RGB2LMS, lin_rgb(c));
        matvec(&LMS2RGB, matvec(s, lms)).map(|x| x.clamp(0.0, 1.0))
    }
    fn lin_to_lab(rgb: [f64; 3]) -> [f64; 3] {
        // linear sRGB → XYZ (D65) → Lab.
        const X: [[f64; 3]; 3] = [
            [0.4124, 0.3576, 0.1805],
            [0.2126, 0.7152, 0.0722],
            [0.0193, 0.1192, 0.9505],
        ];
        let xyz = matvec(&X, rgb);
        let wp = [0.95047, 1.0, 1.08883];
        let f = |t: f64| {
            if t > 0.008856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        let (fx, fy, fz) = (f(xyz[0] / wp[0]), f(xyz[1] / wp[1]), f(xyz[2] / wp[2]));
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    }
    /// ΔE76 between two colours as seen with dichromacy `s`.
    fn delta_e(a: Rgba, b: Rgba, s: &[[f64; 3]; 3]) -> f64 {
        let (la, lb) = (lin_to_lab(simulate(a, s)), lin_to_lab(simulate(b, s)));
        ((la[0] - lb[0]).powi(2) + (la[1] - lb[1]).powi(2) + (la[2] - lb[2]).powi(2)).sqrt()
    }

    #[test]
    fn cvd_presets_stay_perceptually_separated_under_their_deficiency() {
        // (preset, name, the dichromacies it must survive).
        let cases: [(Theme, &str, &[&Mat]); 4] = [
            (Theme::dark_redgreen(), "dark_redgreen", &[&S_DEUT, &S_PROT]),
            (
                Theme::light_redgreen(),
                "light_redgreen",
                &[&S_DEUT, &S_PROT],
            ),
            (Theme::dark_tritan(), "dark_tritan", &[&S_TRIT]),
            (Theme::light_tritan(), "light_tritan", &[&S_TRIT]),
        ];
        for (t, name, mats) in cases {
            // Worst ΔE across the relevant deficiencies for a pair.
            let worst = |a: Rgba, b: Rgba| {
                mats.iter()
                    .map(|s| delta_e(a, b, s))
                    .fold(f64::INFINITY, f64::min)
            };
            let check = |a: Rgba, b: Rgba, floor: f64, pair: &str| {
                let d = worst(a, b);
                assert!(d >= floor, "{name}: {pair} ΔE={d:.1} < {floor}");
            };

            // Git status — the carrying colours, mutually distinct (floor 22; min measured 26.8).
            let st = [
                ("added/modified", t.status_added, t.status_modified),
                ("added/conflict", t.status_added, t.status_conflict),
                ("added/deleted", t.status_added, t.status_deleted),
                ("modified/conflict", t.status_modified, t.status_conflict),
                ("modified/deleted", t.status_modified, t.status_deleted),
                ("conflict/deleted", t.status_conflict, t.status_deleted),
            ];
            for (p, a, b) in st {
                check(a, b, 22.0, p);
            }
            // Common syntax roles (floor 13; min measured 16.1).
            let syn = [
                ("string/keyword", t.syn_string, t.syn_keyword),
                ("string/function", t.syn_string, t.syn_function),
                ("string/number", t.syn_string, t.syn_number),
                ("keyword/function", t.syn_keyword, t.syn_function),
                ("keyword/number", t.syn_keyword, t.syn_number),
                ("number/function", t.syn_number, t.syn_function),
            ];
            for (p, a, b) in syn {
                check(a, b, 13.0, p);
            }
            // Diff hunk tints (floor 12; min measured 15.0).
            let df = [
                ("ins/del", t.diff_inserted_bg, t.diff_deleted_bg),
                ("ins/mod", t.diff_inserted_bg, t.diff_modified_bg),
                ("del/mod", t.diff_deleted_bg, t.diff_modified_bg),
            ];
            for (p, a, b) in df {
                check(a, b, 12.0, p);
            }
            // Legibility: every carrying colour readable on the editor surface (floor 2.8;
            // min measured 3.02 — clears WCAG 3:1 for large/bold UI text).
            for (label, col) in [
                ("status_added", t.status_added),
                ("status_modified", t.status_modified),
                ("status_conflict", t.status_conflict),
                ("syn_string", t.syn_string),
                ("syn_keyword", t.syn_keyword),
                ("syn_number", t.syn_number),
                ("syn_function", t.syn_function),
            ] {
                let cr = contrast(col, t.main_bg);
                assert!(cr >= 2.8, "{name}: {label} contrast={cr:.2} < 2.8");
            }
        }
    }

    #[test]
    fn cvd_simulation_matches_known_confusions() {
        // Sanity-check the simulator against the *default* palette's known weak pairs (the ones
        // that motivated the presets): they must read as low-ΔE under the deficiency that hurts
        // them, proving the metric actually detects CVD collisions (not just always-passing).
        let d = Theme::default();
        // Green added vs red conflict collapses for deuteranopes.
        assert!(delta_e(d.status_added, d.status_conflict, &S_DEUT) < 12.0);
        // Green string vs orange keyword collapses for protanopes.
        assert!(delta_e(d.syn_string, d.syn_keyword, &S_PROT) < 12.0);
    }

    #[test]
    fn presets_are_six_distinct_palettes() {
        let ps = Theme::presets();
        assert_eq!(ps.len(), 6);
        for i in 0..ps.len() {
            for j in (i + 1)..ps.len() {
                assert!(
                    !ps[i].1.same_palette(&ps[j].1),
                    "presets {} and {} share a palette",
                    ps[i].0,
                    ps[j].0
                );
            }
        }
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
    use kyde_color::Color as Rgba;
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
