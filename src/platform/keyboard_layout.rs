//! What Shift + a symbol key types on the active keyboard layout, named the way gpui's Linux
//! backend reports it, so a keymap key written with Shift binds on every layout
//! (`keymap::set_layout_shifted`).
//!
//! gpui reports Ctrl+Shift+] as `ctrl-}` on a US layout: the character the key types, Shift
//! dropped for anything that isn't a letter. That character depends on the layout (Shift+2
//! types `"` on a UK layout, Shift+7 types `/` on a German one, the `&` key types `1` on a
//! French one), and a key typing a non-ASCII character is named after what it types on a US
//! layout instead. This module works that out per physical key, for the layout gpui says is
//! active (`App::keyboard_layout`). The configured layouts come from the X server
//! (`_XKB_RULES_NAMES`), GNOME's input sources (Wayland) or `/etc/default/keyboard`.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

use xkbcommon::xkb;

/// The RMLVO names of the configured layouts; several layouts are comma-separated
/// (`layout = "gb,de"`), as xkb takes them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Names {
    pub rules: String,
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: String,
}

/// Shift + key, as gpui names both, for every symbol key of the layout gpui calls `active`
/// (`None` when no layout could be read; the keymap then falls back to a US layout, as gpui
/// itself does).
pub(crate) fn shifted_map(active: &str) -> Option<BTreeMap<String, String>> {
    shifted_for(&configured_names()?, active)
}

/// [`shifted_map`] for given layout names.
pub(crate) fn shifted_for(names: &Names, active: &str) -> Option<BTreeMap<String, String>> {
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = compile(&ctx, names)?;
    let us = compile(
        &ctx,
        &Names {
            layout: "us".into(),
            ..Names::default()
        },
    )?;
    let group = (0..keymap.num_layouts())
        .find(|&g| keymap.layout_get_name(g) == active)
        .unwrap_or(0);
    let shift = 1u32 << keymap.mod_get_index(xkb::MOD_NAME_SHIFT);
    let us_shift = 1u32 << us.mod_get_index(xkb::MOD_NAME_SHIFT);
    let mut own = xkb::State::new(&keymap);
    let mut usa = xkb::State::new(&us);
    let mut map = BTreeMap::new();
    for code in keymap.min_keycode().raw()..=keymap.max_keycode().raw() {
        let key = xkb::Keycode::new(code);
        own.update_mask(0, 0, 0, 0, 0, group);
        if own.key_get_one_sym(key).is_keypad_key() {
            continue;
        }
        let base = reported(&typed(&mut own, key, 0, group), &typed(&mut usa, key, 0, 0));
        let shifted = reported(
            &typed(&mut own, key, shift, group),
            &typed(&mut usa, key, us_shift, 0),
        );
        if let (Some(base), Some(shifted)) = (base, shifted) {
            let letter = |s: &str| s.chars().any(|c| c.is_ascii_alphabetic());
            if base != shifted && !letter(&base) && !letter(&shifted) {
                map.entry(base).or_insert(shifted);
            }
        }
    }
    Some(map)
}

/// The text `key` types with modifiers `mods` in layout `group`.
fn typed(state: &mut xkb::State, key: xkb::Keycode, mods: u32, group: u32) -> String {
    state.update_mask(mods, 0, 0, 0, 0, group);
    state.key_get_utf8(key)
}

/// gpui's name for a key that types `own` (and `us` on a US layout): the character, lower-cased,
/// when it is one printable ASCII character, else the US one.
fn reported(own: &str, us: &str) -> Option<String> {
    let one = |s: &str| {
        let mut chars = s.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_graphic() => Some(c.to_ascii_lowercase().to_string()),
            _ => None,
        }
    };
    one(own).or_else(|| one(us))
}

fn compile(ctx: &xkb::Context, names: &Names) -> Option<xkb::Keymap> {
    let fields = [
        &names.rules,
        &names.model,
        &names.layout,
        &names.variant,
        &names.options,
    ];
    // xkbcommon turns each name into a C string: an inner NUL would panic there.
    if fields.iter().any(|f| f.contains('\0')) {
        return None;
    }
    let options = (!names.options.is_empty()).then(|| names.options.clone());
    xkb::Keymap::new_from_names(
        ctx,
        &names.rules,
        &names.model,
        &names.layout,
        &names.variant,
        options,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
}

/// The configured layouts: the X server's own (`_XKB_RULES_NAMES`, what `setxkbmap -query`
/// prints) on X11; GNOME's input sources on Wayland; else the system default.
pub(crate) fn configured_names() -> Option<Names> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        gnome_names().or_else(system_names).or_else(x11_names)
    } else {
        x11_names().or_else(gnome_names).or_else(system_names)
    }
}

fn x11_names() -> Option<Names> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let atom = conn
        .intern_atom(true, b"_XKB_RULES_NAMES")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let value = conn
        .get_property(false, root, atom, AtomEnum::STRING, 0, 1024)
        .ok()?
        .reply()
        .ok()?
        .value;
    parse_rules_names(&value)
}

