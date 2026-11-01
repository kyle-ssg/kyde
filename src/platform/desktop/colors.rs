//! The desktop's colours — its dark preference, accent, and the GTK 3 theme's or KDE colour
//! scheme's window / view / selection colours — mapped onto a Kyde palette.

use super::{Desktop, Probe};
use crate::theme::Theme;
use kyde_color::Color;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// What the desktop says about colour; `None` for anything it doesn't say (or Kyde can't read).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Colors {
    /// Window background + text (GTK `theme_bg_color` / `theme_fg_color`).
    window_bg: Option<Color>,
    window_fg: Option<Color>,
    /// Content (lists, text views) background + text (GTK `theme_base_color` /
    /// `theme_text_color`).
    view_bg: Option<Color>,
    view_fg: Option<Color>,
    /// Selected rows (GTK `theme_selected_bg_color` / `theme_selected_fg_color`).
    selected_bg: Option<Color>,
    selected_fg: Option<Color>,
    /// The desktop's accent colour.
    accent: Option<Color>,
    /// The desktop prefers dark (`Some(false)`: it said light).
    dark: Option<bool>,
}

impl Colors {
    /// The content text and background (the theme's view colours, else its window's): what a
    /// terminal set to use the theme's colours draws with.
    pub(super) fn content(&self) -> (Option<Color>, Option<Color>) {
        (
            self.view_fg.or(self.window_fg),
            self.view_bg.or(self.window_bg),
        )
    }

    /// Whether the desktop prefers dark (`None`: it didn't say).
    pub(super) fn dark(&self) -> Option<bool> {
        self.dark
    }
}

/// Read the desktop's colours: KDE's colour scheme, else the GTK 3 theme.
pub(super) fn read(p: &Probe) -> Colors {
    if p.desktop == Desktop::Kde {
        return kde(p);
    }
    let theme = match p.desktop {
        Desktop::Xfce => p.xfconf("xsettings", "/Net/ThemeName"),
        Desktop::Other => p.gtk_setting("gtk-theme-name"),
        d => d
            .interface_schema()
            .and_then(|s| p.gsettings(s, "gtk-theme")),
    };
    let dark = prefers_dark(p, theme.as_deref());
    let defs = theme
        .and_then(|name| theme_css(p, &name, dark == Some(true)))
        .map(|css| Defs::read(&css))
        .unwrap_or_default();
    Colors {
        window_bg: defs.color("theme_bg_color"),
        window_fg: defs.color("theme_fg_color"),
        view_bg: defs.color("theme_base_color"),
        view_fg: defs.color("theme_text_color"),
        selected_bg: defs.color("theme_selected_bg_color"),
        selected_fg: defs.color("theme_selected_fg_color"),
        accent: (p.desktop == Desktop::Gnome)
            .then(|| p.gsettings("org.gnome.desktop.interface", "accent-color"))
            .flatten()
            .and_then(|a| gnome_accent(&a)),
        dark,
    }
}

/// The dark preference on a GTK desktop: its colour-scheme setting (GNOME's, or Cinnamon's
/// own), GTK's `gtk-application-prefer-dark-theme`, or a `-dark` theme name. `Some(false)` when
/// it said anything else; `None` when none of them could be read.
fn prefers_dark(p: &Probe, theme: Option<&str>) -> Option<bool> {
    let scheme = match p.desktop {
        Desktop::Gnome => p.gsettings("org.gnome.desktop.interface", "color-scheme"),
        Desktop::Cinnamon => p.gsettings("org.x.apps.portal", "color-scheme"),
        _ => None,
    };
    let gtk = p.gtk_setting("gtk-application-prefer-dark-theme");
    let dark_name = |n: &str| {
        n.split(['-', '_', ' '])
            .any(|w| w.eq_ignore_ascii_case("dark"))
    };
    if scheme.as_deref() == Some("prefer-dark")
        || gtk
            .as_deref()
            .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        || theme.is_some_and(dark_name)
    {
        Some(true)
    } else {
        (scheme.is_some() || gtk.is_some() || theme.is_some()).then_some(false)
    }
}

