#![deny(missing_docs)]
//! Kyde's on-disk configuration + persistence, all JSON under `~/.config/kyde` (XDG-aware).
//! Pure Rust, no GUI — three independent stores:
//! - [`keymap`] — the keybinding preset + per-action overrides,
//! - [`plugins`] — the installed language-pack list,
//! - [`projects`] — the recent-projects list for the landing view.

/// Keybinding preset + overrides ([`keymap::Keymap`]).
pub mod keymap;
/// Installed language-pack list ([`plugins::Plugins`]).
pub mod plugins;
/// Recent-projects list ([`projects::Recents`]).
pub mod projects;
/// Shared JSON config-file persistence (XDG dir + generic load/save).
pub mod store;
