// SPDX-License-Identifier: MPL-2.0

//! Moving the rail's views: drag a square up or down, or `Ctrl+Shift+↑`/`↓`
//! on one the keyboard is on (`mainview::chrome_focus`).
//!
//! **The tab strip's gesture, on the other axis — and its code, not a copy of
//! it.** A press on a view's square shows that view, as a press on a tab
//! selects it, and the square then follows the pointer from the first pixel,
//! clamped to the views' run. It changes places once its own centre stands in
//! a neighbour's square — half of it over — and the neighbour slides into the
//! slot it left. Release drops it into its slot with the same easing; Esc
//! puts it back where the press found it and everything slides home. The
//! half-way rule and the clamp are `koru_shell_chrome::reorder`, which
//! `tabs::drop_index` and `tabs::carry_x` now read too, and the slide is the
//! strip's own `TabSlides`: one duration, one easing, one rule.
//!
//! **Only the views move.** The Koru mark is the lane's, and Help, Settings
//! and the profile square are the foot's; none of them is a view and none of
//! them is carried.
//!
//! **The digits stay in their slots.** `Alt+1` is the first view down the
//! rail, whatever it is ([`ExploreView::rail_shortcut`]): a view carried
//! from the fourth slot to the first is `Alt+1` the moment it lands there,
//! and each view it passed moves one digit down with its square.
//!
//! **Order and frames.** The order changes the moment a square crosses half
//! way, exactly as a tab's does, so hit-testing and the accessibility tree
//! always read the order that is drawn; it reaches the session file at the
//! next capture (`rail_order=`, `sidebar::rail_order`). Frames are asked for
//! only while a neighbour is sliding or the dropped square is settling —
//! [`ExploreView::animating`] — and a carried square draws on the pointer's
//! own moves, so a rail at rest costs nothing.

use std::time::Instant;

use koru_draw::geometry::Rect;
use koru_shell_chrome::reorder;

use super::{ExploreView, NavigatorMode};
use crate::panes::sidebar::apps_rail;
use crate::panes::sidebar::rail_order::RailOrder;

/// A view's square on the move.
pub(super) struct RailDrag {
    mode: NavigatorMode,
    /// Where it sat when the press began: what Esc puts it back to.
    from: usize,
    /// The press's offset into the square, so the square travels from the
    /// point it was grabbed rather than snapping its top under the pointer.
    grab: f32,
    /// The pointer, down the rail.
    y: f32,
}

impl ExploreView {
    /// The views the rail draws, in the person's order.
    pub fn rail_modes(&self) -> Vec<NavigatorMode> {
        self.rail_order.visible()
    }

    /// The view in rail slot `slot`, counted from zero down the views drawn
    /// — what `Alt+(slot+1)` shows. `None` for a slot with no view in it.
    pub fn rail_mode_at(&self, slot: usize) -> Option<NavigatorMode> {
        self.rail_modes().get(slot).copied()
    }

    /// The chord that shows `mode` **where it sits now**: its slot's digit,
    /// never a digit of its own (`koru_keys::rail_slot`). Moving a view
    /// moves its chord. `None` for a view this build hides, which takes no
    /// slot, and for one past the last digit.
    pub fn rail_shortcut(&self, mode: NavigatorMode) -> Option<koru_keys::ShortcutId> {
        let slot = self.rail_modes().iter().position(|m| *m == mode)?;
        koru_keys::rail_slot(slot)
    }

    /// The `rail_order=` line to save, empty for the default order.
    pub fn rail_order_storage(&self) -> String {
        self.rail_order.to_storage()
    }

    /// Put the rail back in the order a session saved.
    pub fn restore_rail_order(&mut self, line: &str) {
        self.rail_order = RailOrder::parse(line);
        self.rail_slides.clear();
    }

    /// Whether a view's square is held — what sends the pointer's moves, its
    /// release and Esc here wherever the pointer has gone.
    pub fn rail_dragging(&self) -> bool {
        self.rail_drag.is_some()
    }

    /// A press on `mode`'s square, `square` being where it is: it is now
    /// held.
    pub(super) fn begin_rail_drag(&mut self, mode: NavigatorMode, square: Rect, y: f32) {
        let Some(from) = self.rail_modes().iter().position(|m| *m == mode) else {
            return;
        };
        self.rail_drag = Some(RailDrag {
            mode,
            from,
            grab: y - square.y,
            y,
        });
    }

