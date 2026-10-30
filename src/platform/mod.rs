//! Small OS-integration utilities (no gpui): shell-command install, scratch paths, alerts.
#[cfg_attr(not(feature = "terminal"), allow(dead_code))]
pub(crate) mod alert;
pub(crate) mod clipboard;
#[cfg(unix)]
pub(crate) mod instance;
pub(crate) mod scratch;
pub(crate) mod shellcmd;
