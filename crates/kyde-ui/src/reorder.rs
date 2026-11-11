//! Drag-to-reorder for a strip of items (tab strips): press an item, drag it past a few
//! pixels, and a line shows where it lands on release. The hosting view owns a [`Reorder`],
//! feeds it the mouse events, draws [`drop_line`] where [`Reorder::line`] says, and moves
//! the item when [`Reorder::release`] returns one.
use gpui::prelude::*;
use gpui::{div, px, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point};
use kyde_theme as theme;

/// Which edge of the item under the cursor the dragged one lands on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// Its leading edge: the dragged item came from further along.
    Before,
    /// Its trailing edge: the dragged item came from earlier.
    After,
}

/// An item pressed, a drag once the mouse has moved, over the item it would move to.
struct Drag<K> {
    item: K,
    from: Point<Pixels>,
    active: bool,
    over: Option<K>,
}

/// The drag in progress (if any) in one strip, whose items are named by a key `K` that
/// outlives a re-render (a path, an id — not an index).
pub struct Reorder<K>(Option<Drag<K>>);

impl<K> Default for Reorder<K> {
    fn default() -> Self {
        Self(None)
    }
}

impl<K: Clone + PartialEq> Reorder<K> {
    /// `item` was pressed: a drag may start once the mouse moves (not on a double-click).
    pub fn press(&mut self, item: K, e: &MouseDownEvent) {
        if e.click_count == 1 {
            self.0 = Some(Drag {
                item,
                from: e.position,
                active: false,
                over: None,
            });
        }
    }

    /// An item is pressed or being dragged — a release is this strip's to handle.
    pub fn pressed(&self) -> bool {
        self.0.is_some()
    }

    /// The mouse moved over the strip: a pressed item becomes a drag past a few pixels; a
    /// button released elsewhere ends it. `true` when the strip needs a repaint.
    pub fn moved(&mut self, e: &MouseMoveEvent) -> bool {
        let Some(d) = &mut self.0 else {
            return false;
        };
        if e.pressed_button != Some(MouseButton::Left) {
            let was_active = d.active;
            self.0 = None;
            return was_active;
        }
        if !d.active && (e.position - d.from).magnitude() > 5.0 {
            d.active = true;
            return true;
        }
        false
    }

    /// The mouse is over `item`: that's where the dragged one would go. `true` when the
    /// strip needs a repaint.
    pub fn hover(&mut self, item: &K) -> bool {
        let Some(d) = self.0.as_mut().filter(|d| d.active) else {
            return false;
        };
        if d.over.as_ref() == Some(item) {
            return false;
        }
        d.over = Some(item.clone());
        true
    }

    /// `item` is the one being dragged (draw it dimmed).
    pub fn dragging(&self, item: &K) -> bool {
        self.0.as_ref().is_some_and(|d| d.active && &d.item == item)
    }

    /// The drop line to draw on `item`, if the drag would land there. `pos` gives an item's
    /// place in the strip.
    pub fn line(&self, item: &K, pos: impl Fn(&K) -> Option<usize>) -> Option<Side> {
        let d = self.0.as_ref().filter(|d| d.active)?;
        if d.over.as_ref() != Some(item) {
            return None;
        }
        match pos(item)?.cmp(&pos(&d.item)?) {
            std::cmp::Ordering::Less => Some(Side::Before),
            std::cmp::Ordering::Greater => Some(Side::After),
            std::cmp::Ordering::Equal => None,
        }
    }

    /// The button was released: the drag is over. `Some((item, onto))` when `item` should
    /// move to where `onto` is.
    pub fn release(&mut self) -> Option<(K, K)> {
        let d = self.0.take()?;
        let onto = d.over.filter(|o| d.active && *o != d.item)?;
        Some((d.item, onto))
    }
}

/// The accent line marking where a dragged item lands: a child of the item under the cursor
/// (which must be `relative()` with a 1px border), centred in the `gap` between it and its
/// neighbour on that `side`. Absolute, so the strip's layout doesn't shift under the cursor.
pub fn drop_line(side: Side, gap: f32) -> gpui::Div {
    // From the item's padding box: out past its border, to the middle of the gap, less
    // half the line's own width.
    let out = px(-(1.0 + gap / 2.0 + 1.0));
    let line = div()
        .absolute()
        .top(px(1.0))
        .bottom(px(1.0))
        .w(px(2.0))
        .rounded_full()
        .bg(theme::get().primary);
    match side {
        Side::Before => line.left(out),
        Side::After => line.right(out),
    }
}