    /// Where the held square is, and where it is drawn: on the pointer,
    /// clamped to the run of views. `None` with nothing held.
    pub(super) fn rail_carried(&self, rects: &apps_rail::Rects) -> Option<(usize, Rect)> {
        let drag = self.rail_drag.as_ref()?;
        let index = rects.apps.iter().position(|(m, _)| *m == drag.mode)?;
        let square = rects.apps[index].1;
        let (first, last) = (rects.apps.first()?.1, rects.apps.last()?.1);
        let top = reorder::carry(drag.y - drag.grab, first.y, last.y + last.h, square.h);
        Some((index, Rect::new(square.x, top, square.w, square.h)))
    }

    /// The pointer moved while a square is held: it follows, and changes
    /// places once half of it is over a neighbour. Always a repaint — the
    /// square moved.
    pub fn rail_drag_move(&mut self, rail: Rect, y: f32) -> bool {
        let Some(drag) = self.rail_drag.as_mut() else {
            return false;
        };
        drag.y = y;
        let rects = self.apps_rail_rects(rail);
        if let Some((index, carried)) = self.rail_carried(&rects) {
            let slots: Vec<(f32, f32)> = rects.apps.iter().map(|(_, r)| (r.y, r.h)).collect();
            let to = reorder::drop_index(&slots, index, carried.y + carried.h * 0.5);
            if to != index {
                self.move_rail_view(rail, index, to);
            }
        }
        true
    }

    /// The release: the square drops into its slot from wherever the pointer
    /// left it. The order is already the one drawn.
    pub fn rail_drag_release(&mut self, rail: Rect) -> bool {
        let rects = self.apps_rail_rects(rail);
        let landing = self.rail_carried(&rects);
        if self.rail_drag.take().is_none() {
            return false;
        }
        if let Some((index, carried)) = landing {
            let slot = rects.apps[index].1.y;
            self.rail_slides
                .settle(rects.apps.len(), index, carried.y - slot, Instant::now());
        }
        true
    }

    /// Esc while a square is held: it goes back where the press found it, and
    /// the views it passed slide back too.
    pub fn cancel_rail_drag(&mut self, rail: Rect) -> bool {
        let rects = self.apps_rail_rects(rail);
        let landing = self.rail_carried(&rects);
        let Some(drag) = self.rail_drag.take() else {
            return false;
        };
        let Some((index, carried)) = landing else {
            return true;
        };
        // Every move a drag makes moves only the held view, so undoing them
        // all is one move: from where it is back to where it was.
        self.move_rail_view(rail, index, drag.from);
        let rects = self.apps_rail_rects(rail);
        if let Some((_, home)) = rects.apps.get(drag.from) {
            self.rail_slides.settle(
                rects.apps.len(),
                drag.from,
                carried.y - home.y,
                Instant::now(),
            );
        }
        true
    }

    /// `Ctrl+Shift+↑`/`↓` on the view at `index`: one place up or down, the
    /// view easing into its new slot as the one it passed eases into the old.
    /// Where the view now is, or `None` at the end of the run.
    pub fn nudge_rail_view(&mut self, rail: Rect, index: usize, up: bool) -> Option<usize> {
        let to = if up { index.checked_sub(1)? } else { index + 1 };
        let before = self.apps_rail_rects(rail);
        let was = before.apps.get(index)?.1.y;
        if !self.move_rail_view(rail, index, to) {
            return None;
        }
        let after = self.apps_rail_rects(rail);
        if let Some((_, now)) = after.apps.get(to) {
            self.rail_slides
                .settle(after.apps.len(), to, was - now.y, Instant::now());
        }
        Some(to)
    }

    /// Move the view at `from` to `to` among the shown ones, and start the
    /// views between sliding — the one place the order changes, so it and
    /// the slide cannot disagree. `false` when there is no such move.
    fn move_rail_view(&mut self, rail: Rect, from: usize, to: usize) -> bool {
        let mut modes = self.rail_modes();
        if from == to || from >= modes.len() || to >= modes.len() {
            return false;
        }
        let tops = |view: &Self| -> Vec<f32> {
            view.apps_rail_rects(rail)
                .apps
                .iter()
                .map(|(_, r)| r.y)
                .collect()
        };
        let before = tops(self);
        let mode = modes.remove(from);
        modes.insert(to, mode);
        if !self.rail_order.set_visible(&modes) {
            return false;
        }
        let after = tops(self);
        self.rail_slides
            .moved(&before, &after, from, to, Instant::now());
        true
    }
}

#[cfg(test)]
mod tests;