/// `_XKB_RULES_NAMES`: rules, model, layout, variant and options, each NUL-terminated.
fn parse_rules_names(value: &[u8]) -> Option<Names> {
    let mut fields = value
        .split(|b| *b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned());
    let mut next = || fields.next().unwrap_or_default();
    let names = Names {
        rules: next(),
        model: next(),
        layout: next(),
        variant: next(),
        options: next(),
    };
    (!names.layout.is_empty()).then_some(names)
}

fn gnome_names() -> Option<Names> {
    let out = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.input-sources", "sources"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_gnome_sources(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

/// GNOME's `[('xkb', 'gb'), ('xkb', 'de+nodeadkeys'), ('ibus', 'mozc')]`: the xkb ones, in
/// order, as `layout[+variant]`.
fn parse_gnome_sources(printed: &str) -> Option<Names> {
    let (mut layouts, mut variants) = (Vec::new(), Vec::new());
    for tuple in printed.split('(').skip(1) {
        let mut parts = tuple.split('\'').skip(1).step_by(2);
        let (Some(kind), Some(id)) = (parts.next(), parts.next()) else {
            continue;
        };
        if kind != "xkb" {
            continue;
        }
        let (layout, variant) = id.split_once('+').unwrap_or((id, ""));
        layouts.push(layout.to_string());
        variants.push(variant.to_string());
    }
    (!layouts.is_empty()).then(|| Names {
        layout: layouts.join(","),
        variant: variants.join(","),
        ..Names::default()
    })
}

fn system_names() -> Option<Names> {
    parse_default_keyboard(&std::fs::read_to_string("/etc/default/keyboard").ok()?)
}

/// Debian's `/etc/default/keyboard`: `XKBLAYOUT="gb"` and its siblings.
fn parse_default_keyboard(text: &str) -> Option<Names> {
    let get = |key: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let names = Names {
        model: get("XKBMODEL"),
        layout: get("XKBLAYOUT"),
        variant: get("XKBVARIANT"),
        options: get("XKBOPTIONS"),
        ..Names::default()
    };
    (!names.layout.is_empty()).then_some(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(layout: &str, active: &str) -> BTreeMap<String, String> {
        let names = Names {
            layout: layout.into(),
            ..Names::default()
        };
        shifted_for(&names, active).expect("the xkb data is installed")
    }

    fn shift(m: &BTreeMap<String, String>, key: &str) -> Option<String> {
        m.get(key).cloned()
    }

    #[test]
    fn each_layout_shifts_its_own_keys() {
        let us = map("us", "");
        assert_eq!(shift(&us, "]").as_deref(), Some("}"));
        assert_eq!(shift(&us, "2").as_deref(), Some("@"));
        let gb = map("gb", "");
        assert_eq!(shift(&gb, "2").as_deref(), Some("\""));
        assert_eq!(shift(&gb, "'").as_deref(), Some("@"));
        // £ is not ASCII: gpui names the key after its US character
        assert_eq!(shift(&gb, "3").as_deref(), Some("#"));
        let de = map("de", "");
        assert_eq!(shift(&de, "7").as_deref(), Some("/"));
        let fr = map("fr", "");
        assert_eq!(shift(&fr, "&").as_deref(), Some("1"));
        // Cyrillic keys are named after their US characters, as gpui names them
        let ru = map("ru", "");
        assert_eq!(shift(&ru, "]").as_deref(), Some("}"));
    }

    #[test]
    fn the_active_layout_decides() {
        let english = map("us,de", "English (US)");
        assert_eq!(shift(&english, "7").as_deref(), Some("&"));
        let german = map("us,de", "German");
        assert_eq!(shift(&german, "7").as_deref(), Some("/"));
    }

    #[test]
    fn layout_names_parse_from_each_source() {
        let x =
            parse_rules_names(b"evdev\0pc105\0gb,de\0,nodeadkeys\0grp:alt_shift_toggle\0").unwrap();
        assert_eq!(
            (x.rules.as_str(), x.layout.as_str(), x.variant.as_str()),
            ("evdev", "gb,de", ",nodeadkeys")
        );
        assert_eq!(x.options, "grp:alt_shift_toggle");
        assert_eq!(parse_rules_names(b"evdev\0pc105\0\0\0\0"), None);

        let g = parse_gnome_sources("[('xkb', 'gb'), ('ibus', 'mozc'), ('xkb', 'de+nodeadkeys')]")
            .unwrap();
        assert_eq!(
            (g.layout.as_str(), g.variant.as_str()),
            ("gb,de", ",nodeadkeys")
        );
        assert_eq!(parse_gnome_sources("@a(ss) []"), None);

        let d = parse_default_keyboard("XKBMODEL=\"pc105\"\nXKBLAYOUT=\"gb\"\nXKBVARIANT=\"\"\n")
            .unwrap();
        assert_eq!((d.model.as_str(), d.layout.as_str()), ("pc105", "gb"));
        assert_eq!(parse_default_keyboard("BACKSPACE=\"guess\"\n"), None);
    }
}
