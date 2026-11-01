//! "Match desktop" (Settings → Appearance): read the Linux desktop's fonts and colours so
//! Kyde can draw with them.
//!
//! Three fonts — the terminal's (from the user's own terminal app, the one Open in Terminal
//! starts), the code font (the desktop's monospace font) and the UI font — and a palette from
//! the desktop's dark preference, accent and GTK theme or KDE colour scheme. Every read falls
//! back on its own: a desktop Kyde doesn't know, a missing tool (`gsettings`, `xfconf-query`,
//! `fc-list`), an unreadable file, or a font gpui can't draw leaves just that piece on Kyde's
//! own default. Nothing here fails or panics; [`read`] runs on a background thread.
//!
//! Pure + unit-tested apart from [`Probe::here`], which reads the environment.

mod colors;
mod fonts;

use super::terminal_app::{find_on, gsettings_string, names_desktop, resolve_here};
use crate::theme;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Runs a tool: `(name, args)` → its standard output, `None` when it's missing or fails.
type Run = Box<dyn Fn(&str, &[&str]) -> Option<String> + Send>;

/// Where [`read`] looks: the desktop, its tools, the user's terminal and the XDG folders.
pub(crate) struct Probe {
    run: Run,
    /// `PATH`, to tell which terminals are installed.
    path: OsString,
    desktop: Desktop,
    /// The terminal Open in Terminal starts.
    terminal: Option<PathBuf>,
    /// `XDG_CURRENT_DESKTOP`, lower-cased (names `xdg-terminal-exec`'s per-desktop lists).
    desktops: String,
    home: PathBuf,
    config_home: PathBuf,
    config_dirs: Vec<PathBuf>,
    data_home: PathBuf,
    data_dirs: Vec<PathBuf>,
    /// gpui draws through Wayland (it does whenever `WAYLAND_DISPLAY` is set).
    wayland: bool,
}

