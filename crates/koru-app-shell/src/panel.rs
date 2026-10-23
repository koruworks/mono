// SPDX-License-Identifier: MPL-2.0

//! A side column that slides open and shut.
//!
//! ## Why the content does not resize while it slides
//!
//! `reflow` is parley over every row and is the most expensive thing in the
//! app. Interpolating the *content* width would pay it sixty times for one
//! toggle. So the layout width is final the instant the column is toggled —
//! the content reflows **once** — and only the column's own `x` is animated,
//! sliding into the space that already opened or out of the space that has
//! already closed.
//!
//! The cost is that the view snaps to its new width while the column is still
//! arriving. That is the right trade at reflow's price, and upgrading to a
//! continuous push later is a one-line change here if `perf.rs` ever says it is
//! cheap enough.
//!
//! ## Why a deadline rather than a per-frame step
//!
//! `View::animating` must return to `false` or the loop spins at frame rate
//! forever. A per-frame decrement can be stranded by a dropped frame; elapsed
//! time against a fixed duration cannot.

use std::time::Instant;

use koru_draw::geometry::Rect;
use koru_draw::{anim::ease_out_cubic as ease, theme};

/// **How wide the invisible band you can grab is**, and it is Koru's: `GRIP` in
/// `apps/koru/src/mainview/layout.rs`.
///
/// It straddles the edge — half either side of the hairline — so there is a
/// target outside the column as well as inside it. That is what makes the grip
/// half of it land over the *content*, which is why the shell has to answer for
/// it before anything else resolves a point (see `ShellState::pane_grip`).
pub const GRIP: f32 = 8.0;

/// How far the left column may be dragged, both ends. **The theme owns the
/// numbers** (`koru_draw::theme::SIDEBAR_MIN_W` / `SIDEBAR_MAX_W`): this file
/// and the workspace window's layout each held a copy, both 190/500, and a
/// clamp written twice is two clamps. Re-exported so the shell's callers keep
/// their path.
///
/// Koru's reasoning applies here unchanged: the floor is where a tree stops
/// being readable — a name elided to three characters is not a list — and the
/// ceiling is where a *column* stops being one. `regions::solve` still gives way
/// in order below either, so these are taste rather than safety; a limit you
/// cannot usefully reach is not a limit.
pub use koru_draw::theme::{SIDEBAR_MAX_W, SIDEBAR_MIN_W};

/// The band at a drawn column's resizing edge, or a zero-width rect when there
/// is nothing to grab.
///
/// **On the drawn edge, not the settled one**, because that is the one under the
/// pointer: mid-drag `regions.sidebar` is still the width the content was laid
/// out against, and mid-slide it is a column's width away from where the person
/// can see it.
pub fn grip(drawn: Rect) -> Rect {
    if drawn.w <= 0.0 {
        return Rect::new(0.0, 0.0, 0.0, 0.0);
    }
    let edge = drawn.x + drawn.w;
    Rect::new(edge - GRIP * 0.5, drawn.y, GRIP, drawn.h)
}

pub struct Panel {
    /// The column's width when open. `open` decides whether the layout gets it.
    ///
    /// **Was constant, and is not any more**: the explore pane can be dragged
    /// wider. Two widths, not one, because they move at different moments —
    /// see [`committed`](Self::committed).
    full: f32,
    /// The width the *content* has been laid out against.
    ///
    /// Equal to `full` at rest. During a resize drag they come apart: the column
    /// draws at the live `full` and the content keeps this one until the button
    /// is released. That is the whole mitigation for the drag, and it is not a
    /// nicety — `Editing::set_viewport` dirties every row on any width change,
    /// which ROADMAP measures at **72 ms per step at 2000 rows** in debug. A
    /// drag that reflowed per mouse-move would be that, several times a second.
    ///
    /// The cost is a band of empty gutter while you drag it wider, and an
    /// overhang hidden by the content clip while you drag it narrower. That is
    /// the same trade this module's opening comment defends for the toggle.
    committed: f32,
    /// Whether the column is open. Flips instantly on toggle, which is what
    /// makes the content reflow exactly once.
    open: bool,
    /// How far the column has actually slid in, 0.0 (gone) to 1.0 (flush).
    progress: f32,
    /// When the current slide began and the progress it began from. `None` at
    /// rest — and that is what [`animating`](Self::animating) reports.
    slide: Option<(Instant, f32)>,
}

