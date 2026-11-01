//! Your terminal's colours: the background, text, 16-colour palette, and bold and cursor
//! colours set in the user's own terminal app, read from its own settings. A value Kyde can't
//! read stays `None`, which the terminal draws in Kyde's own colour.

use super::colors::{self, kde_color, parse_term_color};
use super::fonts::{alacritty_toml, quoted_or_word};
use super::{ini_value, unquote, Probe};
use crate::theme::TerminalLook;
use kyde_color::Color;
use std::path::Path;

/// The colours set in `terminal` (one of the terminals `fonts::terminal_name` knows).
pub(super) fn read(p: &Probe, terminal: &str) -> TerminalLook {
    match terminal {
        "gnome-terminal" => p
            .gsettings("org.gnome.Terminal.ProfilesList", "default")
            .map(|id| {
                vte_profile(
                    p,
                    &format!(
                        "org.gnome.Terminal.Legacy.Profile:/org/gnome/terminal/legacy/profiles:/:{id}/"
                    ),
                )
            }),
        "mate-terminal" => p
            .gsettings("org.mate.terminal.global", "default-profile")
            .map(|id| {
                vte_profile(
                    p,
                    &format!("org.mate.terminal.profile:/org/mate/terminal/profiles/{id}/"),
                )
            }),
        "ptyxis" => ptyxis(p),
        "xfce4-terminal" => Some(xfce4_terminal(p)),
        "konsole" => konsole(p),
        "kitty" => Some(kitty(p)),
        "alacritty" => Some(alacritty(p)),
        _ => None,
    }
    .unwrap_or_default()
}

/// A VTE terminal's profile in gsettings: gnome-terminal and MATE Terminal use the same keys.
/// With `use-theme-colors` on, the text and background are the GTK theme's content colours.
fn vte_profile(p: &Probe, schema: &str) -> TerminalLook {
    let get = |key: &str| p.gsettings(schema, key);
    let color = |key: &str| get(key).as_deref().and_then(parse_term_color);
    let on = |key: &str| get(key).map(|v| v == "true");
    let (foreground, background) = if on("use-theme-colors") == Some(true) {
        colors::read(p).content()
    } else {
        (color("foreground-color"), color("background-color"))
    };
    let cursor_set = on("cursor-colors-set") == Some(true);
    TerminalLook {
        background,
        foreground,
        ansi: get("palette").map_or([None; 16], |v| palette(&v)),
        bold: (on("bold-color-same-as-fg") == Some(false))
            .then(|| color("bold-color"))
            .flatten(),
        cursor: cursor_set
            .then(|| color("cursor-background-color"))
            .flatten(),
        cursor_text: cursor_set
            .then(|| color("cursor-foreground-color"))
            .flatten(),
        face: None,
    }
}

/// The 16 ANSI colours of a palette setting: a gsettings string list (`['#000000', …]`), or one
/// string split on `:` (MATE Terminal) or `;` (xfce4-terminal). A short palette leaves the rest
/// to Kyde, and an entry that isn't a colour leaves that one.
fn palette(value: &str) -> [Option<Color>; 16] {
    let value = value.trim();
    let entries: Vec<String> = if value.starts_with('[') || value.starts_with('@') {
        quoted_strings(value)
    } else {
        unquote(value)
            .split([':', ';'])
            .map(str::to_string)
            .collect()
    };
    let mut out = [None; 16];
    for (slot, entry) in out.iter_mut().zip(&entries) {
        *slot = parse_term_color(entry);
    }
    out
}

/// The quoted strings in a value gsettings prints (`['a', "it's"]`), unescaped.
fn quoted_strings(printed: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = printed.chars();
    while let Some(c) = chars.next() {
        if c != '\'' && c != '"' {
            continue;
        }
        let mut s = String::new();
        while let Some(d) = chars.next() {
            match d {
                '\\' => s.extend(chars.next()),
                d if d == c => break,
                d => s.push(d),
            }
        }
        out.push(s);
    }
    out
}

