//! Small OS-integration utilities (no gpui): shell-command install, scratch paths, alerts.
#[cfg_attr(not(feature = "terminal"), allow(dead_code))]
pub(crate) mod alert;
pub(crate) mod clipboard;
#[cfg(target_os = "linux")]
pub(crate) mod input_method;
#[cfg(unix)]
pub(crate) mod instance;
#[cfg(target_os = "linux")]
pub(crate) mod keyboard_layout;
pub(crate) mod scratch;
pub(crate) mod shellcmd;
#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) mod terminal_app;
#[cfg(target_os = "linux")]
pub(crate) mod window_icon;