/// The stylesheet GTK 3 loads for theme `name`: `gtk-dark.css` when dark is preferred and the
/// theme has one, else `gtk.css`, from the first theme folder that has it.
fn theme_css(p: &Probe, name: &str, dark: bool) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    let dirs = [p.home.join(".themes"), p.data_home.join("themes")]
        .into_iter()
        .chain(p.data_dirs.iter().map(|d| d.join("themes")));
    let files: &[&str] = if dark {
        &["gtk-dark.css", "gtk.css"]
    } else {
        &["gtk.css"]
    };
    dirs.map(|d| d.join(name).join("gtk-3.0"))
        .find_map(|d| files.iter().map(|f| d.join(f)).find(|f| f.is_file()))
}

/// The `@define-color`s of a GTK 3 stylesheet and the files it `@import`s, by name (a later
/// definition replaces an earlier one, as in GTK), unresolved.
#[derive(Default)]
struct Defs(HashMap<String, String>);

impl Defs {
    fn read(css: &Path) -> Self {
        let mut defs = Self::default();
        defs.load(css, &mut HashSet::new());
        defs
    }

    fn load(&mut self, css: &Path, seen: &mut HashSet<PathBuf>) {
        // Imports nest only so deep in real themes; a loop stops at a file already read.
        let key = std::fs::canonicalize(css).unwrap_or_else(|_| css.to_path_buf());
        if seen.len() > 64 || !seen.insert(key) {
            return;
        }
        let Ok(text) = std::fs::read_to_string(css) else {
            return;
        };
        let dir = css.parent().unwrap_or(Path::new("/"));
        for chunk in strip_comments(&text).split(';') {
            // Only top-level statements: the text after the last rule brace.
            let statement = chunk.rsplit(['{', '}']).next().unwrap_or("").trim();
            if let Some(rest) = statement.strip_prefix("@define-color") {
                if let Some((name, value)) = rest.trim().split_once(char::is_whitespace) {
                    self.0.insert(name.to_string(), value.trim().to_string());
                }
            } else if let Some(rest) = statement.strip_prefix("@import") {
                if let Some(target) = import_target(rest) {
                    self.load(&dir.join(target), seen);
                }
            }
        }
    }

    /// Colour `name`, following `@name` references; `None` when it's undefined, a loop, or
    /// computed (`shade()`, `mix()`, `alpha()` …), which Kyde doesn't evaluate.
    fn color(&self, name: &str) -> Option<Color> {
        let mut value = self.0.get(name)?;
        for _ in 0..32 {
            match value.strip_prefix('@') {
                Some(next) => value = self.0.get(next.trim())?,
                None => return parse_color(value),
            }
        }
        None
    }
}

/// `text` with its `/* … */` comments removed.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = rest[start + 2..]
            .find("*/")
            .map_or("", |end| &rest[start + 2 + end + 2..]);
    }
    out.push_str(rest);
    out
}

/// The file an `@import` names (`url("x.css")`, `url(x.css)` or `"x.css"`); `None` for a
/// `resource://` or other URL a theme can't be read from.
fn import_target(rest: &str) -> Option<&str> {
    let rest = rest.trim();
    let inner = match rest.strip_prefix("url(") {
        Some(url) => url.split(')').next()?.trim(),
        None => rest,
    };
    let target = match inner.chars().next()? {
        q @ ('"' | '\'') => inner[1..].split(q).next()?,
        _ => inner.split_whitespace().next()?,
    };
    let target = target.strip_prefix("file://").unwrap_or(target);
    (!target.is_empty() && !target.contains("://")).then_some(target)
}

