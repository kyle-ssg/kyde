//! Text → "Your desktop's" (Settings → Appearance, Linux): draw glyphs as the desktop does,
//! antialiased or not and with its hinting strength, instead of Kyde's own smooth text.
//!
//! Each value comes from the first of these that has it:
//! 1. the X resources `Xft.antialias`, `Xft.hinting` and `Xft.hintstyle`, which the desktop's
//!    settings daemon publishes for X11 programs (Cinnamon, GNOME, MATE, Xfce and KDE all do);
//! 2. on Wayland, or without those resources, the desktop's gsettings: Cinnamon's xsettings
//!    plugin keys, MATE's font-rendering keys, then GNOME's `font-antialiasing` /
//!    `font-hinting` (what GTK reads there);
//! 3. fontconfig's resolved default (`fc-match`).
//!
//! A value none of them has stays Kyde's own: antialiased, Kyde's hinting.
//!
//! gpui keeps every glyph it draws for the whole run and has no way to drop them, so the
//! rendering is chosen once, before the first glyph ([`choose_at_start`]), through Kyde's
//! cosmic-text patch (`vendor/README.md`). A change in Settings takes effect on the next start.

use super::{Desktop, Probe};
use cosmic_text::{GlyphHinting, GlyphRendering};
use std::sync::OnceLock;

/// Where Kyde read the desktop's text rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The X resources the desktop publishes (`Xft.*`).
    XResources,
    /// A desktop's gsettings keys: the desktop's name.
    Gsettings(&'static str),
    /// fontconfig's resolved default.
    Fontconfig,
}

/// How the desktop draws text, and where Kyde first read it: `None` when it read nothing, so
/// the rendering is Kyde's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DesktopText {
    pub(crate) rendering: GlyphRendering,
    pub(crate) source: Option<Source>,
}

impl DesktopText {
    /// One line for under the Text choice.
    pub(crate) fn describe(self) -> String {
        let Some(source) = self.source else {
            return "Kyde can't read how your desktop draws text, so it stays Kyde's.".to_string();
        };
        let antialias = if self.rendering.antialias {
            "Antialiased"
        } else {
            "Antialiasing off"
        };
        let hinting = match self.rendering.hinting {
            GlyphHinting::Default => "Kyde's hinting",
            GlyphHinting::None => "no hinting",
            GlyphHinting::Slight => "slight hinting",
            GlyphHinting::Medium => "medium hinting",
            GlyphHinting::Full => "full hinting",
        };
        let from = match source {
            Source::XResources => "its X resources".to_string(),
            Source::Gsettings(desktop) => format!("{desktop}'s settings"),
            Source::Fontconfig => "fontconfig".to_string(),
        };
        format!("{antialias}, {hinting}: how your desktop draws text, read from {from}.")
    }
}

/// What one place says: each value `None` where it says nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Found {
    antialias: Option<bool>,
    hinting: Option<GlyphHinting>,
}

impl Found {
    fn is_empty(self) -> bool {
        self.antialias.is_none() && self.hinting.is_none()
    }

    fn is_complete(self) -> bool {
        self.antialias.is_some() && self.hinting.is_some()
    }

    /// `self`, with what it lacks taken from `later`.
    fn or(self, later: Found) -> Found {
        Found {
            antialias: self.antialias.or(later.antialias),
            hinting: self.hinting.or(later.hinting),
        }
    }
}

/// The desktop's text rendering. `xresources` is the X server's resource database (the root
/// window's `RESOURCE_MANAGER` text), `None` on Wayland or without one.
pub(crate) fn read(p: &Probe, xresources: Option<&str>) -> DesktopText {
    let mut found = Found::default();
    let mut source = None;
    let mut take = |f: Found, from: Source| {
        if source.is_none() && !f.is_empty() {
            source = Some(from);
        }
        found = found.or(f);
        found.is_complete()
    };
    let done = xresources.is_some_and(|db| take(from_xresources(db), Source::XResources))
        || gsettings_keys(p.desktop)
            .iter()
            .any(|&(desktop, schema, aa, hint)| {
                take(
                    from_gsettings(
                        p.gsettings(schema, aa).as_deref(),
                        p.gsettings(schema, hint).as_deref(),
                    ),
                    Source::Gsettings(desktop),
                )
            });
    if !done {
        if let Some(out) = p.run("fc-match", &["-f", FC_FORMAT]) {
            take(from_fontconfig(&out), Source::Fontconfig);
        }
    }
    DesktopText {
        rendering: GlyphRendering {
            antialias: found.antialias.unwrap_or(true),
            hinting: found.hinting.unwrap_or_default(),
        },
        source,
    }
}

