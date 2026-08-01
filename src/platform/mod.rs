//! Small OS-integration utilities (no gpui): shell-command install, scratch paths.
pub(crate) mod clipboard;
#[cfg(unix)]
pub(crate) mod instance;
pub(crate) mod scratch;
pub(crate) mod shellcmd;