/// Ptyxis: the default profile's palette, when it's one of the user's own palette files (the
/// palettes Ptyxis ships live inside its program, where Kyde can't read them). A palette with
/// light and dark variants gives the one Ptyxis shows.
fn ptyxis(p: &Probe) -> Option<TerminalLook> {
    let uuid = p.gsettings("org.gnome.Ptyxis", "default-profile-uuid")?;
    let id = p.gsettings(
        &format!("org.gnome.Ptyxis.Profile:/org/gnome/Ptyxis/Profiles/{uuid}/"),
        "palette",
    )?;
    if id.contains('/') {
        return None;
    }
    let text = std::fs::read_to_string(
        p.data_home
            .join("org.gnome.Ptyxis/palettes")
            .join(format!("{id}.palette")),
    )
    .ok()?;
    let has = |section: &str| {
        let header = format!("[{section}]");
        text.lines().any(|l| l.trim() == header)
    };
    let section = if has("Dark") || has("Light") {
        let dark = match p
            .gsettings("org.gnome.Ptyxis", "interface-style")
            .as_deref()
        {
            Some("dark") => true,
            Some("light") => false,
            // Following the system, as libadwaita does: dark only when the desktop says so.
            _ => colors::read(p).dark() == Some(true),
        };
        if dark {
            "Dark"
        } else {
            "Light"
        }
    } else {
        "Palette"
    };
    let color = |key: &str| ini_value(&text, section, key).and_then(|v| parse_term_color(&v));
    let mut look = TerminalLook {
        background: color("Background"),
        foreground: color("Foreground"),
        cursor: color("CursorBackground").or_else(|| color("Cursor")),
        cursor_text: color("CursorForeground"),
        ..TerminalLook::default()
    };
    for (i, slot) in look.ansi.iter_mut().enumerate() {
        *slot = color(&format!("Color{i}"));
    }
    Some(look)
}

/// xfce4-terminal: its xfconf channel (1.1+) once that holds anything, else its older
/// `terminalrc`.
fn xfce4_terminal(p: &Probe) -> TerminalLook {
    let xfconf = p
        .run("xfconf-query", &["-c", "xfce4-terminal", "-l"])
        .is_some_and(|l| !l.trim().is_empty());
    let rc = if xfconf {
        None
    } else {
        p.config_file("xfce4/terminal/terminalrc")
    };
    let get = |property: &str, key: &str| {
        if xfconf {
            p.xfconf("xfce4-terminal", property)
        } else {
            ini_value(rc.as_deref()?, "Configuration", key)
        }
    };
    let color = |property: &str, key: &str| get(property, key).and_then(|v| parse_term_color(&v));
    let on = |property: &str, key: &str| get(property, key).map(|v| v.eq_ignore_ascii_case("true"));
    let (foreground, background) = if on("/color-use-theme", "ColorUseTheme") == Some(true) {
        colors::read(p).content()
    } else {
        (
            color("/color-foreground", "ColorForeground"),
            color("/color-background", "ColorBackground"),
        )
    };
    // Both "use default" switches are on unless set off.
    let own_bold = on("/color-bold-use-default", "ColorBoldUseDefault") == Some(false);
    let own_cursor = on("/color-cursor-use-default", "ColorCursorUseDefault") == Some(false);
    TerminalLook {
        background,
        foreground,
        ansi: get("/color-palette", "ColorPalette").map_or([None; 16], |v| palette(&v)),
        bold: own_bold
            .then(|| color("/color-bold", "ColorBold"))
            .flatten(),
        cursor: own_cursor
            .then(|| color("/color-cursor", "ColorCursor"))
            .flatten(),
        cursor_text: own_cursor
            .then(|| color("/color-cursor-foreground", "ColorCursorForeground"))
            .flatten(),
        face: None,
    }
}

/// Konsole: the default profile's colour scheme (`<name>.colorscheme`: `[Background]`,
/// `[Foreground]`, `[Color0]`…`[Color7]` and their `Intense` brights), and the profile's own
/// cursor colour when it sets one.
fn konsole(p: &Probe) -> Option<TerminalLook> {
    let profile = ini_value(
        &p.config_file("konsolerc")?,
        "Desktop Entry",
        "DefaultProfile",
    )?;
    let profile = p.data_file(&format!("konsole/{profile}"))?;
    let mut look = TerminalLook::default();
    let scheme = ini_value(&profile, "Appearance", "ColorScheme")
        .filter(|s| !s.is_empty() && !s.contains('/'))
        .and_then(|s| p.data_file(&format!("konsole/{s}.colorscheme")));
    if let Some(scheme) = scheme {
        let color =
            |section: &str| ini_value(&scheme, section, "Color").and_then(|v| kde_color(&v));
        look.background = color("Background");
        look.foreground = color("Foreground");
        for i in 0..8 {
            look.ansi[i] = color(&format!("Color{i}"));
            look.ansi[i + 8] = color(&format!("Color{i}Intense"));
        }
    }
    let cursor = |key: &str| ini_value(&profile, "Cursor Options", key);
    if cursor("UseCustomCursorColor").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
        look.cursor = cursor("CustomCursorColor").and_then(|v| kde_color(&v));
        look.cursor_text = cursor("CustomCursorTextColor").and_then(|v| kde_color(&v));
    }
    Some(look)
}

