//! The desktop's fonts: font settings as Pango and Qt write them, the user's terminal's own
//! font, and the check that gpui can draw a family before Kyde switches to it.

use super::{file_name, find_on, ini_value, unquote, Desktop, Probe};
use crate::theme::font::Face;

/// A font setting: a family and, when given, its size.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FontSpec {
    family: String,
    size: Option<Size>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Size {
    Points(f32),
    Pixels(f32),
}

impl FontSpec {
    /// Just a family, at Kyde's size.
    pub(super) fn family(family: &str) -> Self {
        Self {
            family: family.to_string(),
            size: None,
        }
    }
}

impl Size {
    /// The size in gpui px: 96 px per inch, so a point is 4/3 px, times the desktop's own text
    /// scaling (`scale`, see [`text_scale`]). Pixel sizes are already px. Clamped to the range
    /// Kyde's own sizes keep to.
    fn px(self, scale: f32) -> f32 {
        let px = match self {
            Self::Points(pt) => pt * 96.0 / 72.0 * scale,
            Self::Pixels(px) => px,
        };
        px.clamp(6.0, 40.0)
    }
}

/// Words Pango reads as style, weight, stretch, variant or gravity at the end of a font
/// description (`DejaVu Sans Mono Bold Oblique 10`), compared without case or `-`.
const PANGO_STYLE_WORDS: &[&str] = &[
    "normal",
    "roman",
    "oblique",
    "italic",
    "smallcaps",
    "allsmallcaps",
    "petitecaps",
    "allpetitecaps",
    "unicase",
    "titlecaps",
    "thin",
    "ultralight",
    "extralight",
    "light",
    "semilight",
    "demilight",
    "book",
    "regular",
    "medium",
    "semibold",
    "demibold",
    "bold",
    "ultrabold",
    "extrabold",
    "heavy",
    "black",
    "ultraheavy",
    "ultrablack",
    "extrablack",
    "ultracondensed",
    "extracondensed",
    "condensed",
    "semicondensed",
    "semiexpanded",
    "expanded",
    "extraexpanded",
    "ultraexpanded",
    "notrotated",
    "south",
    "upsidedown",
    "north",
    "rotatedleft",
    "east",
    "rotatedright",
    "west",
];

/// A Pango font description, as GNOME, Cinnamon, MATE, XFCE and their terminals store fonts:
/// `FAMILY[,FAMILY…] [STYLE…] [SIZE[px]] [@VARIATIONS] [#FEATURES]`. The first family, without
/// the style words; `None` when there's no family.
pub(super) fn parse_pango(desc: &str) -> Option<FontSpec> {
    let mut words: Vec<&str> = desc.split_whitespace().collect();
    while words
        .last()
        .is_some_and(|w| w.starts_with('@') || w.starts_with('#'))
    {
        words.pop();
    }
    let size = words.last().and_then(|w| match w.strip_suffix("px") {
        Some(px) => px.parse().ok().filter(|v: &f32| *v > 0.0).map(Size::Pixels),
        None => w.parse().ok().filter(|v: &f32| *v > 0.0).map(Size::Points),
    });
    if size.is_some() {
        words.pop();
    }
    while words.last().is_some_and(|w| {
        let w = w.to_lowercase().replace('-', "");
        PANGO_STYLE_WORDS.contains(&w.as_str())
    }) {
        words.pop();
    }
    let families = words.join(" ");
    let family = families.split(',').map(unquote).find(|f| !f.is_empty())?;
    Some(FontSpec {
        family: family.to_string(),
        size,
    })
}

/// A Qt font string, as KDE stores fonts: `Family,pointSize,pixelSize,…` (a point size of -1
/// means the pixel size is set instead).
pub(super) fn parse_qt(desc: &str) -> Option<FontSpec> {
    let mut fields = desc.split(',');
    let family = unquote(fields.next()?);
    if family.is_empty() {
        return None;
    }
    let mut number = || {
        fields
            .next()
            .and_then(|f| f.trim().parse::<f32>().ok())
            .filter(|v| *v > 0.0)
    };
    let (points, pixels) = (number(), number());
    Some(FontSpec {
        family: family.to_string(),
        size: points.map(Size::Points).or(pixels.map(Size::Pixels)),
    })
}

