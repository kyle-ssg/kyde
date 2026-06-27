//! gpui-coupled widgets (their own Entities/Elements), as opposed to app feature views.
pub(crate) mod editor;
pub(crate) mod mdview;
#[cfg(feature = "remote-images")]
pub(crate) mod remote_img;
#[cfg(feature = "terminal")]
pub(crate) mod terminal;
