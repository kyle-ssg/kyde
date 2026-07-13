//! Unified divider dragging — one mechanism for every resize divider in the app
//! (file-tree, markdown split, diff center, history commit/panel, terminal).
//!
//! The pane each divider controls is laid out at an explicit pixel size, and the grab
//! offset captured on mouse-down cancels the (context-dependent) geometry, so the bar
//! stays exactly under the cursor — the 1:1 tracking the markdown split always had, now
//! shared by all of them. A crate-root child module, so the `impl Kyde` block below reaches
//! `Kyde`'s private fields directly (like `render.rs`).

use super::{theme, Kyde, Window, RAIL_W};

/// Width of the diff center gutter (the `»`/checkbox column). It's `flex_none`, so the two
/// diff panes share only `island_w - DIFF_GUTTER_W`.
pub(crate) const DIFF_GUTTER_W: f32 = 44.0;

/// Width a full-width island spans (the diff view + history). Islands begin at the rail's
/// right edge (`RAIL_W` already includes the frame-gap margin) and end a frame gap from the
/// window's right. Used by both the layout and the divider math so they agree exactly.
pub(crate) fn full_island_w(vw: f32) -> f32 {
    (vw - RAIL_W - theme::FRAME_GAP).max(1.0)
}

/// Every draggable resize divider in the app. They ALL share one drag mechanism
/// (`Kyde::start_divider_drag` + `drag_divider`): the pane each controls is laid out at an
/// explicit pixel size, and the grab offset captured on mouse-down cancels the (context-
/// dependent) geometry, so the bar stays exactly under the cursor — the behaviour the
/// markdown split already had, now applied uniformly.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Divider {
    /// Browse / Commit file-tree pane width (horizontal).
    Tree,
    /// Markdown split: code-editor pane width (horizontal).
    MdSplit,
    /// Side-by-side diff: left pane width (horizontal).
    DiffPane,
    /// History view: commit-list pane width (horizontal).
    HistCommit,
    /// History view: bottom log-panel height (vertical, grows upward).
    HistPanel,
    /// Bottom terminal panel height (vertical, grows upward). Only ever dragged when the
    /// `terminal` feature is built in (the panel doesn't exist otherwise).
    #[cfg_attr(not(feature = "terminal"), allow(dead_code))]
    Term,
}

impl Divider {
    /// Vertical dividers drag along Y; the rest along X. (`pub(crate)`: the root mouse handler
    /// reads it to pin the row/col resize cursor while dragging.)
    pub(crate) fn vertical(self) -> bool {
        matches!(self, Self::HistPanel | Self::Term)
    }
    /// "Trailing" panes are anchored to the far (bottom) edge and grow as the cursor moves
    /// toward the near edge; "leading" panes grow with the cursor.
    fn trailing(self) -> bool {
        matches!(self, Self::HistPanel | Self::Term)
    }
}

impl Kyde {
    /// Current pixel size of the pane `d` controls. Diff/history store a *fraction*; convert it
    /// to live pixels with the SAME width the layout uses, so the round-trip through
    /// `set_divider_size` is lossless (the fractional scale cancels).
    fn divider_size(&self, d: Divider, vw: f32, _vh: f32) -> f32 {
        match d {
            Divider::Tree => self.browse.tree_width,
            Divider::MdSplit => self.browse.md_editor_w,
            Divider::DiffPane => {
                self.diff.split.clamp(0.15, 0.85) * (full_island_w(vw) - DIFF_GUTTER_W).max(1.0)
            }
            Divider::HistCommit => self.history.commit_frac.clamp(0.15, 0.85) * full_island_w(vw),
            Divider::HistPanel => self.history.panel_h,
            Divider::Term => {
                #[cfg(feature = "terminal")]
                {
                    self.term.height
                }
                #[cfg(not(feature = "terminal"))]
                {
                    0.0
                }
            }
        }
    }

    /// Set state so the pane `d` controls becomes `size` px, clamped to keep both sides usable.
    fn set_divider_size(&mut self, d: Divider, size: f32, vw: f32, vh: f32) {
        match d {
            Divider::Tree => self.browse.tree_width = size.clamp(180.0, 900.0),
            Divider::MdSplit => {
                let island_left = RAIL_W + self.browse.tree_width + theme::FRAME_GAP;
                let island_w = (vw - island_left - theme::FRAME_GAP).max(1.0);
                self.browse.md_editor_w = size.clamp(200.0, (island_w - 200.0).max(200.0));
            }
            Divider::DiffPane => {
                let avail = (full_island_w(vw) - DIFF_GUTTER_W).max(1.0);
                self.diff.split = (size / avail).clamp(0.15, 0.85);
            }
            Divider::HistCommit => {
                self.history.commit_frac = (size / full_island_w(vw)).clamp(0.15, 0.85);
            }
            Divider::HistPanel => {
                self.history.panel_h = size.clamp(140.0, (vh - 180.0).max(140.0));
            }
            Divider::Term => {
                #[cfg(feature = "terminal")]
                {
                    self.term.height = size.clamp(120.0, (vh - 160.0).max(120.0));
                }
                #[cfg(not(feature = "terminal"))]
                {
                    let _ = (size, vh);
                }
            }
        }
    }

    /// Begin dragging divider `d`. Captures the grab offset so the first move doesn't jolt the
    /// bar to the cursor — and, because the offset absorbs the geometry, tracking is exact 1:1.
    pub(crate) fn start_divider_drag(
        &mut self,
        d: Divider,
        cursor: gpui::Point<gpui::Pixels>,
        window: &Window,
    ) {
        let sz = window.viewport_size();
        let (vw, vh) = (f32::from(sz.width), f32::from(sz.height));
        let coord = f32::from(if d.vertical() { cursor.y } else { cursor.x });
        let size = self.divider_size(d, vw, vh);
        // Leading panes grow with the cursor (size = coord − grab); trailing panes grow as the
        // cursor moves toward the near edge (size = grab − coord).
        let grab = if d.trailing() {
            coord + size
        } else {
            coord - size
        };
        self.divider_drag = Some((d, grab));
    }

    /// Apply the active divider drag for the current cursor position. Returns whether a drag was
    /// active (so the caller can `cx.notify()`).
    pub(crate) fn drag_divider(
        &mut self,
        cursor: gpui::Point<gpui::Pixels>,
        vw: f32,
        vh: f32,
    ) -> bool {
        let Some((d, grab)) = self.divider_drag else {
            return false;
        };
        let coord = f32::from(if d.vertical() { cursor.y } else { cursor.x });
        let size = if d.trailing() {
            grab - coord
        } else {
            coord - grab
        };
        self.set_divider_size(d, size, vw, vh);
        true
    }

    /// Whether divider `d` is the one currently being dragged.
    pub(crate) fn dragging(&self, d: Divider) -> bool {
        matches!(self.divider_drag, Some((k, _)) if k == d)
    }
}