/// The desktop's text scaling, applied to point sizes. Only under Wayland: on X11 gpui scales
/// everything by `Xft.dpi / 96`, and the desktop's settings daemon already folds its text
/// scaling (GNOME's text-scaling-factor, KDE's font DPI) into `Xft.dpi` — scaling again would
/// count it twice.
pub(super) fn text_scale(p: &Probe) -> f32 {
    let schema = match p.desktop {
        _ if !p.wayland => return 1.0,
        Desktop::Cinnamon => "org.cinnamon.desktop.interface",
        Desktop::Gnome => "org.gnome.desktop.interface",
        _ => return 1.0,
    };
    p.gsettings(schema, "text-scaling-factor")
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|f| (0.5..=3.0).contains(f))
        .unwrap_or(1.0)
}

/// fontconfig's generic names, which name no font of their own.
fn is_alias(family: &str) -> bool {
    [
        "monospace",
        "mono",
        "sans",
        "sans-serif",
        "serif",
        "system-ui",
    ]
    .iter()
    .any(|a| a.eq_ignore_ascii_case(family))
}

/// `family` written into a fontconfig pattern (`-`, `:`, `,` and `\` are pattern syntax).
fn fc_escape(family: &str) -> String {
    let mut out = String::with_capacity(family.len());
    for ch in family.chars() {
        if matches!(ch, '\\' | '-' | ':' | ',') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// The face Kyde draws `spec` with, when gpui can draw it: a generic name (`Monospace`) is
/// first resolved by fontconfig; then fontconfig must have the family as a scalable TrueType
/// or OpenType font (a Windows `.fon` or a PCF bitmap font isn't), and gpui's text system must
/// know it (`gpui_family` gives its spelling), so gpui never quietly draws a stand-in.
pub(super) fn face(
    p: &Probe,
    spec: FontSpec,
    scale: f32,
    gpui_family: &dyn Fn(&str) -> Option<String>,
) -> Option<Face> {
    let family = if is_alias(&spec.family) {
        let found = p.run("fc-match", &["-f", "%{family[0]}", &spec.family])?;
        found.trim().to_string()
    } else {
        spec.family
    };
    if family.is_empty() {
        return None;
    }
    let pattern = format!("{}:scalable=true", fc_escape(&family));
    let formats = p.run("fc-list", &["-f", "%{fontformat}\n", &pattern])?;
    if !formats
        .lines()
        .any(|f| matches!(f.trim(), "TrueType" | "CFF"))
    {
        return None;
    }
    let family = gpui_family(&family)?;
    Some(Face::new(&family, spec.size.map(|s| s.px(scale))))
}

/// The desktop's monospace font.
pub(super) fn monospace(p: &Probe) -> Option<FontSpec> {
    match p.desktop {
        Desktop::Cinnamon | Desktop::Gnome => p
            .gsettings("org.gnome.desktop.interface", "monospace-font-name")
            .and_then(|f| parse_pango(&f)),
        Desktop::Mate => p
            .gsettings("org.mate.interface", "monospace-font-name")
            .and_then(|f| parse_pango(&f)),
        Desktop::Kde => p.kde_setting("General", "fixed").and_then(|f| parse_qt(&f)),
        Desktop::Xfce => p
            .xfconf("xsettings", "/Gtk/MonospaceFontName")
            .and_then(|f| parse_pango(&f)),
        Desktop::Other => None,
    }
}

/// The desktop's UI font.
pub(super) fn ui(p: &Probe) -> Option<FontSpec> {
    match p.desktop {
        Desktop::Kde => p.kde_setting("General", "font").and_then(|f| parse_qt(&f)),
        Desktop::Xfce => p
            .xfconf("xsettings", "/Gtk/FontName")
            .and_then(|f| parse_pango(&f)),
        Desktop::Other => p.gtk_setting("gtk-font-name").and_then(|f| parse_pango(&f)),
        d => d
            .interface_schema()
            .and_then(|s| p.gsettings(s, "font-name"))
            .and_then(|f| parse_pango(&f)),
    }
}

/// The terminals whose own font setting Kyde reads.
const TERMINALS: [&str; 7] = [
    "gnome-terminal",
    "ptyxis",
    "mate-terminal",
    "xfce4-terminal",
    "konsole",
    "kitty",
    "alacritty",
];

/// The font set in the user's own terminal (the one Open in Terminal starts); `None` when it
/// uses the system monospace font, or it's a terminal or setting Kyde can't read.
pub(super) fn terminal(p: &Probe) -> Option<FontSpec> {
    match terminal_name(p)? {
        "gnome-terminal" => {
            let profile = p.gsettings("org.gnome.Terminal.ProfilesList", "default")?;
            own_font(
                p,
                &format!(
                    "org.gnome.Terminal.Legacy.Profile:/org/gnome/terminal/legacy/profiles:/:{profile}/"
                ),
                "font",
            )
        }
        "ptyxis" => own_font(p, "org.gnome.Ptyxis", "font-name"),
        "mate-terminal" => {
            let profile = p.gsettings("org.mate.terminal.global", "default-profile")?;
            own_font(
                p,
                &format!("org.mate.terminal.profile:/org/mate/terminal/profiles/{profile}/"),
                "font",
            )
        }
        "xfce4-terminal" => xfce4_terminal(p),
        "konsole" => konsole(p),
        "kitty" => Some(kitty(p)),
        "alacritty" => alacritty(p),
        _ => None,
    }
}

/// Which of [`TERMINALS`] the terminal program is: by its name (Debian's `.real` / `.wrapper`
/// launchers included), else the program a link points to (Debian's `x-terminal-emulator`),
/// and for `xdg-terminal-exec` the first installed terminal on its lists.
pub(super) fn terminal_name(p: &Probe) -> Option<&'static str> {
    let program = p.terminal.as_deref()?;
    let known = |name: &str| {
        let name = name
            .strip_suffix(".real")
            .or_else(|| name.strip_suffix(".wrapper"))
            .unwrap_or(name);
        TERMINALS.into_iter().find(|t| *t == name)
    };
    if file_name(program) == "xdg-terminal-exec" {
        return xdg_terminal_exec_choice(p);
    }
    known(file_name(program)).or_else(|| known(file_name(&std::fs::canonicalize(program).ok()?)))
}

/// The terminal `xdg-terminal-exec` starts: the first entry of its lists (per-desktop list
/// before the plain one, the user's folders before the system's) that is installed; `None` at
/// an installed terminal Kyde can't read.
fn xdg_terminal_exec_choice(p: &Probe) -> Option<&'static str> {
    let data = |d: &std::path::PathBuf| d.join("xdg-terminal-exec");
    let dirs = std::iter::once(p.config_home.clone())
        .chain(p.config_dirs.iter().cloned())
        .chain(std::iter::once(data(&p.data_home)))
        .chain(p.data_dirs.iter().map(data));
    let names: Vec<String> = p
        .desktops
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| format!("{d}-xdg-terminals.list"))
        .chain(std::iter::once("xdg-terminals.list".to_string()))
        .collect();
    for dir in dirs {
        for name in &names {
            let Ok(list) = std::fs::read_to_string(dir.join(name)) else {
                continue;
            };
            // Entries are desktop-file ids, optionally with an action (`:new-window`); lines
            // starting with `#`, `-` (excluded) or `/` (directives) aren't entries.
            for entry in list.lines().map(str::trim) {
                if entry.is_empty() || entry.starts_with(['#', '-', '/']) {
                    continue;
                }
                let id = entry.split(':').next().unwrap_or(entry);
                let id = id.strip_suffix(".desktop").unwrap_or(id);
                let terminal = match id {
                    "org.gnome.Terminal" => "gnome-terminal",
                    "org.gnome.Ptyxis" => "ptyxis",
                    "mate-terminal" => "mate-terminal",
                    "xfce4-terminal" => "xfce4-terminal",
                    "org.kde.konsole" => "konsole",
                    "kitty" => "kitty",
                    "Alacritty" | "alacritty" => "alacritty",
                    // Any other terminal is installed when its desktop file is, as
                    // xdg-terminal-exec checks; one that isn't is skipped like it.
                    _ => {
                        let file = format!("applications/{id}.desktop");
                        if std::iter::once(&p.data_home)
                            .chain(&p.data_dirs)
                            .any(|d| d.join(&file).is_file())
                        {
                            return None;
                        }
                        continue;
                    }
                };
                if find_on(terminal, &p.path).is_some() {
                    return Some(terminal);
                }
            }
        }
    }
    None
}