/// [`read`] for this session (a [`super::read`]-style reader for Settings, which shows what it
/// found). gpui's font list plays no part.
pub(crate) fn read_here(p: &Probe, _gpui_family: &dyn Fn(&str) -> Option<String>) -> DesktopText {
    read(p, xresources_here(p).as_deref())
}

/// What [`choose_at_start`] chose for this run: `None` for Kyde's own text.
static AT_START: OnceLock<Option<DesktopText>> = OnceLock::new();

/// Call before gpui draws any text: with Text set to Your desktop's (`on`), read the desktop and
/// draw every glyph of this run its way. Only the first call counts.
pub(crate) fn choose_at_start(on: bool) {
    AT_START.get_or_init(|| {
        let text = on.then(|| {
            let p = Probe::here_without_terminal();
            read(&p, xresources_here(&p).as_deref())
        });
        if let Some(text) = text {
            cosmic_text::set_glyph_rendering(text.rendering);
        }
        text
    });
}

/// The desktop text this run draws with: `None` for Kyde's own.
pub(crate) fn at_start() -> Option<DesktopText> {
    AT_START.get().copied().flatten()
}

/// The X server's resource database, read over a short-lived connection of its own; `None` on
/// Wayland (where gpui doesn't draw through X), with no X server, or with no database.
fn xresources_here(p: &Probe) -> Option<String> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
    if p.wayland {
        return None;
    }
    let (conn, _) = x11rb::connect(None).ok()?;
    // Xlib's XResourceManagerString: the first screen's root window.
    let root = conn.setup().roots.first()?.root;
    let reply = conn
        .get_property(
            false,
            root,
            AtomEnum::RESOURCE_MANAGER,
            AtomEnum::STRING,
            0,
            u32::MAX / 4,
        )
        .ok()?
        .reply()
        .ok()?;
    (!reply.value.is_empty()).then(|| String::from_utf8_lossy(&reply.value).into_owned())
}

/// What the X resource database says: Xft's own reading of `Xft.antialias`, `Xft.hinting` and
/// `Xft.hintstyle` (or `Xft*…`, else `*…`). `Xft.hinting: false` is no hinting whatever the
/// style.
fn from_xresources(db: &str) -> Found {
    let value = |name: &str| {
        let mut wildcard = None;
        for line in db.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key
                .strip_prefix("Xft.")
                .or_else(|| key.strip_prefix("Xft*"))
            {
                Some(k) if k == name => return Some(value),
                _ if key.strip_prefix('*') == Some(name) => wildcard = wildcard.or(Some(value)),
                _ => {}
            }
        }
        wildcard
    };
    let hinting = match value("hinting").and_then(xft_bool) {
        Some(false) => Some(GlyphHinting::None),
        _ => value("hintstyle").and_then(hint_style),
    };
    Found {
        antialias: value("antialias").and_then(xft_bool),
        hinting,
    }
}

/// A boolean as Xft reads one: `1`, `true`, `yes`, `on` (any case), and their opposites.
fn xft_bool(v: &str) -> Option<bool> {
    let v = v.to_ascii_lowercase();
    match v.as_bytes() {
        [b't' | b'y' | b'1', ..] | [b'o', b'n', ..] => Some(true),
        [b'f' | b'n' | b'0', ..] | [b'o', b'f', ..] => Some(false),
        _ => None,
    }
}

/// A fontconfig hint style: its name (`hintslight`) or number (`1`).
fn hint_style(v: &str) -> Option<GlyphHinting> {
    match v.trim() {
        "hintnone" | "0" => Some(GlyphHinting::None),
        "hintslight" | "1" => Some(GlyphHinting::Slight),
        "hintmedium" | "2" => Some(GlyphHinting::Medium),
        "hintfull" | "3" => Some(GlyphHinting::Full),
        _ => None,
    }
}