/// A CSS colour: `#rgb`, `#rrggbb`, `#rrrgggbbb`, `#rrrrggggbbbb` (as GDK writes 16-bit
/// colours), opaque `rgb()` / `rgba()`, or a basic colour name. Anything else (a translucent
/// colour, a GTK colour function) is `None`.
pub(super) fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let digits = match hex.len() {
            3 => hex.chars().flat_map(|c| [c, c]).collect(),
            6 => hex.to_string(),
            // Each channel's digits, scaled to 0–1 by their own width.
            9 | 12 => {
                let width = hex.len() / 3;
                let max = f32::from(u16::MAX >> (16 - 4 * width));
                let chan = |i: usize| -> Option<f32> {
                    let v = u16::from_str_radix(&hex[i * width..(i + 1) * width], 16).ok()?;
                    Some(f32::from(v) / max)
                };
                return Some(Color {
                    r: chan(0)?,
                    g: chan(1)?,
                    b: chan(2)?,
                    a: 1.0,
                });
            }
            _ => return None,
        };
        return u32::from_str_radix(&digits, 16).ok().map(rgb_hex);
    }
    let args = s
        .strip_prefix("rgba(")
        .or_else(|| s.strip_prefix("rgb("))
        .and_then(|a| a.strip_suffix(')'));
    if let Some(args) = args {
        let parts: Vec<&str> = args.split(',').map(str::trim).collect();
        let channel = |v: &str| -> Option<f32> {
            let n = match v.strip_suffix('%') {
                Some(pc) => pc.parse::<f32>().ok()? / 100.0,
                None => v.parse::<f32>().ok()? / 255.0,
            };
            Some(n.clamp(0.0, 1.0))
        };
        let opaque = match parts.get(3) {
            None => true,
            Some(a) => match a.strip_suffix('%') {
                Some(pc) => pc.parse::<f32>().ok()? >= 100.0,
                None => a.parse::<f32>().ok()? >= 1.0,
            },
        };
        if !(3..=4).contains(&parts.len()) || !opaque {
            return None;
        }
        return Some(Color {
            r: channel(parts[0])?,
            g: channel(parts[1])?,
            b: channel(parts[2])?,
            a: 1.0,
        });
    }
    let hex = match s.to_ascii_lowercase().as_str() {
        "black" => 0x000000,
        "white" => 0xFFFFFF,
        "red" => 0xFF0000,
        "green" => 0x008000,
        "blue" => 0x0000FF,
        "yellow" => 0xFFFF00,
        "cyan" | "aqua" => 0x00FFFF,
        "magenta" | "fuchsia" => 0xFF00FF,
        "gray" | "grey" => 0x808080,
        "silver" => 0xC0C0C0,
        "maroon" => 0x800000,
        "navy" => 0x000080,
        "olive" => 0x808000,
        "purple" => 0x800080,
        "teal" => 0x008080,
        "lime" => 0x00FF00,
        "orange" => 0xFFA500,
        _ => return None,
    };
    Some(rgb_hex(hex))
}

/// A colour as terminals write one: what [`parse_color`] reads, quoted or not, plus Alacritty's
/// `0xRRGGBB`, and a translucent `rgba()` taken as its opaque colour (Kyde doesn't draw a
/// terminal see-through).
pub(super) fn parse_term_color(s: &str) -> Option<Color> {
    let s = super::unquote(s).trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return parse_color(&format!("#{hex}"));
    }
    parse_color(s).or_else(|| {
        let args = s.strip_prefix("rgba(")?.strip_suffix(')')?;
        let parts: Vec<&str> = args.split(',').collect();
        let [r, g, b, _] = parts.as_slice() else {
            return None;
        };
        parse_color(&format!("rgb({r},{g},{b})"))
    })
}

