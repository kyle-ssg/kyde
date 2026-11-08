//! A UTF-8 locale for the shells Kyde starts. An app opened from the Dock or Finder gets no
//! `LANG`, so its terminals' shells would run in the C locale: zsh then draws each non-ASCII
//! character as `<0080>`-style escapes and miscounts line widths (an em dash in a typed agent
//! prompt came out mangled). Like Terminal and iTerm, Kyde gives a shell `LANG` when its
//! environment sets no locale at all. macOS only: a Linux desktop session exports `LANG`.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The variables a shell's locale comes from; any one set means it already has one.
const LOCALE_VARS: [&str; 3] = ["LC_ALL", "LC_CTYPE", "LANG"];

/// What a shell gets when the system's own locale has no UTF-8 form.
const FALLBACK: &str = "en_US.UTF-8";

/// `env` (a shell's variables on top of Kyde's own environment) plus `LANG` when neither
/// sets a locale: the system's language and region in UTF-8 (`en_GB.UTF-8`).
pub(crate) fn with_lang(mut env: HashMap<String, String>) -> HashMap<String, String> {
    let has = |k: &str| {
        env.get(k).is_some_and(|v| !v.is_empty())
            || std::env::var_os(k).is_some_and(|v| !v.is_empty())
    };
    if let Some(lang) = lang(has, system_lang) {
        env.insert("LANG".into(), lang);
    }
    env
}

/// `LANG` for a shell whose variables `has` reports, from `system` (only asked when no
/// locale is set).
fn lang(has: impl Fn(&str) -> bool, system: impl FnOnce() -> String) -> Option<String> {
    (!LOCALE_VARS.iter().any(|k| has(k))).then(system)
}

/// The system locale's UTF-8 form (read once: it's the same for every shell this run).
fn system_lang() -> String {
    static LANG: OnceLock<String> = OnceLock::new();
    LANG.get_or_init(|| {
        let id = objc2_foundation::NSLocale::currentLocale()
            .localeIdentifier()
            .to_string();
        utf8_lang(&id, |name| {
            std::path::Path::new("/usr/share/locale")
                .join(name)
                .is_dir()
        })
    })
    .clone()
}

/// The UTF-8 locale for macOS locale identifier `id` (`en_GB`, `de_DE@rg=chzzzz`) when
/// `exists` says the system has it — a language and region it doesn't pair (`en_DE`) has
/// none — else [`FALLBACK`].
fn utf8_lang(id: &str, exists: impl Fn(&str) -> bool) -> String {
    let base = id.split('@').next().unwrap_or_default();
    let name = format!("{base}.UTF-8");
    if !base.is_empty() && exists(&name) {
        name
    } else {
        FALLBACK.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shell_without_a_locale_gets_the_systems_in_utf8() {
        let none = |_: &str| false;
        let have = |name: &str| ["en_GB.UTF-8", "de_DE.UTF-8"].contains(&name);
        let system = |id: &str| utf8_lang(id, have);
        assert_eq!(
            lang(none, || system("en_GB")).as_deref(),
            Some("en_GB.UTF-8")
        );
        // Region override (`@rg=`) dropped; a pairing the system lacks falls back.
        assert_eq!(system("de_DE@rg=chzzzz"), "de_DE.UTF-8");
        assert_eq!(system("en_DE"), FALLBACK);
        // Any locale variable already set is left alone.
        for var in LOCALE_VARS {
            assert_eq!(lang(|k| k == var, || system("en_GB")), None);
        }
    }
}
