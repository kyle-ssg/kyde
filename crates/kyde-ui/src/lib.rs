#![deny(missing_docs)]
//! kyde-ui — reusable, app-agnostic UI building blocks (gpui widgets). Knows nothing about
//! Kyde: buttons, tab pills, badges, checkboxes, context-menu icons, colour + scrollbar maths,
//! and the file-tree row (`tree::item<V>`, generic over the hosting view). Depends only on
//! gpui + kyde-theme.
mod badge;
mod button;
mod checkbox;
mod color;
mod menu;
mod scrollbar;
mod select;
mod tab;
/// Call sites use the explicit `ui::tree::item(…)`.
pub mod tree;

pub use badge::*;
pub use button::*;
pub use checkbox::*;
pub use color::*;
pub use menu::*;
pub use scrollbar::*;
pub use select::*;
pub use tab::*;