/// kitty: `background`, `foreground`, `color0`…`color15`, `cursor` and `cursor_text_color` in
/// `kitty.conf` and the files it `include`s (a theme kitten's `current-theme.conf`, say). The
/// last setting wins, as in kitty.
fn kitty(p: &Probe) -> TerminalLook {
    let dir = p.config_home.join("kitty");
    let mut look = TerminalLook::default();
    kitty_file(p, &dir, &dir.join("kitty.conf"), &mut look, 0);
    look
}

fn kitty_file(p: &Probe, dir: &Path, file: &Path, look: &mut TerminalLook, depth: usize) {
    // Includes nest only so deep in real configs; this stops a loop.
    let Some(text) = (depth < 8)
        .then(|| std::fs::read_to_string(file).ok())
        .flatten()
    else {
        return;
    };
    for line in text.lines().map(str::trim) {
        let Some((key, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let value = value.trim();
        let color = || parse_term_color(value);
        match key {
            // A plain path; kitty's glob and environment forms aren't followed.
            "include" if !value.contains(['*', '?', '[', '$']) => {
                let path = value
                    .strip_prefix("~/")
                    .map_or_else(|| dir.join(value), |rest| p.home.join(rest));
                kitty_file(p, dir, &path, look, depth + 1);
            }
            "background" => look.background = color(),
            "foreground" => look.foreground = color(),
            "cursor" => look.cursor = color(),
            "cursor_text_color" => look.cursor_text = color(),
            _ => {
                if let Some(i) = key
                    .strip_prefix("color")
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|i| *i < 16)
                {
                    look.ansi[i] = color();
                }
            }
        }
    }
}

/// Alacritty: `[colors.primary]` background + foreground, `[colors.normal]` and
/// `[colors.bright]` black…white, and `[colors.cursor]` in `alacritty.toml`.
fn alacritty(p: &Probe) -> TerminalLook {
    const NAMES: [&str; 8] = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    let text = alacritty_toml(p);
    let mut look = TerminalLook::default();
    let mut section = "";
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let color = parse_term_color(quoted_or_word(value));
        let ansi = |offset: usize| {
            NAMES
                .iter()
                .position(|n| *n == key.trim())
                .map(|i| i + offset)
        };
        match (section, key.trim()) {
            ("colors.primary", "background") => look.background = color,
            ("colors.primary", "foreground") => look.foreground = color,
            ("colors.cursor", "cursor") => look.cursor = color,
            ("colors.cursor", "text") => look.cursor_text = color,
            ("colors.normal", _) => {
                if let Some(i) = ansi(0) {
                    look.ansi[i] = color;
                }
            }
            ("colors.bright", _) => {
                if let Some(i) = ansi(8) {
                    look.ansi[i] = color;
                }
            }
            _ => {}
        }
    }
    look
}

#[cfg(test)]
mod tests {
    use super::super::tests::{probe, put, workdir};
    use super::*;

    fn hex(c: Option<Color>) -> Option<u32> {
        let b = |f: f32| (f * 255.0).round() as u32;
        c.map(|c| (b(c.r) << 16) | (b(c.g) << 8) | b(c.b))
    }

    fn ansi(look: &TerminalLook) -> Vec<Option<u32>> {
        look.ansi.iter().map(|c| hex(*c)).collect()
    }

    /// The VGA palette a classic black terminal profile sets.
    const VGA: [u32; 16] = [
        0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xC0C0C0, 0x808080,
        0xFF0000, 0x00FF00, 0xFFFF00, 0x0000FF, 0xFF00FF, 0x00FFFF, 0xFFFFFF,
    ];

    fn vga() -> Vec<Option<u32>> {
        VGA.iter().map(|c| Some(*c)).collect()
    }