/// The gsettings keys to read on `desktop`, in order: `(desktop name, schema, antialiasing key,
/// hinting key)`. KDE and Xfce keep their own settings, which reach X resources and fontconfig.
fn gsettings_keys(
    desktop: Desktop,
) -> &'static [(&'static str, &'static str, &'static str, &'static str)] {
    const CINNAMON: (&str, &str, &str, &str) = (
        "Cinnamon",
        "org.cinnamon.settings-daemon.plugins.xsettings",
        "antialiasing",
        "hinting",
    );
    const MATE: (&str, &str, &str, &str) =
        ("MATE", "org.mate.font-rendering", "antialiasing", "hinting");
    const GNOME: (&str, &str, &str, &str) = (
        "GNOME",
        "org.gnome.desktop.interface",
        "font-antialiasing",
        "font-hinting",
    );
    match desktop {
        Desktop::Cinnamon => &[CINNAMON, GNOME],
        Desktop::Mate => &[MATE, GNOME],
        Desktop::Gnome | Desktop::Other => &[GNOME],
        Desktop::Kde | Desktop::Xfce => &[],
    }
}

/// What a desktop's antialiasing (`none`, `grayscale`, `rgba`) and hinting (`none`, `slight`,
/// `medium`, `full`) keys say. Subpixel (`rgba`) is drawn greyscale.
fn from_gsettings(antialiasing: Option<&str>, hinting: Option<&str>) -> Found {
    Found {
        antialias: match antialiasing {
            Some("none") => Some(false),
            Some("grayscale" | "rgba") => Some(true),
            _ => None,
        },
        hinting: match hinting {
            Some("none") => Some(GlyphHinting::None),
            Some("slight") => Some(GlyphHinting::Slight),
            Some("medium") => Some(GlyphHinting::Medium),
            Some("full") => Some(GlyphHinting::Full),
            _ => None,
        },
    }
}

/// `fc-match`'s format for [`from_fontconfig`].
const FC_FORMAT: &str = "%{antialias}|%{hinting}|%{hintstyle}";