impl Probe {
    /// The probe for this session, from the environment.
    pub(crate) fn here() -> Self {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        let home = var("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let dirs = |k: &str, default: &str| -> Vec<PathBuf> {
            std::env::split_paths(&var(k).unwrap_or_else(|| default.into())).collect()
        };
        let path = var("PATH").unwrap_or_default();
        let tools = path.clone();
        let desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        Self {
            run: Box::new(move |tool, args| run_tool(&tools, tool, args)),
            path,
            desktop: Desktop::named(&desktops),
            terminal: resolve_here(),
            desktops: desktops.to_lowercase(),
            config_home: var("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from),
            config_dirs: dirs("XDG_CONFIG_DIRS", "/etc/xdg"),
            data_home: var("XDG_DATA_HOME")
                .map_or_else(|| home.join(".local/share"), PathBuf::from),
            data_dirs: dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share"),
            home,
            wayland: var("WAYLAND_DISPLAY").is_some(),
        }
    }

    fn run(&self, tool: &str, args: &[&str]) -> Option<String> {
        (self.run)(tool, args)
    }

    /// A gsettings key's value (strings unquoted); `None` when gsettings, the schema or the
    /// key is missing.
    fn gsettings(&self, schema: &str, key: &str) -> Option<String> {
        gsettings_string(&self.run("gsettings", &["get", schema, key])?)
    }

    /// An xfconf property's value; `None` when xfconf or the property is missing.
    fn xfconf(&self, channel: &str, property: &str) -> Option<String> {
        let v = self.run("xfconf-query", &["-c", channel, "-p", property])?;
        let v = v.trim();
        (!v.is_empty()).then(|| v.to_string())
    }

    /// The first readable `rel` under the user's config folder, then the system ones.
    fn config_file(&self, rel: &str) -> Option<String> {
        std::iter::once(&self.config_home)
            .chain(&self.config_dirs)
            .find_map(|d| std::fs::read_to_string(d.join(rel)).ok())
    }

    /// The first readable `rel` under the user's data folder, then the system ones.
    fn data_file(&self, rel: &str) -> Option<String> {
        std::iter::once(&self.data_home)
            .chain(&self.data_dirs)
            .find_map(|d| std::fs::read_to_string(d.join(rel)).ok())
    }

    /// `key` in `[section]` of GTK 3's `settings.ini`, which GTK reads on any desktop without
    /// a settings daemon of its own.
    fn gtk_setting(&self, key: &str) -> Option<String> {
        ini_value(&self.config_file("gtk-3.0/settings.ini")?, "Settings", key)
    }

    /// `key` in `[section]` of KDE's `kdeglobals`, the user's file before the system's.
    fn kde_setting(&self, section: &str, key: &str) -> Option<String> {
        std::iter::once(&self.config_home)
            .chain(&self.config_dirs)
            .filter_map(|d| std::fs::read_to_string(d.join("kdeglobals")).ok())
            .find_map(|text| ini_value(&text, section, key))
    }
}

/// `tool args…` run from `path`; its standard output when it ran and succeeded.
fn run_tool(path: &OsString, tool: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(find_on(tool, path)?)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The desktops whose settings Kyde knows where to find.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Desktop {
    Cinnamon,
    /// GNOME and the desktops that keep their settings in GNOME's schemas (Budgie, Unity,
    /// Pantheon).
    Gnome,
    Mate,
    Kde,
    Xfce,
    Other,
}

impl Desktop {
    /// The desktop `XDG_CURRENT_DESKTOP` names (e.g. `X-Cinnamon`, `ubuntu:GNOME`).
    fn named(desktops: &str) -> Self {
        let is = |name: &str| names_desktop(desktops, name);
        if is("Cinnamon") {
            Self::Cinnamon
        } else if is("MATE") {
            Self::Mate
        } else if is("KDE") {
            Self::Kde
        } else if is("XFCE") {
            Self::Xfce
        } else if ["GNOME", "Unity", "Budgie", "Pantheon"]
            .iter()
            .any(|d| is(d))
        {
            Self::Gnome
        } else {
            Self::Other
        }
    }

    /// The gsettings schema holding the desktop's own font + GTK theme settings.
    fn interface_schema(self) -> Option<&'static str> {
        match self {
            Self::Cinnamon => Some("org.cinnamon.desktop.interface"),
            Self::Gnome => Some("org.gnome.desktop.interface"),
            Self::Mate => Some("org.mate.interface"),
            Self::Kde | Self::Xfce | Self::Other => None,
        }
    }
}

/// What Kyde draws with while Match desktop is on: what [`read`] could read and gpui can draw,
/// each piece `None` (Kyde's own) otherwise. `gpui_family` gives the spelling of a family
/// gpui's text system has, `None` for one it doesn't.
pub(crate) fn read(p: &Probe, gpui_family: &dyn Fn(&str) -> Option<String>) -> theme::Overlay {
    let scale = fonts::text_scale(p);
    let face = |spec: fonts::FontSpec| fonts::face(p, spec, scale, gpui_family);
    // No monospace setting at all: fontconfig's `monospace`, at Kyde's size.
    let editor = match fonts::monospace(p) {
        Some(spec) => face(spec),
        None => face(fonts::FontSpec::family("monospace")),
    };
    // A terminal on the system monospace font, or one Kyde can't read: the code font.
    let terminal = match fonts::terminal(p) {
        Some(spec) => face(spec),
        None => editor,
    };
    theme::Overlay {
        palette: colors::palette(&colors::read(p)),
        ui: fonts::ui(p).and_then(face),
        editor,
        terminal,
    }
}

/// `key`'s value in `[section]` of an INI-style file (GTK `settings.ini`, KDE config files).
/// KDE's `[$e]`-style flags on a key are ignored; localised keys (`Name[de]`) aren't `key`.
fn ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            inside = name == section;
            continue;
        }
        let Some((k, v)) = line.split_once('=').filter(|_| inside) else {
            continue;
        };
        let k = k.trim();
        let bare = k
            .find("[$")
            .filter(|_| k.ends_with(']'))
            .map_or(k, |at| &k[..at]);
        if bare == key {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// `s` without one pair of surrounding `"` or `'`.
fn unquote(s: &str) -> &str {
    let s = s.trim();
    ['"', '\'']
        .into_iter()
        .find_map(|q| s.strip_prefix(q).and_then(|v| v.strip_suffix(q)))
        .unwrap_or(s)
}

/// File name of `p`, or `""`.
fn file_name(p: &Path) -> &str {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// A temp dir, removed when dropped.
    pub(super) struct Workdir(PathBuf);

    impl std::ops::Deref for Workdir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Workdir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A fresh, unique temp dir.
    pub(super) fn workdir() -> Workdir {
        let d = std::env::temp_dir().join(format!(
            "kyde-desktop-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        Workdir(d)
    }

    /// Write `text` to `root/rel`, making its folders.
    pub(super) fn put(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// A probe on `desktop` whose tools answer from `answers` (`"tool arg arg"` → output; any
    /// other call fails, as a missing tool or key does), with every XDG folder under `root`.
    pub(super) fn probe(desktop: &str, root: &Path, answers: &[(&str, &str)]) -> Probe {
        let answers: HashMap<String, String> = answers
            .iter()
            .map(|(k, v)| ((*k).to_string(), format!("{v}\n")))
            .collect();
        Probe {
            run: Box::new(move |tool, args| {
                answers
                    .get(
                        &std::iter::once(tool)
                            .chain(args.iter().copied())
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                    .cloned()
            }),
            path: root.join("bin").into_os_string(),
            desktop: Desktop::named(desktop),
            terminal: None,
            desktops: desktop.to_lowercase(),
            home: root.join("home"),
            config_home: root.join("config"),
            config_dirs: vec![root.join("etc-xdg")],
            data_home: root.join("data"),
            data_dirs: vec![root.join("usr-share")],
            wayland: false,
        }
    }

    /// gpui's text system with these families.
    pub(super) fn gpui_with(families: &'static [&'static str]) -> impl Fn(&str) -> Option<String> {
        move |f: &str| {
            families
                .iter()
                .find(|n| n.eq_ignore_ascii_case(f))
                .map(|n| (*n).to_string())
        }
    }

    #[test]
    fn the_desktop_comes_from_xdg_current_desktop() {
        assert_eq!(Desktop::named("X-Cinnamon"), Desktop::Cinnamon);
        assert_eq!(Desktop::named("ubuntu:GNOME"), Desktop::Gnome);
        assert_eq!(Desktop::named("Budgie:GNOME"), Desktop::Gnome);
        assert_eq!(Desktop::named("MATE"), Desktop::Mate);
        assert_eq!(Desktop::named("KDE"), Desktop::Kde);
        assert_eq!(Desktop::named("XFCE"), Desktop::Xfce);
        assert_eq!(Desktop::named("sway"), Desktop::Other);
        assert_eq!(Desktop::named(""), Desktop::Other);
    }

    #[test]
    fn ini_values_by_section_and_key() {
        let text = "[General]\nfont=Noto Sans,10\nfixed[$e]=Hack,10\nName[de]=x\n\n[Colors:View]\nBackgroundNormal = 255,255,255\n";
        assert_eq!(
            ini_value(text, "General", "font").as_deref(),
            Some("Noto Sans,10")
        );
        assert_eq!(
            ini_value(text, "General", "fixed").as_deref(),
            Some("Hack,10")
        );
        assert_eq!(ini_value(text, "General", "Name"), None);
        assert_eq!(
            ini_value(text, "Colors:View", "BackgroundNormal").as_deref(),
            Some("255,255,255")
        );
        assert_eq!(ini_value(text, "Colors:View", "font"), None);
        assert_eq!(ini_value("", "General", "font"), None);
    }

    /// No desktop Kyde knows, no tools, no files: nothing to lay over Kyde's own look.
    #[test]
    fn nothing_readable_leaves_kydes_own_look() {
        let root = workdir();
        for desktop in ["", "X-Cinnamon", "GNOME", "KDE", "XFCE", "MATE"] {
            let o = read(
                &probe(desktop, &root, &[]),
                &gpui_with(&["DejaVu Sans Mono"]),
            );
            assert!(o.palette.is_none(), "{desktop}");
            assert_eq!(
                (o.ui, o.editor, o.terminal),
                (None, None, None),
                "{desktop}"
            );
        }
    }

    /// Cinnamon as on a stock box with a GTK 3 theme: gnome-terminal's own profile font for
    /// the terminal, the desktop monospace font for code, a bitmap UI font left to Kyde, and
    /// the theme's colours.
    #[test]
    fn cinnamon_with_gnome_terminal_and_a_bitmap_ui_font() {
        let root = workdir();
        // gnome-terminal's stock default profile.
        let id = "b1dcc9dd-5262-4d8d-a863-c897e6d979b9";
        let default = format!("'{id}'");
        let profile = format!(
            "org.gnome.Terminal.Legacy.Profile:/org/gnome/terminal/legacy/profiles:/:{id}/"
        );
        let use_system = format!("gsettings get {profile} use-system-font");
        let font = format!("gsettings get {profile} font");
        let fonts_of = |f: &str| format!("fc-list -f %{{fontformat}}\n {f}:scalable=true");
        let (noto, dejavu, retro) = (
            fonts_of("Noto Sans Mono"),
            fonts_of("DejaVu Sans Mono"),
            fonts_of("Retro Sans"),
        );
        let answers = [
            (
                "gsettings get org.cinnamon.desktop.interface font-name",
                "'Retro Sans 8'",
            ),
            (
                "gsettings get org.gnome.desktop.interface monospace-font-name",
                "'Noto Sans Mono 11'",
            ),
            (
                "gsettings get org.cinnamon.desktop.interface gtk-theme",
                "'Retro'",
            ),
            (
                "gsettings get org.x.apps.portal color-scheme",
                "'prefer-light'",
            ),
            (
                "gsettings get org.gnome.Terminal.ProfilesList default",
                default.as_str(),
            ),
            (use_system.as_str(), "false"),
            (font.as_str(), "'DejaVu Sans Mono 10'"),
            (noto.as_str(), "TrueType\nTrueType"),
            (dejavu.as_str(), "TrueType"),
            // Installed, but only as a Windows bitmap font: none scalable.
            (retro.as_str(), ""),
        ];
        let mut p = probe("X-Cinnamon", &root, &answers);
        p.terminal = Some(PathBuf::from("/usr/bin/gnome-terminal"));
        put(
            &p.home,
            ".themes/Retro/gtk-3.0/gtk.css",
            "@define-color bg_color #c0c0c0;\n@define-color theme_bg_color @bg_color;\n\
             @define-color theme_fg_color #000000;\n@define-color theme_base_color #ffffff;\n\
             @define-color theme_text_color #000000;\n@define-color theme_selected_bg_color #000080;\n\
             @define-color theme_selected_fg_color #ffffff;\n",
        );
        let o = read(&p, &gpui_with(&["DejaVu Sans Mono", "Noto Sans Mono"]));
        let px = |pt: f32| pt * 96.0 / 72.0;
        assert_eq!(
            o.terminal,
            Some(theme::font::Face::new("DejaVu Sans Mono", Some(px(10.0))))
        );
        assert_eq!(
            o.editor,
            Some(theme::font::Face::new("Noto Sans Mono", Some(px(11.0))))
        );
        assert_eq!(o.ui, None);
        let t = o.palette.expect("the GTK theme's colours");
        let hex = |c: kyde_color::Color| {
            let b = |f: f32| (f * 255.0).round() as u32;
            (b(c.r) << 16) | (b(c.g) << 8) | b(c.b)
        };
        assert_eq!(hex(t.frame_bg), 0xC0C0C0);
        assert_eq!(hex(t.main_bg), 0xFFFFFF);
        assert_eq!(hex(t.text), 0x000000);
        assert_eq!(hex(t.primary), 0x000080);
        // Syntax stays Kyde Light's: the theme's background is light.
        assert_eq!(hex(t.syn_keyword), hex(theme::Theme::light().syn_keyword));
    }
}