/// `key` (a Pango font) of a terminal's gsettings `schema`, unless the terminal is set to use
/// the system font.
fn own_font(p: &Probe, schema: &str, key: &str) -> Option<FontSpec> {
    if p.gsettings(schema, "use-system-font")? != "false" {
        return None;
    }
    parse_pango(&p.gsettings(schema, key)?)
}

/// xfce4-terminal: its xfconf channel (1.1+), else its older `terminalrc`.
fn xfce4_terminal(p: &Probe) -> Option<FontSpec> {
    let (system, font) = if let Some(font) = p.xfconf("xfce4-terminal", "/font-name") {
        (p.xfconf("xfce4-terminal", "/font-use-system"), font)
    } else {
        let rc = p.config_file("xfce4/terminal/terminalrc")?;
        (
            ini_value(&rc, "Configuration", "FontUseSystem"),
            ini_value(&rc, "Configuration", "FontName")?,
        )
    };
    if system.is_some_and(|s| s.eq_ignore_ascii_case("true")) {
        return None;
    }
    parse_pango(&font)
}

/// Konsole: the default profile's font (a profile without one uses the system's).
fn konsole(p: &Probe) -> Option<FontSpec> {
    let profile = ini_value(
        &p.config_file("konsolerc")?,
        "Desktop Entry",
        "DefaultProfile",
    )?;
    let text = p.data_file(&format!("konsole/{profile}"))?;
    parse_qt(&ini_value(&text, "Appearance", "Font")?)
}

