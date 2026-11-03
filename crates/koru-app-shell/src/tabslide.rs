// SPDX-License-Identifier: MPL-2.0

//! The slide a tab makes when the order changes under it.
//!
//! ## Why the geometry does not move
//!
//! [`tabs::layout`](koru_shell_chrome::tabs::layout) stays pure arithmetic over
//! `(strip, count)` — that is what keeps the hit path free of a `TextEngine`
//! and free of a clock. So a reorder moves the tabs *immediately*, and what
//! animates is only how far from its slot each one is **drawn**. A click during
//! the slide therefore lands on the tab the strip says is there, not on the one
//! the eye has not caught up with — which is the behaviour you want when a swap
//! happens under a moving pointer.
//!
//! ## Why the tab you are dragging does not slide
//!
//! It is already where the pointer put it — the host's drag state decides
//! where it draws, and this decides where everything *else* does. Easing it as
//! well would make it trail the cursor, which reads as lag rather than as
//! motion. What the eye needs to see is its *neighbour* getting out of the way,
//! so that is the one that moves. The carried tab gets exactly one easing, on
//! release: [`settle`](TabSlides::settle) takes it over from the pointer and
//! drops it into its slot rather than teleporting it there.
//!
//! ## Not only tabs
//!
//! Nothing here knows which way the strip runs: a position is a number on
//! whatever axis the caller lays out on. The apps rail down the sidebar's
//! left edge rides this same slide on the vertical axis, with the same
//! duration and easing, so a view dragged up the rail moves the way a tab
//! dragged along the strip does. Where the carried item may go and when it
//! changes places is `koru_shell_chrome::reorder`, which the two share too.
//!
//! ## Why a deadline rather than a per-frame step
//!
//! The same reason as [`Panel`](crate::Panel): `View::animating` must
//! return to `false` or the loop spins at frame rate forever, and a per-frame
//! decrement can be stranded by a dropped frame where elapsed time cannot.

use std::time::Instant;

use koru_draw::{anim::ease_out_cubic as ease, theme};

#[derive(Default)]
pub struct TabSlides {
    /// Per tab, in current index order: how far from its slot it was when the
    /// current slide began, in logical pixels. Empty at rest.
    from: Vec<f32>,
    /// How much of `from` is still owed, 1.0 at the start and 0.0 at rest.
    /// Advanced once per frame so every tab in one frame agrees on the time.
    remaining: f32,
    /// When the current slide began. `None` at rest, and that is what
    /// [`animating`](Self::animating) reports.
    started: Option<Instant>,
}

impl TabSlides {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a reorder: the tab at `from` has just become the tab at `to`.
    ///
    /// `old_xs` / `new_xs` are each tab's slot left edge before and after the
    /// move (no scroll). Variable-width strips need the real distances rather
    /// than a uniform step.
    ///
    /// The moved tab itself starts at rest — see the module docs. While it is
    /// being dragged that entry is unused anyway, since the pointer decides
    /// where it draws; [`settle`](Self::settle) takes it back over on release.
    pub fn moved(&mut self, old_xs: &[f32], new_xs: &[f32], from: usize, to: usize, now: Instant) {
        let count = old_xs.len().min(new_xs.len());
        if from == to || count == 0 {
            return;
        }
        // What each tab is being drawn at right now, indexed by the order
        // *before* this move — so a reorder mid-slide carries on from where the
        // tabs actually are rather than snapping back to their slots first.
        let carried: Vec<f32> = (0..count).map(|i| self.offset(i)).collect();
        let from_offsets = (0..count)
            .map(|i| {
                let was = old_index(i, from, to);
                let carry = carried.get(was).copied().unwrap_or(0.0);
                // The dragged tab is where the pointer put it — do not ease it.
                if i == to {
                    return carry;
                }
                let old_x = old_xs.get(was).copied().unwrap_or(0.0);
                let new_x = new_xs.get(i).copied().unwrap_or(0.0);
                // Draw offset so the tab appears where it was, then eases to
                // its new slot: draw_x = new_x + offset ⇒ offset = old_x - new_x.
                carry + (old_x - new_x)
            })
            .collect();
        self.begin(from_offsets, now);
    }

    /// Let go of a carried tab: it is `offset` from its slot, and now eases in.
    ///
    /// Everything *else* keeps whatever it was already owed rather than
    /// restarting — the release usually lands in the middle of a neighbour's
    /// slide, and snapping that neighbour back to full travel would undo the
    /// motion the eye has already followed.
    pub fn settle(&mut self, count: usize, index: usize, offset: f32, now: Instant) {
        if offset == 0.0 || index >= count {
            return;
        }
        let mut from_offsets: Vec<f32> = (0..count).map(|i| self.offset(i)).collect();
        from_offsets[index] = offset;
        self.begin(from_offsets, now);
    }

    fn begin(&mut self, from: Vec<f32>, now: Instant) {
        self.from = from;
        self.remaining = 1.0;
        self.started = Some(now);
    }

    /// Forget every slide.
    ///
    /// Adding, closing or restoring tabs renumbers the list, at which point an
    /// offset recorded against an old index would be applied to a different
    /// tab — a document tab visibly sliding because a *different* one closed.
    pub fn clear(&mut self) {
        self.from.clear();
        self.remaining = 0.0;
        self.started = None;
    }

    pub fn animating(&self) -> bool {
        self.started.is_some()
    }

    /// Advance the slide. Returns whether anything moved.
    pub fn advance(&mut self, now: Instant) -> bool {
        let Some(started) = self.started else {
            return false;
        };
        let secs = (theme::TAB_SLIDE_MS / 1000.0).max(f32::EPSILON);
        let t = (now.duration_since(started).as_secs_f32() / secs).clamp(0.0, 1.0);
        self.remaining = 1.0 - ease(t);
        if t >= 1.0 {
            self.clear();
        }
        true
    }

    /// How far left or right of its slot the tab at `i` should be drawn.
    pub fn offset(&self, i: usize) -> f32 {
        self.from.get(i).copied().unwrap_or(0.0) * self.remaining
    }
}

/// Where the tab now at `i` sat before `from` moved to `to`.
fn old_index(i: usize, from: usize, to: usize) -> usize {
    if i == to {
        from
    } else if to < from && i > to && i <= from {
        i - 1
    } else if to > from && i >= from && i < to {
        i + 1
    } else {
        i
    }
}

#[cfg(test)]
mod tests;