/// A KDE colour: `r,g,b` (or `r,g,b,a`) in 0–255, or a CSS colour.
pub(super) fn kde_color(value: &str) -> Option<Color> {
    let channels: Option<Vec<f32>> = value
        .split(',')
        .map(|c| {
            c.trim()
                .parse::<f32>()
                .ok()
                .map(|n| n.clamp(0.0, 255.0) / 255.0)
        })
        .collect();
    match channels.as_deref() {
        Some(&[red, green, blue] | &[red, green, blue, _]) => Some(Color {
            r: red,
            g: green,
            b: blue,
            a: 1.0,
        }),
        _ => parse_color(value),
    }
}

fn rgb_hex(hex: u32) -> Color {
    let chan = |shift: u32| ((hex >> shift) & 0xFF) as f32 / 255.0;
    Color {
        r: chan(16),
        g: chan(8),
        b: chan(0),
        a: 1.0,
    }
}

/// GNOME's accent colour names (GNOME 47+) as the colours GNOME draws them.
fn gnome_accent(name: &str) -> Option<Color> {
    let hex = match name {
        "blue" => 0x3584E4,
        "teal" => 0x2190A4,
        "green" => 0x3A944A,
        "yellow" => 0xC88800,
        "orange" => 0xED5B00,
        "red" => 0xE62D42,
        "pink" => 0xD56199,
        "purple" => 0x9141AC,
        "slate" => 0x6F8396,
        _ => return None,
    };
    Some(rgb_hex(hex))
}

/// KDE: the colour scheme in `kdeglobals` (`[Colors:Window]`, `[Colors:View]`,
/// `[Colors:Selection]`), the accent, and the scheme's name for its lightness when the colours
/// themselves aren't written.
fn kde(p: &Probe) -> Colors {
    let color = |section: &str, key: &str| kde_color(&p.kde_setting(section, key)?);
    Colors {
        window_bg: color("Colors:Window", "BackgroundNormal"),
        window_fg: color("Colors:Window", "ForegroundNormal"),
        view_bg: color("Colors:View", "BackgroundNormal"),
        view_fg: color("Colors:View", "ForegroundNormal"),
        selected_bg: color("Colors:Selection", "BackgroundNormal"),
        selected_fg: color("Colors:Selection", "ForegroundNormal"),
        accent: color("General", "AccentColor"),
        dark: p
            .kde_setting("General", "ColorScheme")
            .map(|name| name.to_lowercase().contains("dark")),
    }
}

