//! Start without XIM where all it brings is a crash (Linux, X11).
//!
//! On X11 gpui 0.2.2 talks to the input method over XIM. Its pre-edit decoder reads an
//! unescaped compound-text string as UTF-8, but unescaped compound text is Latin-1, so the
//! dead-key mark ibus shows while it waits for the next letter (´, ¨, ¸ or the grave accent;
//! `AltGr+=` on a UK layout) panics the whole app. When ibus only passes plain keyboard layouts
//! through (every configured engine or input source is an xkb layout), XIM adds nothing: gpui
//! composes dead keys itself with xkb's compose tables. So Kyde starts without it there and puts
//! `XMODIFIERS` back as soon as gpui has looked, which keeps the input method for the terminal
//! and the apps Kyde opens. A real input method (pinyin, mozc, hangul…) keeps XIM: its
//! pre-edit is escaped UTF-8, which decodes.

use std::ffi::OsString;
use std::process::{Command, Stdio};

/// The `XMODIFIERS` value held back while gpui starts; [`restore`] gives it back.
pub(crate) struct Held(Option<OsString>);

/// Clear `XMODIFIERS` if it names ibus and ibus only carries plain layouts (see the module
/// docs). Call before the app starts any thread: it changes the process environment.
pub(crate) fn hold_back_xim() -> Held {
    // gpui only uses XIM on X11; a Wayland session needs nothing here.
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return Held(None);
    }
    let Some(value) = std::env::var_os("XMODIFIERS") else {
        return Held(None);
    };
    let ibus = value.to_str().is_some_and(|v| v.trim() == "@im=ibus");
    if !ibus
        || !plain_layouts_only(
            gsettings("org.freedesktop.ibus.general", "preload-engines").as_deref(),
            gsettings("org.gnome.desktop.input-sources", "sources").as_deref(),
        )
    {
        return Held(None);
    }
    std::env::remove_var("XMODIFIERS");
    Held(Some(value))
}

/// Put back what [`hold_back_xim`] cleared, once gpui has connected (or not) to the input method.
pub(crate) fn restore(held: Held) {
    if let Some(value) = held.0 {
        std::env::set_var("XMODIFIERS", value);
    }
}

/// `true` only when something lists the user's input sources and every one is a plain layout:
/// ibus's engines (`['xkb:gb::eng']`, printed by gsettings) and GNOME's input sources
/// (`[('xkb', 'gb'), ('ibus', 'mozc-jp')]`). Unknown or empty means keep XIM.
fn plain_layouts_only(ibus_engines: Option<&str>, gnome_sources: Option<&str>) -> bool {
    let engines: Vec<&str> = ibus_engines.map(quoted).unwrap_or_default();
    let sources: Vec<&str> = gnome_sources.map(quoted).unwrap_or_default();
    // GNOME's sources come as (kind, id) pairs: the kinds are every other quoted string.
    let kinds: Vec<&str> = sources.iter().step_by(2).copied().collect();
    if engines.is_empty() && kinds.is_empty() {
        return false;
    }
    engines.iter().all(|e| e.starts_with("xkb:")) && kinds.iter().all(|k| *k == "xkb")
}

/// The single-quoted strings in a value gsettings printed.
fn quoted(printed: &str) -> Vec<&str> {
    printed.split('\'').skip(1).step_by(2).collect()
}

fn gsettings(schema: &str, key: &str) -> Option<String> {
    let out = Command::new("gsettings")
        .args(["get", schema, key])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_layouts_drop_xim() {
        assert!(plain_layouts_only(Some("['xkb:gb::eng']\n"), None));
        assert!(plain_layouts_only(
            Some("['xkb:us::eng', 'xkb:de::ger']"),
            Some("[('xkb', 'us'), ('xkb', 'de')]")
        ));
        assert!(plain_layouts_only(Some("@as []"), Some("[('xkb', 'gb')]")));
        // a real input method anywhere keeps XIM
        assert!(!plain_layouts_only(
            Some("['xkb:us::eng', 'libpinyin']"),
            None
        ));
        assert!(!plain_layouts_only(
            None,
            Some("[('xkb', 'jp'), ('ibus', 'mozc-jp')]")
        ));
        // nothing known keeps XIM
        assert!(!plain_layouts_only(Some("@as []"), Some("@a(ss) []")));
        assert!(!plain_layouts_only(None, None));
    }
}