    const GNOME_ID: &str = "b1dcc9dd-5262-4d8d-a863-c897e6d979b9";

    fn gnome_key(key: &str) -> String {
        format!(
            "gsettings get org.gnome.Terminal.Legacy.Profile:/org/gnome/terminal/legacy/profiles:/:{GNOME_ID}/ {key}"
        )
    }

    /// A gnome-terminal default profile as gsettings prints it: black, silver text, the VGA
    /// palette, bold in the text colour, no cursor colours of its own.
    fn black_vga_profile() -> Vec<(String, String)> {
        let palette = format!(
            "[{}]",
            VGA.iter()
                .map(|c| format!("'#{c:06X}'"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        vec![
            (
                "gsettings get org.gnome.Terminal.ProfilesList default".into(),
                format!("'{GNOME_ID}'"),
            ),
            (gnome_key("use-theme-colors"), "false".into()),
            (gnome_key("background-color"), "'#000000'".into()),
            (gnome_key("foreground-color"), "'#C0C0C0'".into()),
            (gnome_key("palette"), palette),
            (gnome_key("bold-color-same-as-fg"), "true".into()),
            (gnome_key("bold-color"), "'#000000'".into()),
            (gnome_key("cursor-colors-set"), "false".into()),
            (gnome_key("cursor-background-color"), "'#000000'".into()),
            (gnome_key("cursor-foreground-color"), "'#ffffff'".into()),
        ]
    }

    fn answers(pairs: &[(String, String)]) -> Vec<(&str, &str)> {
        pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    #[test]
    fn gnome_terminal_profile_black_with_the_vga_palette() {
        let root = workdir();
        let pairs = black_vga_profile();
        let look = read(
            &probe("X-Cinnamon", &root, &answers(&pairs)),
            "gnome-terminal",
        );
        assert_eq!(hex(look.background), Some(0x000000));
        assert_eq!(hex(look.foreground), Some(0xC0C0C0));
        assert_eq!(ansi(&look), vga());
        // Bold in the text colour, the cursor as Kyde draws it: neither is set.
        assert_eq!(
            (look.bold, look.cursor, look.cursor_text),
            (None, None, None)
        );
        assert_eq!(look.face, None);
    }

    #[test]
    fn gnome_terminal_bold_and_cursor_colours_when_the_profile_sets_them() {
        let root = workdir();
        let mut pairs = black_vga_profile();
        for (key, value) in [
            ("bold-color-same-as-fg", "false"),
            ("bold-color", "'rgb(255,255,255)'"),
            ("cursor-colors-set", "true"),
            ("cursor-background-color", "'#00FF00'"),
            ("cursor-foreground-color", "'#000000000000'"),
            ("background-color", "'rgba(0,0,0,0.9)'"),
        ] {
            pairs.retain(|(k, _)| *k != gnome_key(key));
            pairs.push((gnome_key(key), value.into()));
        }
        let look = read(&probe("GNOME", &root, &answers(&pairs)), "gnome-terminal");
        assert_eq!(hex(look.background), Some(0x000000));
        assert_eq!(hex(look.bold), Some(0xFFFFFF));
        assert_eq!(hex(look.cursor), Some(0x00FF00));
        assert_eq!(hex(look.cursor_text), Some(0x000000));
    }

    /// `use-theme-colors`: the GTK theme's content colours, the palette still the profile's.
    #[test]
    fn gnome_terminal_on_theme_colours_takes_the_gtk_theme() {
        let root = workdir();
        let mut pairs = black_vga_profile();
        pairs.retain(|(k, _)| *k != gnome_key("use-theme-colors"));
        pairs.push((gnome_key("use-theme-colors"), "true".into()));
        pairs.push((
            "gsettings get org.gnome.desktop.interface gtk-theme".into(),
            "'Mine'".into(),
        ));
        let p = probe("GNOME", &root, &answers(&pairs));
        put(
            &p.data_dirs[0],
            "themes/Mine/gtk-3.0/gtk.css",
            "@define-color theme_base_color #fdf6e3;\n@define-color theme_text_color #657b83;\n",
        );
        let look = read(&p, "gnome-terminal");
        assert_eq!(hex(look.background), Some(0xFDF6E3));
        assert_eq!(hex(look.foreground), Some(0x657B83));
        assert_eq!(ansi(&look), vga());
        // A theme Kyde can't read: Kyde's own text and background.
        let p = probe("GNOME", &root, &answers(&pairs[..pairs.len() - 1]));
        let look = read(&p, "gnome-terminal");
        assert_eq!((look.background, look.foreground), (None, None));
    }

    /// Keys missing or unreadable each fall back alone; no profile at all is Kyde's terminal.
    #[test]
    fn missing_keys_leave_kydes_own_colours() {
        let root = workdir();
        let id = format!("'{GNOME_ID}'");
        let fg = gnome_key("foreground-color");
        let palette = gnome_key("palette");
        let p = probe(
            "GNOME",
            &root,
            &[
                (
                    "gsettings get org.gnome.Terminal.ProfilesList default",
                    id.as_str(),
                ),
                (fg.as_str(), "'#C0C0C0'"),
                // Eight colours and a GTK colour function.
                (
                    palette.as_str(),
                    "['#000000', '#800000', 'shade(#000, 0.5)', '#808000', '#000080', '#800080', '#008080', '#C0C0C0']",
                ),
            ],
        );
        let look = read(&p, "gnome-terminal");
        assert_eq!(look.background, None);
        assert_eq!(hex(look.foreground), Some(0xC0C0C0));
        let mut want = vga()[..8].to_vec();
        want[2] = None;
        want.extend([None; 8]);
        assert_eq!(ansi(&look), want);
        // No gsettings, a terminal Kyde doesn't read, or none: nothing set.
        for terminal in ["gnome-terminal", "mate-terminal", "ptyxis", "xterm", ""] {
            assert_eq!(
                read(&probe("GNOME", &root, &[]), terminal),
                TerminalLook::default(),
                "{terminal}"
            );
        }
    }

    #[test]
    fn mate_terminal_palette_is_one_colon_separated_string() {
        let root = workdir();
        let key = |k: &str| {
            format!(
                "gsettings get org.mate.terminal.profile:/org/mate/terminal/profiles/default/ {k}"
            )
        };
        let (theme, bg, fg, pal) = (
            key("use-theme-colors"),
            key("background-color"),
            key("foreground-color"),
            key("palette"),
        );
        let palette = format!(
            "'{}'",
            VGA.iter()
                .map(|c| {
                    let (r, g, b) = (c >> 16, (c >> 8) & 0xFF, c & 0xFF);
                    format!("#{r:02X}{r:02X}{g:02X}{g:02X}{b:02X}{b:02X}")
                })
                .collect::<Vec<_>>()
                .join(":")
        );
        let p = probe(
            "MATE",
            &root,
            &[
                (
                    "gsettings get org.mate.terminal.global default-profile",
                    "'default'",
                ),
                (theme.as_str(), "false"),
                (bg.as_str(), "'#000000000000'"),
                (fg.as_str(), "'#C0C0C0C0C0C0'"),
                (pal.as_str(), palette.as_str()),
            ],
        );
        let look = read(&p, "mate-terminal");
        assert_eq!(hex(look.background), Some(0x000000));
        assert_eq!(hex(look.foreground), Some(0xC0C0C0));
        assert_eq!(ansi(&look), vga());
    }

    #[test]
    fn ptyxis_reads_the_users_own_palette_file() {
        let root = workdir();
        let profile =
            "gsettings get org.gnome.Ptyxis.Profile:/org/gnome/Ptyxis/Profiles/abc/ palette";
        let mut answers = vec![
            (
                "gsettings get org.gnome.Ptyxis default-profile-uuid",
                "'abc'",
            ),
            (profile, "'Mine'"),
        ];
        let p = probe("GNOME", &root, &answers);
        // A palette Ptyxis ships (no file): Kyde's own.
        assert_eq!(read(&p, "ptyxis"), TerminalLook::default());
        let colors: String = (0..16)
            .map(|i| format!("Color{i}=#{:06X}\n", VGA[i]))
            .collect();
        put(
            &p.data_home,
            "org.gnome.Ptyxis/palettes/Mine.palette",
            &format!("[Palette]\nName=Mine\nBackground=#000000\nForeground=#C0C0C0\nCursor=#FFFFFF\n{colors}"),
        );
        let look = read(&p, "ptyxis");
        assert_eq!(hex(look.background), Some(0x000000));
        assert_eq!(hex(look.cursor), Some(0xFFFFFF));
        assert_eq!(ansi(&look), vga());
        // Light and dark variants: the one Ptyxis shows.
        put(
            &p.data_home,
            "org.gnome.Ptyxis/palettes/Mine.palette",
            "[Palette]\nName=Mine\n\n[Light]\nBackground=#FFFFFF\nForeground=#000000\n\n[Dark]\nBackground=#101010\nForeground=#EEEEEE\n",
        );
        answers.push(("gsettings get org.gnome.Ptyxis interface-style", "'dark'"));
        let look = read(&probe("GNOME", &root, &answers), "ptyxis");
        assert_eq!(hex(look.background), Some(0x101010));
        answers.pop();
        answers.push(("gsettings get org.gnome.Ptyxis interface-style", "'system'"));
        let mut light = answers.clone();
        let look = read(&probe("GNOME", &root, &light), "ptyxis");
        assert_eq!(hex(look.background), Some(0xFFFFFF));
        light.push((
            "gsettings get org.gnome.desktop.interface color-scheme",
            "'prefer-dark'",
        ));
        let look = read(&probe("GNOME", &root, &light), "ptyxis");
        assert_eq!(hex(look.background), Some(0x101010));
    }

    #[test]
    fn xfce4_terminal_from_xfconf_else_terminalrc() {
        let root = workdir();
        let p = probe("XFCE", &root, &[]);
        let palette = VGA
            .iter()
            .map(|c| format!("rgb({},{},{})", c >> 16, (c >> 8) & 0xFF, c & 0xFF))
            .collect::<Vec<_>>()
            .join(";");
        put(
            &p.config_home,
            "xfce4/terminal/terminalrc",
            &format!(
                "[Configuration]\nColorForeground=#C0C0C0\nColorBackground=#000000\nColorPalette={palette}\n\
                 ColorBoldUseDefault=FALSE\nColorBold=#FFFFFF\nColorCursor=#00FF00\n"
            ),
        );
        let look = read(&p, "xfce4-terminal");
        assert_eq!(hex(look.background), Some(0x000000));
        assert_eq!(hex(look.foreground), Some(0xC0C0C0));
        assert_eq!(ansi(&look), vga());
        assert_eq!(hex(look.bold), Some(0xFFFFFF));
        // The cursor keeps its default unless that's switched off.
        assert_eq!(look.cursor, None);

        // xfce4-terminal 1.1+: the xfconf channel, and the old file is no longer read.
        let p = probe(
            "XFCE",
            &root,
            &[
                (
                    "xfconf-query -c xfce4-terminal -l",
                    "/color-background\n/color-use-theme",
                ),
                (
                    "xfconf-query -c xfce4-terminal -p /color-background",
                    "#300A24",
                ),
                (
                    "xfconf-query -c xfce4-terminal -p /color-use-theme",
                    "false",
                ),
                (
                    "xfconf-query -c xfce4-terminal -p /color-cursor-use-default",
                    "false",
                ),
                (
                    "xfconf-query -c xfce4-terminal -p /color-cursor",
                    "rgb(255,0,255)",
                ),
            ],
        );
        put(
            &p.config_home,
            "xfce4/terminal/terminalrc",
            "[Configuration]\nColorForeground=#C0C0C0\n",
        );
        let look = read(&p, "xfce4-terminal");
        assert_eq!(hex(look.background), Some(0x300A24));
        assert_eq!(look.foreground, None);
        assert_eq!(hex(look.cursor), Some(0xFF00FF));
    }

    #[test]
    fn konsole_reads_the_profiles_colour_scheme() {
        let root = workdir();
        let p = probe("KDE", &root, &[]);
        put(
            &p.config_home,
            "konsolerc",
            "[Desktop Entry]\nDefaultProfile=Mine.profile\n",
        );
        put(
            &p.data_home,
            "konsole/Mine.profile",
            "[Appearance]\nColorScheme=BlackOnWhite\n\n[Cursor Options]\nUseCustomCursorColor=true\nCustomCursorColor=255,0,0\nCustomCursorTextColor=255,255,255\n",
        );
        let mut scheme = String::from(
            "[Background]\nColor=0,0,0\n\n[BackgroundIntense]\nColor=40,40,40\n\n[Foreground]\nColor=192,192,192\n\n",
        );
        for i in 0..8 {
            let rgb = |c: u32| format!("{},{},{}", c >> 16, (c >> 8) & 0xFF, c & 0xFF);
            scheme += &format!(
                "[Color{i}]\nColor={}\n\n[Color{i}Intense]\nColor={}\n\n[Color{i}Faint]\nColor=1,2,3\n\n",
                rgb(VGA[i]),
                rgb(VGA[i + 8])
            );
        }
        // The user's scheme, found before the system's.
        put(&p.data_home, "konsole/BlackOnWhite.colorscheme", &scheme);
        put(
            &p.data_dirs[0],
            "konsole/BlackOnWhite.colorscheme",
            "[Background]\nColor=255,255,255\n",
        );
        let look = read(&p, "konsole");
        assert_eq!(hex(look.background), Some(0x000000));
        assert_eq!(hex(look.foreground), Some(0xC0C0C0));
        assert_eq!(ansi(&look), vga());
        assert_eq!(hex(look.cursor), Some(0xFF0000));
        assert_eq!(hex(look.cursor_text), Some(0xFFFFFF));
        // A scheme that isn't there: Kyde's colours.
        put(
            &p.data_home,
            "konsole/Mine.profile",
            "[Appearance]\nColorScheme=Gone\n",
        );
        assert_eq!(read(&p, "konsole"), TerminalLook::default());
    }

    #[test]
    fn kitty_reads_its_conf_and_the_files_it_includes() {
        let root = workdir();
        let p = probe("GNOME", &root, &[]);
        assert_eq!(read(&p, "kitty"), TerminalLook::default());
        let colors: String = (0..16)
            .map(|i| format!("color{i}   #{:06x}\n", VGA[i]))
            .collect();
        put(
            &p.config_home,
            "kitty/current-theme.conf",
            &format!("# theme\nbackground #000000\nforeground #c0c0c0\n{colors}"),
        );
        put(
            &p.config_home,
            "kitty/kitty.conf",
            "font_size 12\nforeground #ffffff\ninclude current-theme.conf\ncursor #00ff00\ncursor_text_color background\ninclude ./*.conf\n",
        );
        let look = read(&p, "kitty");
        assert_eq!(hex(look.background), Some(0x000000));
        // The included theme's, set after the conf's own.
        assert_eq!(hex(look.foreground), Some(0xC0C0C0));
        assert_eq!(ansi(&look), vga());
        assert_eq!(hex(look.cursor), Some(0x00FF00));
        assert_eq!(look.cursor_text, None);
        // A file that includes itself stops, keeping what it set.
        put(
            &p.config_home,
            "kitty/kitty.conf",
            "background #101010\ninclude kitty.conf\n",
        );
        assert_eq!(hex(read(&p, "kitty").background), Some(0x101010));
    }

    #[test]
    fn alacritty_reads_its_colour_tables() {
        let root = workdir();
        let p = probe("GNOME", &root, &[]);
        assert_eq!(read(&p, "alacritty"), TerminalLook::default());
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        let table = |offset: usize| -> String {
            names
                .iter()
                .enumerate()
                .map(|(i, n)| format!("{n} = \"0x{:06x}\"\n", VGA[i + offset]))
                .collect()
        };
        put(
            &p.config_home,
            "alacritty/alacritty.toml",
            &format!(
                "[font]\nsize = 11\n\n[colors.primary]\nbackground = '#000000' # black\nforeground = \"#c0c0c0\"\n\n\
                 [colors.cursor]\ntext = \"CellBackground\"\ncursor = \"#ffffff\"\n\n\
                 [colors.normal]\n{}\n[colors.bright]\n{}",
                table(0),
                table(8)
            ),
        );
        let look = read(&p, "alacritty");
        assert_eq!(hex(look.background), Some(0x000000));
        assert_eq!(hex(look.foreground), Some(0xC0C0C0));
        assert_eq!(ansi(&look), vga());
        assert_eq!(hex(look.cursor), Some(0xFFFFFF));
        assert_eq!(look.cursor_text, None);
    }

    #[test]
    fn gvariant_string_lists_unquote_each_entry() {
        assert_eq!(
            quoted_strings("['#000000', \"it's\", 'a\\'b', 'rgb(1,2,3)']"),
            ["#000000", "it's", "a'b", "rgb(1,2,3)"]
        );
        assert!(quoted_strings("@as []").is_empty());
        assert_eq!(palette("@as []"), [None; 16]);
    }
}
