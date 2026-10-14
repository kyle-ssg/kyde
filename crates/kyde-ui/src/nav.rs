//! Keyboard navigation for lists, trees and popups (issue #81). One shared set of actions,
//! bound in three key contexts that a view layers onto its elements:
//!
//! - [`NAV_LIST`] — ↑ / ↓ / ⏎ / ⇧F10. Put it on a list's wrapper, *including* a search box
//!   above the list: a single-line input doesn't bind those keys, so they bubble from it
//!   to the list (type to filter, arrow through the results).
//! - [`NAV_TREE`] — ← / → / space. Only on the focusable list element itself, never around
//!   an input (or space and the caret keys would stop reaching it).
//! - [`NAV_POPUP`] — ⎋ closes a popup list instead of bubbling to the window.
//!
//! Each view registers its own handlers on its elements; the focus path decides which run.
use gpui::{actions, App, KeyBinding};

actions!(
    kyde_nav,
    [
        /// Previous row.
        ListUp,
        /// Next row.
        ListDown,
        /// Collapse / go to parent (trees), or move to the pane on the left.
        ListLeft,
        /// Expand / go to first child (trees), or move to the pane on the right.
        ListRight,
        /// Activate the row (open, pick, expand).
        ListConfirm,
        /// Secondary activation: toggle a checkbox, preview a file.
        ListToggle,
        /// Open the row's context menu (⇧F10).
        ListMenu,
        /// Close the popup list.
        ListDismiss
    ]
);

/// Context for a list + its search box: ↑ / ↓ / ⏎ / ⇧F10.
pub const NAV_LIST: &str = "NavList";
/// Context for the focusable list element only: ← / → / space.
pub const NAV_TREE: &str = "NavTree";
/// Context for a popup list: ⎋ closes it.
pub const NAV_POPUP: &str = "NavPopup";

/// Bind the navigation keys (fixed, not user-configurable). Call from the app's keymap
/// setup, after `clear_key_bindings`.
pub fn bind_nav_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", ListUp, Some(NAV_LIST)),
        KeyBinding::new("down", ListDown, Some(NAV_LIST)),
        KeyBinding::new("enter", ListConfirm, Some(NAV_LIST)),
        KeyBinding::new("shift-f10", ListMenu, Some(NAV_LIST)),
        KeyBinding::new("left", ListLeft, Some(NAV_TREE)),
        KeyBinding::new("right", ListRight, Some(NAV_TREE)),
        KeyBinding::new("space", ListToggle, Some(NAV_TREE)),
        KeyBinding::new("escape", ListDismiss, Some(NAV_POPUP)),
    ]);
}