/// What fontconfig resolves for the default font, as [`FC_FORMAT`] prints it (`True|True|1`).
fn from_fontconfig(out: &str) -> Found {
    let mut values = out.trim().split('|');
    let mut next = || match values.next().map(str::trim) {
        Some("True") => Some(true),
        Some("False") => Some(false),
        _ => None,
    };
    let antialias = next();
    let hinting = next();
    let style = values.next().and_then(hint_style);
    Found {
        antialias,
        hinting: match hinting {
            Some(false) => Some(GlyphHinting::None),
            _ => style,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{probe, workdir};
    use super::*;
    use cosmic_text::{fontdb, CacheKey, CacheKeyFlags, FontSystem, SwashCache, SwashContent};

    fn found(antialias: Option<bool>, hinting: Option<GlyphHinting>) -> Found {
        Found { antialias, hinting }
    }

    fn text(antialias: bool, hinting: GlyphHinting, source: Source) -> DesktopText {
        DesktopText {
            rendering: GlyphRendering { antialias, hinting },
            source: Some(source),
        }
    }

    /// The `RESOURCE_MANAGER` text the settings daemons write, read as Xft reads it.
    #[test]
    fn x_resources_as_xft_reads_them() {
        // Cinnamon set to no antialiasing and full hinting (its daemon writes these).
        let cinnamon = "Xft.dpi:\t96\nXft.antialias:\t0\nXft.hinting:\t1\nXft.hintstyle:\thintfull\nXft.rgba:\trgb\n";
        assert_eq!(
            from_xresources(cinnamon),
            found(Some(false), Some(GlyphHinting::Full))
        );
        // GNOME's defaults.
        let gnome = "Xft.antialias:\t1\nXft.hinting:\t1\nXft.hintstyle:\thintslight\n";
        assert_eq!(
            from_xresources(gnome),
            found(Some(true), Some(GlyphHinting::Slight))
        );
        // Hinting off wins over the style; numbers, words and wildcards as Xft takes them.
        assert_eq!(
            from_xresources("Xft.hinting: false\nXft.hintstyle: hintfull\n"),
            found(None, Some(GlyphHinting::None))
        );
        assert_eq!(
            from_xresources("Xft*antialias: on\nXft.hintstyle: 2\n"),
            found(Some(true), Some(GlyphHinting::Medium))
        );
        assert_eq!(
            from_xresources("*antialias: no\n*hintstyle: hintnone\nXft.antialias: yes\n"),
            found(Some(true), Some(GlyphHinting::None))
        );
        // Nothing about text, or values Xft wouldn't take.
        assert_eq!(from_xresources("Xft.dpi:\t96\n"), Found::default());
        assert_eq!(
            from_xresources("Xft.antialias: maybe\nXft.hintstyle: hintsome\n"),
            Found::default()
        );
        assert_eq!(from_xresources(""), Found::default());
    }

    /// The gsettings words of Cinnamon, MATE and GNOME.
    #[test]
    fn gsettings_words() {
        assert_eq!(
            from_gsettings(Some("none"), Some("full")),
            found(Some(false), Some(GlyphHinting::Full))
        );
        assert_eq!(
            from_gsettings(Some("grayscale"), Some("slight")),
            found(Some(true), Some(GlyphHinting::Slight))
        );
        assert_eq!(
            from_gsettings(Some("rgba"), Some("medium")),
            found(Some(true), Some(GlyphHinting::Medium))
        );
        assert_eq!(
            from_gsettings(None, Some("none")),
            found(None, Some(GlyphHinting::None))
        );
        assert_eq!(from_gsettings(Some("lcd"), Some("max")), Found::default());
    }

    /// fontconfig's values as `fc-match -f` prints them.
    #[test]
    fn fontconfig_values() {
        assert_eq!(
            from_fontconfig("True|True|1"),
            found(Some(true), Some(GlyphHinting::Slight))
        );
        assert_eq!(
            from_fontconfig("False|True|3\n"),
            found(Some(false), Some(GlyphHinting::Full))
        );
        assert_eq!(
            from_fontconfig("True|False|3"),
            found(Some(true), Some(GlyphHinting::None))
        );
        assert_eq!(from_fontconfig("True||"), found(Some(true), None));
        assert_eq!(from_fontconfig("||"), Found::default());
        assert_eq!(from_fontconfig(""), Found::default());
    }

    fn cinnamon_gsettings() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "gsettings get org.cinnamon.settings-daemon.plugins.xsettings antialiasing",
                "'none'",
            ),
            (
                "gsettings get org.cinnamon.settings-daemon.plugins.xsettings hinting",
                "'full'",
            ),
            (
                "gsettings get org.gnome.desktop.interface font-antialiasing",
                "'grayscale'",
            ),
            (
                "gsettings get org.gnome.desktop.interface font-hinting",
                "'slight'",
            ),
            (
                "fc-match -f %{antialias}|%{hinting}|%{hintstyle}",
                "True|True|1",
            ),
        ]
    }

    /// X resources first; on Wayland (none given) the desktop's own gsettings: Cinnamon's keys
    /// before GNOME's, which the same box also has.
    #[test]
    fn x_resources_then_the_desktops_gsettings() {
        let root = workdir();
        let p = probe("X-Cinnamon", &root, &cinnamon_gsettings());
        assert_eq!(
            read(&p, Some("Xft.antialias:\t1\nXft.hintstyle:\thintmedium\n")),
            text(true, GlyphHinting::Medium, Source::XResources)
        );
        assert_eq!(
            read(&p, None),
            text(false, GlyphHinting::Full, Source::Gsettings("Cinnamon"))
        );
        // The same settings under GNOME: GNOME's keys.
        let p = probe("ubuntu:GNOME", &root, &cinnamon_gsettings());
        assert_eq!(
            read(&p, None),
            text(true, GlyphHinting::Slight, Source::Gsettings("GNOME"))
        );
        // KDE keeps its own settings: fontconfig.
        let p = probe("KDE", &root, &cinnamon_gsettings());
        assert_eq!(
            read(&p, None),
            text(true, GlyphHinting::Slight, Source::Fontconfig)
        );
    }

    /// Each value from the first place that has it.
    #[test]
    fn each_value_from_the_first_place_that_has_it() {
        let root = workdir();
        let p = probe("X-Cinnamon", &root, &cinnamon_gsettings());
        // Xft has the antialiasing only: Cinnamon's keys give the hinting.
        assert_eq!(
            read(&p, Some("Xft.antialias:\t1\nXft.dpi:\t96\n")),
            text(true, GlyphHinting::Full, Source::XResources)
        );
        // Only fontconfig answers.
        let p = probe(
            "sway",
            &root,
            &[(
                "fc-match -f %{antialias}|%{hinting}|%{hintstyle}",
                "False|True|3",
            )],
        );
        assert_eq!(
            read(&p, Some("Xft.dpi:\t96\n")),
            text(false, GlyphHinting::Full, Source::Fontconfig)
        );
    }

    /// Nothing readable: Kyde's own text, exactly (the default rendering draws as before).
    #[test]
    fn nothing_readable_is_kydes_own_text() {
        let root = workdir();
        for desktop in ["", "X-Cinnamon", "GNOME", "KDE", "XFCE", "MATE"] {
            let found = read(&probe(desktop, &root, &[]), None);
            assert_eq!(found.rendering, GlyphRendering::default(), "{desktop}");
            assert_eq!(found.source, None, "{desktop}");
            assert_eq!(
                found.describe(),
                "Kyde can't read how your desktop draws text, so it stays Kyde's."
            );
        }
        assert_eq!(GlyphRendering::default().flags(), CacheKeyFlags::empty());
    }

    #[test]
    fn the_note_says_what_and_where() {
        assert_eq!(
            text(false, GlyphHinting::Full, Source::XResources).describe(),
            "Antialiasing off, full hinting: how your desktop draws text, read from its X resources."
        );
        assert_eq!(
            text(true, GlyphHinting::Slight, Source::Gsettings("GNOME")).describe(),
            "Antialiased, slight hinting: how your desktop draws text, read from GNOME's settings."
        );
    }

    /// Every rendering survives the trip through the cache key's flags; the default sets none.
    #[test]
    fn renderings_ride_in_the_cache_key_flags() {
        for antialias in [true, false] {
            for hinting in [
                GlyphHinting::Default,
                GlyphHinting::None,
                GlyphHinting::Slight,
                GlyphHinting::Medium,
                GlyphHinting::Full,
            ] {
                let r = GlyphRendering { antialias, hinting };
                assert_eq!(GlyphRendering::from_flags(r.flags()), r);
                assert_eq!(
                    r.flags().is_empty(),
                    r == GlyphRendering::default(),
                    "{r:?}"
                );
                assert!(GlyphRendering::FLAGS.contains(r.flags()));
            }
        }
    }

    /// The bundled fonts in a font system of their own (never the machine's).
    fn bundled() -> (FontSystem, Vec<(String, fontdb::ID)>) {
        let mut db = fontdb::Database::new();
        db.load_font_data(include_bytes!("../../../assets/fonts/Inter-Regular.ttf").to_vec());
        db.load_font_data(
            include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf").to_vec(),
        );
        let ids = db
            .faces()
            .map(|f| (f.post_script_name.clone(), f.id))
            .collect();
        (FontSystem::new_with_locale_and_db("en-US".into(), db), ids)
    }

    /// FNV-1a, 64-bit: a hash whose value never changes with Rust's version.
    fn fnv(h: &mut u64, bytes: &[u8]) {
        for b in bytes {
            *h ^= u64::from(*b);
            *h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }

    /// Text = Kyde draws byte for byte as before the cosmic-text patch: the key gpui asks with
    /// (`CacheKeyFlags::empty()`), over both bundled fonts, 20 characters, 5 sizes and gpui's 4
    /// subpixel offsets. The baseline hash and the `g` below came from the crates.io
    /// cosmic-text 0.14.2 (unpatched) with these exact inputs.
    #[test]
    fn kydes_text_draws_as_before() {
        let (mut fs, ids) = bundled();
        let mut cache = SwashCache::new();
        let mut h = 0xcbf2_9ce4_8422_2325_u64;
        let mut g13 = None;
        for (name, id) in &ids {
            let font = fs.get_font(*id).unwrap();
            for ch in "AaBbgjkMWy@#0Oil1&{}".chars() {
                let glyph = font.as_swash().charmap().map(ch);
                for (at, size) in [11.0, 13.0, 14.666_667, 16.0, 26.0].into_iter().enumerate() {
                    for v in 0..4u8 {
                        let pos = (f32::from(v) / 4.0, 0.0);
                        let key = CacheKey::new(*id, glyph, size, pos, CacheKeyFlags::empty()).0;
                        let img = cache.get_image(&mut fs, key).clone().unwrap();
                        let p = img.placement;
                        for x in [p.left, p.top, p.width as i32, p.height as i32] {
                            fnv(&mut h, &x.to_le_bytes());
                        }
                        fnv(&mut h, &[u8::from(img.content == SwashContent::Mask)]);
                        fnv(&mut h, &img.data);
                        if name == "JetBrainsMono-Regular" && ch == 'g' && at == 1 && v == 0 {
                            g13 = Some(img);
                        }
                    }
                }
            }
        }
        // JetBrains Mono's `g` at 13 px, as crates.io cosmic-text 0.14.2 draws it.
        let g13 = g13.unwrap();
        let p = g13.placement;
        assert_eq!((p.left, p.top, p.width, p.height), (1, 8, 6, 11));
        #[rustfmt::skip]
        let before: [u8; 66] = [
            0x0e, 0xaf, 0xf3, 0xb7, 0x9a, 0x98,
            0x85, 0xd7, 0x37, 0x55, 0xe5, 0x98,
            0xc7, 0x66, 0x00, 0x00, 0xac, 0x98,
            0xdb, 0x50, 0x00, 0x00, 0x90, 0x98,
            0xdc, 0x50, 0x00, 0x00, 0x90, 0x98,
            0xcd, 0x5e, 0x00, 0x00, 0xa3, 0x98,
            0x97, 0xc4, 0x11, 0x2c, 0xe6, 0x98,
            0x1c, 0xd8, 0xfe, 0xdf, 0xa8, 0x98,
            0x00, 0x01, 0x1c, 0x05, 0x98, 0x8d,
            0x00, 0x1d, 0x20, 0x39, 0xe3, 0x55,
            0x00, 0xe8, 0xff, 0xed, 0x8d, 0x01,
        ];
        assert_eq!(g13.data, before);
        assert_eq!(h, 0x3eb6_f400_6149_edd6, "800 glyph images, hashed");
    }

    /// Another rendering is a cache entry of its own, unantialiased glyphs are only fully on and
    /// off pixels, and grid-fitted ones sit on whole pixels whatever gpui's subpixel offset.
    #[test]
    fn desktop_renderings_are_their_own_entries() {
        let (mut fs, ids) = bundled();
        let id = ids[1].1;
        let glyph = fs.get_font(id).unwrap().as_swash().charmap().map('g');
        let mut cache = SwashCache::new();
        let mut image = |r: GlyphRendering, x: f32| {
            let key = CacheKey::new(id, glyph, 13.0, (x, 0.0), r.flags()).0;
            cache.get_image(&mut fs, key).clone().unwrap()
        };
        let kyde = image(GlyphRendering::default(), 0.0);
        let crisp = GlyphRendering {
            antialias: false,
            hinting: GlyphHinting::Full,
        };
        let mono = image(crisp, 0.0);
        let slight = image(
            GlyphRendering {
                antialias: true,
                hinting: GlyphHinting::Slight,
            },
            0.0,
        );
        assert!(kyde.data.iter().any(|a| !matches!(a, 0 | 255)));
        assert!(mono.data.iter().all(|a| matches!(a, 0 | 255)));
        assert!(mono.data.contains(&255));
        assert_ne!(mono.data, kyde.data);
        assert!(slight.data.iter().any(|a| !matches!(a, 0 | 255)));
        let at = |i: &cosmic_text::SwashImage| {
            let p = i.placement;
            (p.left, p.top, p.width, p.height, i.data.clone())
        };
        for x in [0.25, 0.5, 0.75] {
            assert_eq!(at(&image(crisp, x)), at(&mono), "{x}");
        }
        // Default, mono and slight at offset 0, and mono at three more offsets.
        assert_eq!(cache.image_cache.len(), 6);
    }

    /// A screen of text drawn crisp, at sizes and offsets enough for thousands of glyph images,
    /// stays far under the budget: the hinting set-up is kept, not redone per glyph. (Loose on
    /// purpose; see CLAUDE.md, "Performance regression tests".)
    #[test]
    fn perf_desktop_text_draws_a_screen_of_glyphs() {
        let (mut fs, ids) = bundled();
        let mut cache = SwashCache::new();
        let crisp = GlyphRendering {
            antialias: false,
            hinting: GlyphHinting::Full,
        };
        let start = std::time::Instant::now();
        let mut n = 0;
        for (_, id) in &ids {
            let font = fs.get_font(*id).unwrap();
            let glyphs: Vec<u16> = (' '..='~')
                .map(|c| font.as_swash().charmap().map(c))
                .collect();
            for size in [11.0, 12.0, 13.0, 14.0, 16.0, 20.0, 26.0] {
                for v in 0..4u8 {
                    for &glyph in &glyphs {
                        let key = CacheKey::new(
                            *id,
                            glyph,
                            size,
                            (f32::from(v) / 4.0, 0.0),
                            crisp.flags(),
                        )
                        .0;
                        assert!(cache.get_image(&mut fs, key).is_some());
                        n += 1;
                    }
                }
            }
        }
        let took = start.elapsed();
        assert!(n > 5000);
        assert!(took.as_secs_f32() < 2.0, "{n} glyph images took {took:?}");
    }
}