impl Panel {
    pub fn new(full: f32, open: bool) -> Self {
        Self {
            full,
            committed: full,
            open,
            progress: if open { 1.0 } else { 0.0 },
            slide: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The width the layout should use — what the content rect gives up. Final
    /// the instant the column is toggled, and **unchanged during a resize drag**
    /// until the button comes up. See [`committed`](Self::committed).
    pub fn layout_width(&self) -> f32 {
        if self.open { self.committed } else { 0.0 }
    }

    /// The width the column is being dragged to, which is what it *draws* at.
    pub fn full(&self) -> f32 {
        self.full
    }

    /// Set the width. Returns whether it moved.
    ///
    /// The column redraws at the new width immediately; the content does not
    /// move until [`commit_width`](Self::commit_width).
    pub fn set_full(&mut self, w: f32) -> bool {
        let moved = (self.full - w).abs() > f32::EPSILON;
        self.full = w;
        moved
    }

    /// Let the content catch up with the column. Called on mouse-up, and once
    /// when a width is restored or a workspace is switched.
    ///
    /// Returns whether the layout width actually moved — so a caller can skip
    /// the reflow when it did not, which is every frame but one of a drag.
    pub fn commit_width(&mut self) -> bool {
        let moved = (self.committed - self.full).abs() > f32::EPSILON;
        self.committed = self.full;
        moved
    }

    /// How far left of its slot the column should be *drawn*, in logical
    /// pixels. Zero when flush; `full` when entirely off the edge.
    pub fn slide_offset(&self) -> f32 {
        (1.0 - self.progress) * self.full
    }

    /// The width to draw at, which is the full width whenever any of the column
    /// is on screen — including while it slides out, after the layout has
    /// already taken its space back.
    pub fn drawn_width(&self) -> f32 {
        if self.progress > 0.0 || self.slide.is_some() {
            self.full
        } else {
            0.0
        }
    }

    /// How far through its slide the column is, 0 shut to 1 open.
    ///
    /// **Nothing in the app reads this today.** The caption's logo used to: it
    /// turned as the pane arrived, riding this number so it needed no clock of
    /// its own and added nothing to `View::animating`. The logo is now the
    /// pane's own toggle glyph, which reports open/shut rather than progress, so
    /// the only callers left are the tests that pin [`set_immediately`] leaving
    /// a restored window at exactly 0 or 1 with no slide in flight. Kept because
    /// it is the natural question to ask a slide, and the next thing that wants
    /// to ride one will ask it.
    ///
    /// [`set_immediately`]: Self::set_immediately
    #[allow(dead_code)]
    pub fn progress(&self) -> f32 {
        self.progress
    }

    pub fn animating(&self) -> bool {
        self.slide.is_some()
    }

    /// Advance the slide. Returns whether anything moved.
    pub fn advance(&mut self, now: Instant) -> bool {
        let Some((started, from)) = self.slide else {
            return false;
        };
        let secs = (theme::PANEL_SLIDE_MS / 1000.0).max(f32::EPSILON);
        let t = (now.duration_since(started).as_secs_f32() / secs).clamp(0.0, 1.0);
        let to = if self.open { 1.0 } else { 0.0 };
        self.progress = from + (to - from) * ease(t);
        if t >= 1.0 {
            self.progress = to;
            self.slide = None;
        }
        true
    }

    /// Toggle, and begin sliding. Returns the new open state.
    pub fn toggle(&mut self, now: Instant) -> bool {
        self.open = !self.open;
        self.slide = Some((now, self.progress));
        self.open
    }

    /// Set the state with no animation.
    ///
    /// For `restore`: a slide on startup is motion the user did not ask for,
    /// and `animating` would be true before the first frame had been drawn.
    pub fn set_immediately(&mut self, open: bool) {
        self.open = open;
        // A restored width is not a drag: the content is laid out against it
        // from the first frame rather than snapping when something is released.
        self.committed = self.full;
        self.progress = if open { 1.0 } else { 0.0 };
        self.slide = None;
    }
}

#[cfg(test)]
mod tests;