/// WCAG relative luminance.
fn luminance(c: Color) -> f32 {
    let lin = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG contrast ratio between two colours (1–21).
fn contrast(a: Color, b: Color) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// A dark surface: one white text reads better on than black text does.
fn is_dark(c: Color) -> bool {
    luminance(c) < 0.18
}

/// `a` moved `t` of the way to `b`.
fn mix(a: Color, b: Color, t: f32) -> Color {
    let m = |x: f32, y: f32| x + (y - x) * t;
    Color {
        r: m(a.r, b.r),
        g: m(a.g, b.g),
        b: m(a.b, b.b),
        a: 1.0,
    }
}

/// Text for a filled button of colour `bg`: `preferred` (the theme's own) when it reads,
/// else white when that reads at the 3:1 UI-text floor, else black.
fn text_on(bg: Color, preferred: Option<Color>) -> Color {
    let (white, black) = (rgb_hex(0xFFFFFF), rgb_hex(0x000000));
    preferred
        .filter(|f| contrast(*f, bg) >= 3.0)
        .unwrap_or(if contrast(white, bg) >= 3.0 {
            white
        } else {
            black
        })
}

/// Minimum contrast for body text on its background (WCAG AA).
const TEXT_CONTRAST: f32 = 4.5;

/// Minimum contrast for icons and other non-text parts on their background (WCAG 1.4.11).
const ICON_CONTRAST: f32 = 3.0;

/// The desktop's surfaces + text, when they read: text at [`TEXT_CONTRAST`] on both the
/// content and the window background.
struct Surfaces {
    frame: Color,
    main: Color,
    text: Color,
}

impl Surfaces {
    fn of(c: &Colors) -> Option<Self> {
        let main = c.view_bg.or(c.window_bg)?;
        let frame = c.window_bg.unwrap_or(main);
        let text = c.view_fg.or(c.window_fg)?;
        (contrast(text, main) >= TEXT_CONTRAST && contrast(text, frame) >= TEXT_CONTRAST)
            .then_some(Self { frame, main, text })
    }
}

/// Kyde's palette from the desktop's colours. The base is Kyde Dark or Kyde Light — by the
/// theme's own background when its colours read, else by the dark preference — so syntax and
/// diff colours (which stay the base's) suit the background. Over it: the window / content
/// backgrounds and text, Kyde's greys and dividers re-derived between them, the selection,
/// and the accent as the primary colour. Colours that don't read at [`TEXT_CONTRAST`] are
/// dropped, keeping the base. `None` when the desktop said nothing about colour at all, so the
/// user's own palette stays.
pub(super) fn palette(c: &Colors) -> Option<Theme> {
    let surfaces = Surfaces::of(c);
    let dark = surfaces.as_ref().map(|s| is_dark(s.main)).or(c.dark);
    if dark.is_none() && c.accent.is_none() {
        return None;
    }
    let dark = dark.unwrap_or(true);
    let mut t = if dark {
        Theme::default()
    } else {
        Theme::light()
    };
    if let Some(s) = &surfaces {
        let grey = |k: f32| mix(s.main, s.text, k);
        t.frame_bg = s.frame;
        t.titlebar_bg = s.frame;
        t.main_bg = s.main;
        t.panel_bg = s.main;
        t.text = s.text;
        t.caret = s.text;
        let secondary = grey(0.7);
        t.secondary_text = if contrast(secondary, s.main) >= TEXT_CONTRAST
            && contrast(secondary, s.frame) >= TEXT_CONTRAST
        {
            secondary
        } else {
            s.text
        };
        // Inactive icons and the gutter: the left rail draws them on the window background,
        // the editor on the content's. Where those differ (a grey window round white or
        // black fields) the plain mix can vanish on one of them, so it moves on toward the
        // text until it reads at ICON_CONTRAST on both.
        t.line_number = (35u8..=100)
            .map(|k| grey(f32::from(k) / 100.0))
            .find(|c| {
                contrast(*c, s.main) >= ICON_CONTRAST && contrast(*c, s.frame) >= ICON_CONTRAST
            })
            .unwrap_or(s.text);
        t.caret_row = grey(0.04);
        t.bg_mid = grey(0.08);
        t.divider = grey(0.12);
        t.bg_light = grey(0.13);
        t.diff_separator_bg = grey(0.07);
        // Rows draw `text` on the selection, so it's a tint of the theme's selection colour.
        if let Some(sel) = c.selected_bg {
            let tint = mix(s.main, sel, if dark { 0.4 } else { 0.2 });
            if contrast(s.text, tint) >= TEXT_CONTRAST {
                t.selected_bg = tint;
            }
        }
    }
    // The accent, else the theme's selection colour (only when the theme's colours read).
    let theme_sel = surfaces.and(c.selected_bg);
    if let Some(primary) = c.accent.or(theme_sel) {
        t.primary = primary;
        t.primary_text = text_on(
            primary,
            c.accent.is_none().then_some(c.selected_fg).flatten(),
        );
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{probe, put, workdir};
    use super::*;

    fn hex(c: Color) -> u32 {
        let b = |f: f32| (f * 255.0).round() as u32;
        (b(c.r) << 16) | (b(c.g) << 8) | b(c.b)
    }

    #[test]
    fn css_colours_in_the_forms_gtk_themes_write() {
        let p = |s: &str| parse_color(s).map(hex);
        assert_eq!(p("#abc"), Some(0xAABBCC));
        assert_eq!(p("#3584E4"), Some(0x3584E4));
        assert_eq!(p("rgb(53, 132, 228)"), Some(0x3584E4));
        assert_eq!(p("rgba(255,255,255,1)"), Some(0xFFFFFF));
        assert_eq!(p("rgb(100%, 50%, 0%)"), Some(0xFF8000));
        assert_eq!(p("Navy"), Some(0x000080));
        assert_eq!(p("white"), Some(0xFFFFFF));
        // Translucent, computed, or unknown: unresolved.
        assert_eq!(p("rgba(0, 0, 0, 0.5)"), None);
        assert_eq!(p("shade(#ffffff, 0.9)"), None);
        assert_eq!(p("mix(@a, @b, 0.5)"), None);
        assert_eq!(p("alpha(black, 0.8)"), None);
        assert_eq!(p("transparent"), None);
        assert_eq!(p("#12345"), None);
        assert_eq!(p("rgb(1, 2)"), None);
        // GDK's 16-bit (and 12-bit) hex, as MATE Terminal and older gnome-terminals store.
        assert_eq!(p("#FFFF80800000"), Some(0xFF8000));
        assert_eq!(p("#AAAA00000000"), Some(0xAA0000));
        assert_eq!(p("#fff800000"), Some(0xFF8000));
        assert_eq!(p("#+12345"), None);
        assert_eq!(p("#ÿÿÿÿ"), None);
    }

    #[test]
    fn terminal_colours_in_the_forms_terminals_write() {
        let p = |s: &str| parse_term_color(s).map(hex);
        assert_eq!(p("#C0C0C0"), Some(0xC0C0C0));
        assert_eq!(p("'#C0C0C0'"), Some(0xC0C0C0));
        assert_eq!(p("\"#1d1f21\""), Some(0x1D1F21));
        assert_eq!(p("0x1d1f21"), Some(0x1D1F21));
        assert_eq!(p("#c0c0c0c0c0c0"), Some(0xC0C0C0));
        assert_eq!(p("rgb(192,192,192)"), Some(0xC0C0C0));
        assert_eq!(p("rgba(0,0,0,1)"), Some(0x000000));
        // A see-through terminal background: its colour, drawn opaque.
        assert_eq!(p("rgba(0, 0, 128, 0.85)"), Some(0x000080));
        // Not a colour: Kyde's own.
        assert_eq!(p("CellBackground"), None);
        assert_eq!(p(""), None);
        assert_eq!(p("rgba(0,0,0)x"), None);
        assert_eq!(
            kde_color("35,38,39").map(hex),
            Some(0x232627),
            "a Konsole scheme's r,g,b"
        );
    }

    #[test]
    fn define_colors_resolve_through_references_and_imports() {
        let dir = workdir();
        put(
            &dir,
            "theme/gtk-3.0/gtk.css",
            "/* @define-color theme_bg_color #ff0000; */\n\
             @import url(\"resource:///org/gnome/theme/gtk.css\");\n\
             @import url(\"colors.css\");\n\
             @define-color theme_text_color @text_color;\n\
             @define-color theme_selected_bg_color shade(@bg_color, 0.8);\n\
             @define-color loop_a @loop_b;\n@define-color loop_b @loop_a;\n\
             window { background-color: @theme_bg_color; color: @fg; }\n\
             @define-color theme_fg_color #000;\n",
        );
        put(
            &dir,
            "theme/gtk-3.0/colors.css",
            "@define-color bg_color #c0c0c0;\n@define-color text_color @black;\n\
             @define-color black rgb(0, 0, 0);\n@define-color theme_bg_color @bg_color;\n\
             @define-color theme_fg_color #fff;\n",
        );
        let defs = Defs::read(&dir.join("theme/gtk-3.0/gtk.css"));
        let c = |n: &str| defs.color(n).map(hex);
        // Through the import, and a reference to a reference.
        assert_eq!(c("theme_bg_color"), Some(0xC0C0C0));
        assert_eq!(c("theme_text_color"), Some(0x000000));
        // A later definition replaces the imported one.
        assert_eq!(c("theme_fg_color"), Some(0x000000));
        // Computed, looping, or undefined: unresolved.
        assert_eq!(c("theme_selected_bg_color"), None);
        assert_eq!(c("loop_a"), None);
        assert_eq!(c("theme_base_color"), None);
        // No file at all.
        assert!(Defs::read(&dir.join("none.css"))
            .color("theme_bg_color")
            .is_none());
    }

    #[test]
    fn the_dark_preference_and_the_dark_stylesheet() {
        let root = workdir();
        let p = probe(
            "GNOME",
            &root,
            &[
                (
                    "gsettings get org.gnome.desktop.interface gtk-theme",
                    "'Yaru'",
                ),
                (
                    "gsettings get org.gnome.desktop.interface color-scheme",
                    "'prefer-dark'",
                ),
                (
                    "gsettings get org.gnome.desktop.interface accent-color",
                    "'orange'",
                ),
            ],
        );
        put(
            &p.data_dirs[0],
            "themes/Yaru/gtk-3.0/gtk.css",
            "@define-color theme_base_color #ffffff;\n@define-color theme_text_color #000000;\n",
        );
        put(
            &p.data_dirs[0],
            "themes/Yaru/gtk-3.0/gtk-dark.css",
            "@define-color theme_base_color #1e1e1e;\n@define-color theme_text_color #ffffff;\n",
        );
        let c = read(&p);
        assert_eq!(c.dark, Some(true));
        assert_eq!(c.view_bg.map(hex), Some(0x1E1E1E));
        assert_eq!(c.accent.map(hex), Some(0xED5B00));
        let t = palette(&c).unwrap();
        assert_eq!(hex(t.main_bg), 0x1E1E1E);
        assert_eq!(hex(t.primary), 0xED5B00);
        assert_eq!(hex(t.syn_keyword), hex(Theme::default().syn_keyword));

        // The accent is GNOME's own: another desktop's leftover GNOME setting isn't read.
        let cinnamon = probe(
            "X-Cinnamon",
            &root,
            &[(
                "gsettings get org.gnome.desktop.interface accent-color",
                "'orange'",
            )],
        );
        assert_eq!(read(&cinnamon), Colors::default());

        // A dark theme name, or GTK's own settings.ini, says dark too.
        let named = probe(
            "MATE",
            &root,
            &[(
                "gsettings get org.mate.interface gtk-theme",
                "'Mint-Y-Dark-Aqua'",
            )],
        );
        assert_eq!(read(&named).dark, Some(true));
        let other = probe("sway", &root, &[]);
        put(
            &other.config_home,
            "gtk-3.0/settings.ini",
            "[Settings]\ngtk-theme-name=Arc\ngtk-application-prefer-dark-theme=1\n",
        );
        assert_eq!(read(&other).dark, Some(true));
    }

    #[test]
    fn kde_colour_schemes_from_kdeglobals() {
        let root = workdir();
        let p = probe("KDE", &root, &[]);
        assert_eq!(read(&p), Colors::default());
        put(
            &p.config_home,
            "kdeglobals",
            "[General]\nColorScheme=BreezeDark\nAccentColor=61,174,233\n\n\
             [Colors:Window]\nBackgroundNormal=32,35,38\nForegroundNormal=252,252,252\n\n\
             [Colors:View]\nBackgroundNormal=20,22,24\nForegroundNormal=252,252,252\n\n\
             [Colors:Selection]\nBackgroundNormal=61,174,233,255\nForegroundNormal=#ffffff\n",
        );
        let c = read(&p);
        assert_eq!(c.dark, Some(true));
        assert_eq!(c.window_bg.map(hex), Some(0x202326));
        assert_eq!(c.view_bg.map(hex), Some(0x141618));
        assert_eq!(c.selected_bg.map(hex), Some(0x3DAEE9));
        assert_eq!(c.selected_fg.map(hex), Some(0xFFFFFF));
        assert_eq!(c.accent.map(hex), Some(0x3DAEE9));
        let t = palette(&c).unwrap();
        assert_eq!(
            (hex(t.frame_bg), hex(t.main_bg), hex(t.text)),
            (0x202326, 0x141618, 0xFCFCFC)
        );
        assert_eq!(hex(t.primary), 0x3DAEE9);
    }

    #[test]
    fn colours_that_dont_read_are_dropped_for_the_base() {
        let light = Colors {
            window_bg: Some(rgb_hex(0xC0C0C0)),
            view_bg: Some(rgb_hex(0xFFFFFF)),
            view_fg: Some(rgb_hex(0x000000)),
            selected_bg: Some(rgb_hex(0x000080)),
            selected_fg: Some(rgb_hex(0xFFFFFF)),
            ..Colors::default()
        };
        let t = palette(&light).unwrap();
        assert_eq!((hex(t.frame_bg), hex(t.main_bg)), (0xC0C0C0, 0xFFFFFF));
        assert_eq!(hex(t.primary), 0x000080);
        assert_eq!(hex(t.primary_text), 0xFFFFFF);
        // The selection is a tint that black text still reads on.
        assert!(contrast(t.text, t.selected_bg) >= TEXT_CONTRAST);
        assert_ne!(hex(t.selected_bg), hex(Theme::light().selected_bg));

        // Grey text on grey: the theme's colours go, the light preference stays.
        let murky = Colors {
            view_bg: Some(rgb_hex(0x808080)),
            view_fg: Some(rgb_hex(0x606060)),
            selected_bg: Some(rgb_hex(0x000080)),
            dark: Some(false),
            ..Colors::default()
        };
        let t = palette(&murky).unwrap();
        assert!(t.same_palette(&Theme::light()));

        // Only a dark preference: Kyde Dark. Only an accent: Kyde's default with it.
        let dark = Colors {
            dark: Some(true),
            ..Colors::default()
        };
        assert!(palette(&dark).unwrap().same_palette(&Theme::default()));
        let accent = Colors {
            accent: Some(rgb_hex(0xF6D32D)),
            ..Colors::default()
        };
        let t = palette(&accent).unwrap();
        assert_eq!(hex(t.primary), 0xF6D32D);
        // White doesn't read on a light accent: black text.
        assert_eq!(hex(t.primary_text), 0x000000);
        // Nothing said: the user's own palette stays.
        assert!(palette(&Colors::default()).is_none());
    }

    #[test]
    fn inactive_icons_read_on_the_window_and_the_content() {
        // A grey window (the left rail sits on it) round black fields, and round white ones:
        // a grey mixed from the fields alone vanishes on the rail.
        for (frame, main, text) in [
            (0x3C3C3C, 0x000000, 0xC0C0C0),
            (0xC0C0C0, 0xFFFFFF, 0x000000),
        ] {
            let t = palette(&Colors {
                window_bg: Some(rgb_hex(frame)),
                view_bg: Some(rgb_hex(main)),
                view_fg: Some(rgb_hex(text)),
                ..Colors::default()
            })
            .unwrap();
            assert!(
                contrast(t.line_number, t.frame_bg) >= ICON_CONTRAST,
                "on {frame:06X}"
            );
            assert!(
                contrast(t.line_number, t.main_bg) >= ICON_CONTRAST,
                "on {main:06X}"
            );
            // Still quieter than the text.
            assert!(contrast(t.line_number, t.main_bg) < contrast(t.text, t.main_bg));
        }
        // One background, where the plain mix already reads: it's kept.
        let (bg, fg) = (rgb_hex(0x242424), rgb_hex(0xFFFFFF));
        let t = palette(&Colors {
            window_bg: Some(bg),
            view_bg: Some(bg),
            view_fg: Some(fg),
            ..Colors::default()
        })
        .unwrap();
        assert_eq!(hex(t.line_number), hex(mix(bg, fg, 0.35)));
    }
}
