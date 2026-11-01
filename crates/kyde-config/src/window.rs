//! Window settings on Linux, persisted as `window.json` next to keymap.json: where the menus
//! live — a menu bar along the top of the window, or behind a ☰ button.

use serde::{Deserialize, Serialize};

/// Where the menus live (Settings → Appearance → Menu bar).
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum MenuBar {
    /// A menu bar where the window manager draws the title bar, the ☰ button where Kyde draws
    /// its own (a Wayland desktop that leaves decorations to the app, like GNOME).
    #[default]
    FollowDesktop,
    /// Always the menu bar.
    Bar,
    /// Always the ☰ button.
    Button,
}

impl MenuBar {
    /// Whether the menus show as a menu bar (else behind the ☰ button), given whether the
    /// window manager draws the window's title bar (server-side decorations).
    ///
    /// ```
    /// use kyde_config::window::MenuBar;
    /// assert!(MenuBar::FollowDesktop.shows_bar(true));
    /// assert!(!MenuBar::FollowDesktop.shows_bar(false));
    /// ```
    #[must_use]
    pub fn shows_bar(self, wm_title_bar: bool) -> bool {
        match self {
            Self::FollowDesktop => wm_title_bar,
            Self::Bar => true,
            Self::Button => false,
        }
    }
}

/// Window settings (persisted to `window.json`).
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug, Default)]
pub struct WindowCfg {
    /// Where the menus live.
    #[serde(default)]
    pub menu_bar: MenuBar,
}

impl WindowCfg {
    /// Load from `window.json` (missing / unreadable / an unknown value → defaults).
    #[must_use]
    pub fn load() -> Self {
        crate::store::load_or_default("window.json")
    }

    /// Persist to `window.json`.
    pub fn save(&self) {
        crate::store::save("window.json", self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Follow desktop gives the menu bar under the window manager's title bar and the ☰
    /// button where Kyde draws its own; the other two choices hold whatever the desktop does.
    #[test]
    fn menu_bar_follows_the_title_bar_unless_chosen() {
        for wm_title_bar in [true, false] {
            assert_eq!(MenuBar::FollowDesktop.shows_bar(wm_title_bar), wm_title_bar);
            assert!(MenuBar::Bar.shows_bar(wm_title_bar));
            assert!(!MenuBar::Button.shows_bar(wm_title_bar));
        }
    }

    /// A missing file or field follows the desktop; a value Kyde doesn't know (a hand edit, a
    /// newer Kyde's) does too instead of failing.
    #[test]
    fn unreadable_settings_follow_the_desktop() {
        let parse = |s: &str| serde_json::from_str::<WindowCfg>(s).unwrap_or_default();
        assert_eq!(parse("{}").menu_bar, MenuBar::FollowDesktop);
        assert_eq!(
            parse(r#"{"menu_bar":"sideways"}"#).menu_bar,
            MenuBar::FollowDesktop
        );
        assert_eq!(parse("not json").menu_bar, MenuBar::FollowDesktop);
        for choice in [MenuBar::FollowDesktop, MenuBar::Bar, MenuBar::Button] {
            let cfg = WindowCfg { menu_bar: choice };
            let json = serde_json::to_string(&cfg).unwrap();
            assert_eq!(parse(&json), cfg);
        }
        assert_eq!(parse(r#"{"menu_bar":"button"}"#).menu_bar, MenuBar::Button);
    }
}