/// kitty: `font_family` + `font_size` in `kitty.conf`, else kitty's own defaults (fontconfig's
/// `monospace`, 11 pt). The last setting wins, as in kitty; `include`d files aren't read.
fn kitty(p: &Probe) -> FontSpec {
    let conf = std::fs::read_to_string(p.config_home.join("kitty/kitty.conf")).unwrap_or_default();
    let (mut family, mut size) = ("monospace".to_string(), 11.0);
    for line in conf.lines().map(str::trim) {
        let Some((key, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let value = value.trim();
        match key {
            // `font_family Fira Code`, or kitty 0.36's `font_family family="Fira Code" …`.
            "font_family" => {
                let name = match value.split_once("family=") {
                    Some((_, rest)) => quoted_or_word(rest),
                    None => unquote(value),
                };
                family = if name.is_empty() || name == "auto" {
                    "monospace".to_string()
                } else {
                    name.to_string()
                };
            }
            "font_size" => size = value.parse().unwrap_or(size),
            _ => {}
        }
    }
    FontSpec {
        family,
        size: Some(Size::Points(size)),
    }
}

/// Alacritty: `[font] size` and `[font.normal] family` (or `normal = { family = … }`) in
/// `alacritty.toml`, else Alacritty's defaults (fontconfig's `monospace`, 11.25 pt).
fn alacritty(p: &Probe) -> Option<FontSpec> {
    let text = alacritty_toml(p);
    let (mut family, mut size) = ("monospace".to_string(), 11.25);
    let mut section = "";
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match (section, key) {
            ("font", "size") => size = value.parse().unwrap_or(size),
            ("font.normal", "family") => family = unquote(value).to_string(),
            ("font", "normal") => {
                if let Some((_, rest)) = value.split_once("family") {
                    if let Some(rest) = rest.trim_start().strip_prefix('=') {
                        family = quoted_or_word(rest).to_string();
                    }
                }
            }
            _ => {}
        }
    }
    (!family.is_empty()).then_some(FontSpec {
        family,
        size: Some(Size::Points(size)),
    })
}

/// Alacritty's `alacritty.toml`, from the first place Alacritty looks that has one; `""` when
/// there's none.
pub(super) fn alacritty_toml(p: &Probe) -> String {
    [
        p.config_home.join("alacritty/alacritty.toml"),
        p.config_home.join("alacritty.toml"),
        p.home.join(".config/alacritty/alacritty.toml"),
        p.home.join(".alacritty.toml"),
    ]
    .iter()
    .find_map(|f| std::fs::read_to_string(f).ok())
    .unwrap_or_default()
}

/// The quoted string `s` starts with, or its first word.
pub(super) fn quoted_or_word(s: &str) -> &str {
    let s = s.trim_start();
    match s.chars().next() {
        Some(q @ ('"' | '\'')) => s[1..].split(q).next().unwrap_or(""),
        _ => s
            .split(|c: char| c.is_whitespace() || c == ',' || c == '}')
            .next()
            .unwrap_or(""),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{gpui_with, probe, put, workdir};
    use super::*;
    use std::path::PathBuf;

    fn font(family: &str, size: Option<Size>) -> FontSpec {
        FontSpec {
            family: family.to_string(),
            size,
        }
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }
    use Size::{Pixels, Points};

    #[test]
    fn pango_descriptions_give_the_family_and_size() {
        assert_eq!(
            parse_pango("DejaVu Sans Mono 10"),
            Some(font("DejaVu Sans Mono", Some(Points(10.0))))
        );
        // Style words go, whatever their case or hyphens.
        assert_eq!(
            parse_pango("Ubuntu Sans Mono Bold Italic 11"),
            Some(font("Ubuntu Sans Mono", Some(Points(11.0))))
        );
        assert_eq!(
            parse_pango("Noto Sans semi-bold Condensed 10.5"),
            Some(font("Noto Sans", Some(Points(10.5))))
        );
        // A family list: the first; variations and features after the size.
        assert_eq!(
            parse_pango("Cantarell, Noto Sans 11 @wght=500 #tnum=1"),
            Some(font("Cantarell", Some(Points(11.0))))
        );
        assert_eq!(
            parse_pango("Monospace 13px"),
            Some(font("Monospace", Some(Pixels(13.0))))
        );
        assert_eq!(
            parse_pango("\"JetBrains Mono\" 12"),
            Some(font("JetBrains Mono", Some(Points(12.0))))
        );
        // No size: the family alone. A name ending in a non-style word keeps it.
        assert_eq!(
            parse_pango("Noto Sans Mono"),
            Some(font("Noto Sans Mono", None))
        );
        // Nothing but style or size: no family.
        assert_eq!(parse_pango("Bold 10"), None);
        assert_eq!(parse_pango(""), None);
    }

    #[test]
    fn qt_font_strings_give_the_family_and_size() {
        assert_eq!(
            parse_qt("Hack,10,-1,5,50,0,0,0,0,0"),
            Some(font("Hack", Some(Points(10.0))))
        );
        assert_eq!(
            parse_qt("Noto Sans,10.5,-1,5,400,0,0,0,0,0,0,0,0,0,0,1,Regular"),
            Some(font("Noto Sans", Some(Points(10.5))))
        );
        assert_eq!(
            parse_qt("Terminus,-1,14,5,50,0,0,0,0,0"),
            Some(font("Terminus", Some(Pixels(14.0))))
        );
        assert_eq!(parse_qt("Hack"), Some(font("Hack", None)));
        assert_eq!(parse_qt(",10,-1"), None);
        assert_eq!(parse_qt(""), None);
    }

    #[test]
    fn points_become_px_at_96_dpi_times_the_text_scale() {
        assert!(near(Points(10.0).px(1.0), 13.333));
        assert!(near(Points(11.0).px(1.25), 18.333));
        assert!(near(Pixels(13.0).px(2.0), 13.0));
        // Clamped like Kyde's own sizes.
        assert!(near(Points(72.0).px(1.0), 40.0));
        assert!(near(Points(2.0).px(1.0), 6.0));
    }

    #[test]
    fn text_scaling_counts_only_under_wayland() {
        let root = workdir();
        let answers = [(
            "gsettings get org.gnome.desktop.interface text-scaling-factor",
            "1.25",
        )];
        let mut p = probe("GNOME", &root, &answers);
        assert!(near(text_scale(&p), 1.0));
        p.wayland = true;
        assert!(near(text_scale(&p), 1.25));
        // No setting, a nonsense one, or a desktop without one: unscaled.
        assert!(near(text_scale(&probe("GNOME", &root, &[])), 1.0));
        let mut odd = probe(
            "GNOME",
            &root,
            &[(
                "gsettings get org.gnome.desktop.interface text-scaling-factor",
                "40",
            )],
        );
        odd.wayland = true;
        assert!(near(text_scale(&odd), 1.0));
        let mut kde = probe("KDE", &root, &answers);
        kde.wayland = true;
        assert!(near(text_scale(&kde), 1.0));
    }

    #[test]
    fn only_a_font_gpui_can_draw_becomes_a_face() {
        let root = workdir();
        let p = probe(
            "GNOME",
            &root,
            &[
                ("fc-match -f %{family[0]} Monospace", "DejaVu Sans Mono"),
                (
                    "fc-list -f %{fontformat}\n DejaVu Sans Mono:scalable=true",
                    "TrueType",
                ),
                // fontconfig matches names without case, as the real one does.
                (
                    "fc-list -f %{fontformat}\n source code pro:scalable=true",
                    "CFF\nType 1",
                ),
                ("fc-list -f %{fontformat}\n Terminus:scalable=true", "PCF"),
                (
                    "fc-list -f %{fontformat}\n Fira\\-Code:scalable=true",
                    "TrueType",
                ),
                ("fc-list -f %{fontformat}\n Cantarell:scalable=true", ""),
            ],
        );
        let gpui = gpui_with(&[
            "DejaVu Sans Mono",
            "Source Code Pro",
            "Terminus",
            "Cantarell",
        ]);
        let face = |s: Option<FontSpec>| face(&p, s.unwrap(), 1.0, &gpui);
        // A generic name resolves through fontconfig first.
        assert_eq!(
            face(parse_pango("Monospace 9")),
            Some(Face::new("DejaVu Sans Mono", Some(12.0)))
        );
        // An OpenType (CFF) font draws; gpui's spelling is kept.
        assert_eq!(
            face(parse_pango("source code pro 12")),
            Some(Face::new("Source Code Pro", Some(16.0)))
        );
        // Bitmap-only, not installed, or not in gpui's text system: no face.
        assert_eq!(face(parse_pango("Terminus 12")), None);
        assert_eq!(face(parse_pango("Cantarell 11")), None);
        assert_eq!(face(parse_pango("Fira-Code 11")), None);
        // No fontconfig at all: nothing can be checked, so nothing switches.
        let bare = probe("GNOME", &root, &[]);
        assert_eq!(
            super::face(&bare, FontSpec::family("DejaVu Sans Mono"), 1.0, &gpui),
            None
        );
    }

    #[test]
    fn each_desktop_names_its_monospace_and_ui_fonts() {
        let root = workdir();
        let answers = [
            (
                "gsettings get org.mate.interface monospace-font-name",
                "'Ubuntu Mono 12'",
            ),
            ("gsettings get org.mate.interface font-name", "'Ubuntu 11'"),
            (
                "xfconf-query -c xsettings -p /Gtk/MonospaceFontName",
                "Monospace 10",
            ),
            ("xfconf-query -c xsettings -p /Gtk/FontName", "Sans 10"),
        ];
        let mate = probe("MATE", &root, &answers);
        assert_eq!(
            monospace(&mate),
            Some(font("Ubuntu Mono", Some(Points(12.0))))
        );
        assert_eq!(ui(&mate), Some(font("Ubuntu", Some(Points(11.0)))));
        let xfce = probe("XFCE", &root, &answers);
        assert_eq!(
            monospace(&xfce),
            Some(font("Monospace", Some(Points(10.0))))
        );
        assert_eq!(ui(&xfce), Some(font("Sans", Some(Points(10.0)))));

        let kde = probe("KDE", &root, &[]);
        put(
            &kde.config_dirs[0],
            "kdeglobals",
            "[General]\nfixed=Hack,10,-1,5,50,0,0,0,0,0\nfont=Noto Sans,10,-1,5,50,0,0,0,0,0\n",
        );
        assert_eq!(monospace(&kde), Some(font("Hack", Some(Points(10.0)))));
        // The user's own kdeglobals wins over the system's.
        put(
            &kde.config_home,
            "kdeglobals",
            "[General]\nfont=Inter,9,-1,5,50,0,0,0,0,0\n",
        );
        assert_eq!(ui(&kde), Some(font("Inter", Some(Points(9.0)))));

        // Any other desktop: GTK's own settings.ini for the UI font, no monospace setting.
        let other = probe("sway", &root, &[]);
        put(
            &other.config_home,
            "gtk-3.0/settings.ini",
            "[Settings]\ngtk-theme-name=Arc-Dark\ngtk-font-name=Cantarell 11\n",
        );
        assert_eq!(ui(&other), Some(font("Cantarell", Some(Points(11.0)))));
        assert_eq!(monospace(&other), None);
    }

    fn on_terminal(p: &mut Probe, program: &str) -> Option<FontSpec> {
        p.terminal = Some(PathBuf::from(program));
        terminal(p)
    }

    #[test]
    fn gsettings_terminals_use_their_own_font_unless_set_to_the_system_one() {
        let root = workdir();
        let mut p = probe(
            "GNOME",
            &root,
            &[
                ("gsettings get org.gnome.Ptyxis use-system-font", "false"),
                ("gsettings get org.gnome.Ptyxis font-name", "'Fira Code 12'"),
                ("gsettings get org.mate.terminal.global default-profile", "'default'"),
                (
                    "gsettings get org.mate.terminal.profile:/org/mate/terminal/profiles/default/ use-system-font",
                    "true",
                ),
                (
                    "gsettings get org.mate.terminal.profile:/org/mate/terminal/profiles/default/ font",
                    "'Monospace 13'",
                ),
            ],
        );
        assert_eq!(
            on_terminal(&mut p, "/usr/bin/ptyxis"),
            Some(font("Fira Code", Some(Points(12.0))))
        );
        assert_eq!(on_terminal(&mut p, "/usr/bin/mate-terminal"), None);
        // gnome-terminal's profile list unreadable: no font of its own.
        assert_eq!(on_terminal(&mut p, "/usr/bin/gnome-terminal.wrapper"), None);
        // A terminal Kyde doesn't read, or none at all.
        assert_eq!(on_terminal(&mut p, "/usr/bin/xterm"), None);
        p.terminal = None;
        assert_eq!(terminal(&p), None);
    }

    #[test]
    fn config_file_terminals_read_their_own_files() {
        let root = workdir();
        let mut p = probe("KDE", &root, &[]);
        put(
            &p.config_home,
            "konsolerc",
            "[Desktop Entry]\nDefaultProfile=Mine.profile\n",
        );
        put(
            &p.data_home,
            "konsole/Mine.profile",
            "[Appearance]\nColorScheme=Breeze\nFont=Hack,11,-1,5,50,0,0,0,0,0\n",
        );
        assert_eq!(
            on_terminal(&mut p, "konsole"),
            Some(font("Hack", Some(Points(11.0))))
        );

        put(
            &p.config_home,
            "xfce4/terminal/terminalrc",
            "[Configuration]\nFontName=DejaVu Sans Mono 9\nFontUseSystem=FALSE\n",
        );
        assert_eq!(
            on_terminal(&mut p, "xfce4-terminal"),
            Some(font("DejaVu Sans Mono", Some(Points(9.0))))
        );

        // kitty with no config: its defaults. Then its own settings, quoted or not.
        assert_eq!(
            on_terminal(&mut p, "kitty"),
            Some(font("monospace", Some(Points(11.0))))
        );
        put(
            &p.config_home,
            "kitty/kitty.conf",
            "# font_family Hack\nfont_family family=\"Fira Code\" style=Retina\nfont_size 13.5\n",
        );
        assert_eq!(
            on_terminal(&mut p, "kitty"),
            Some(font("Fira Code", Some(Points(13.5))))
        );

        assert_eq!(
            on_terminal(&mut p, "alacritty"),
            Some(font("monospace", Some(Points(11.25))))
        );
        put(
            &p.config_home,
            "alacritty/alacritty.toml",
            "[colors.primary]\nbackground = \"#1d1f21\"\n\n[font]\nsize = 12\nnormal = { family = \"Iosevka Term\", style = \"Regular\" }\n",
        );
        assert_eq!(
            on_terminal(&mut p, "alacritty"),
            Some(font("Iosevka Term", Some(Points(12.0))))
        );
        put(
            &p.config_home,
            "alacritty/alacritty.toml",
            "[font.normal]\nfamily = 'JetBrains Mono'\n",
        );
        assert_eq!(
            on_terminal(&mut p, "alacritty"),
            Some(font("JetBrains Mono", Some(Points(11.25))))
        );
    }

    /// Stock Ubuntu: GNOME's terminal is `xdg-terminal-exec`, whose list names Ptyxis, and the
    /// `x-terminal-emulator` alternative links to it.
    #[test]
    fn launchers_are_seen_through_to_the_terminal_they_start() {
        let root = workdir();
        let answers = [
            ("gsettings get org.gnome.Ptyxis use-system-font", "false"),
            (
                "gsettings get org.gnome.Ptyxis font-name",
                "'Source Code Pro 11'",
            ),
        ];
        let mut p = probe("ubuntu:GNOME", &root, &answers);
        // Installed: ptyxis only.
        let bin = root.join("bin");
        put(&bin, "ptyxis", "");
        std::fs::set_permissions(
            bin.join("ptyxis"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let ptyxis = Some(font("Source Code Pro", Some(Points(11.0))));
        // The system's per-desktop list; an uninstalled terminal before it is skipped.
        put(
            &p.data_dirs[0],
            "xdg-terminal-exec/ubuntu-xdg-terminals.list",
            "# preferred\n-org.kde.yakuake.desktop\n/execarg_default:kitty.desktop:--\nkitty.desktop\norg.gnome.Ptyxis.desktop:new-window\n",
        );
        assert_eq!(on_terminal(&mut p, "/usr/bin/xdg-terminal-exec"), ptyxis);
        // The user's list comes first; a terminal Kyde can't read is skipped while it isn't
        // installed, and stops the search once it is.
        put(
            &p.config_home,
            "xdg-terminals.list",
            "org.gnome.Console.desktop\n",
        );
        assert_eq!(on_terminal(&mut p, "/usr/bin/xdg-terminal-exec"), ptyxis);
        put(
            &p.data_dirs[0],
            "applications/org.gnome.Console.desktop",
            "",
        );
        assert_eq!(on_terminal(&mut p, "/usr/bin/xdg-terminal-exec"), None);
        // The x-terminal-emulator link.
        let alt = root.join("x-terminal-emulator");
        std::os::unix::fs::symlink(bin.join("ptyxis"), &alt).unwrap();
        assert_eq!(on_terminal(&mut p, alt.to_str().unwrap()), ptyxis);
    }
}
