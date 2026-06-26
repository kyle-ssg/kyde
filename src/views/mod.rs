//! Per-feature view modules. Each is an `impl Kyde` block holding that feature's `render_*`
//! methods + logic; methods called from elsewhere are `pub(crate)`. The features compose the
//! shared `ui` toolkit and the controller core in app.rs/render.rs.
mod branch;
mod browse;
mod commit;
mod diff_view;
mod file_ops;
mod find;
mod finder;
mod history;
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
