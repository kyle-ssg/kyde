//! Per-feature view modules. Each is an `impl Kyde` block holding that feature's `render_*`
//! methods + logic; methods called from elsewhere are `pub(crate)`. The features compose the
//! shared `ui` toolkit and the controller core in app.rs/render.rs.
mod agents;
mod branch;
mod browse;
mod changelog;
pub(crate) mod chrome;
mod commit;
mod compare;
mod context_menu;
mod diff_view;
mod file_ops;
mod find;
mod finder;
mod history;
mod keyboard;
mod local_history;
#[cfg(target_os = "linux")]
mod menu_bar;
mod merge;
mod modals;
mod notifications;
mod onboarding;
mod projects_view;
mod push;
mod rollback;
mod settings;
mod tabs;
#[cfg(feature = "terminal")]
mod terminal_panel;
mod toolbar;
mod worktree;

pub(crate) use context_menu::MenuEntry;
pub(crate) use file_ops::{REVEAL_IN_FILES, REVEAL_IN_TERMINAL};
pub(crate) use keyboard::{nav_keys, NavTarget};
