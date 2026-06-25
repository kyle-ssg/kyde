//! Shared UI toolkit — the reusable, Kyde-agnostic building blocks every view composes from:
//! buttons, tab pills, badges, checkboxes, the dismiss overlay, context-menu icons, colour +
//! scrollbar maths, and the file-tree row. Each lives in its own file; all are re-exported
//! here and again at the crate root, so feature modules reach them via `use super::*`.

mod badge;
mod button;
mod checkbox;
mod color;
mod menu;
mod overlay;
mod scrollbar;
mod tab;

pub(crate) use badge::*;
pub(crate) use button::*;
pub(crate) use checkbox::*;
pub(crate) use color::*;
pub(crate) use menu::*;
pub(crate) use overlay::*;
pub(crate) use scrollbar::*;
pub(crate) use tab::*;
