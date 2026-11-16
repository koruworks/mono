// SPDX-License-Identifier: MPL-2.0

//! Dragging a row of an indented list: where it would land, when a shut row
//! under it opens, and how fast the list crawls at its edges.
//!
//! The document tree and the tasks rail's Lists section are both an indented
//! column of equal-height rows that a person rearranges by dragging, and one
//! hand learns one gesture. What they share is arithmetic, and that is all
//! that is here: plain rows, a pointer and a clock in; a [`Target`], an opened
//! row or a scroll step out.
//!
//! **Which drops are allowed stays with the host.** A document nests into a
//! document and not into a file; a list nests into any list but its own
//! descendants. The host says so per row with [`Row::can_nest`] and by marking
//! what is being carried, and this module never learns what a row *is*.
//!
//! Nothing here draws, reorders or scrolls anything. The host keeps its rows,
//! its scroll offset and its slide.

use std::time::Instant;

/// One row of the column, as much as a drop needs to know about it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Row {
    /// Levels below the top; `0` is a top-level row.
    pub depth: usize,
    /// Part of what is being dragged. A carried row is never a target: the
    /// carried block follows the pointer, so the pointer is always over one
    /// of its own rows, and the question is which *other* row it has reached.
    pub carried: bool,
    /// Whether something may be dropped *into* this row. When it may not,
    /// the middle of the row falls back to the nearer edge instead of
    /// refusing the drop.
    pub can_nest: bool,
}

/// Where in a row the carried block sits.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Zone {
    /// Immediately before the row, as its sibling.
    Before,
    /// Immediately after the row's subtree, as its sibling.
    After,
    /// Inside the row, as its last child.
    Nest,
}

/// The resolved landing of a drag.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Target {
    /// Against the row at `index` of the slice that was asked about.
    Row { index: usize, zone: Zone },
    /// Every row is carried, so there is no other row to anchor to: the top
    /// level itself.
    Root,
}

/// The top quarter of a row is *before*, the bottom quarter *after*, and the
/// half between is *into*.
const EDGE: f32 = 0.25;

/// Where a carried block whose grabbed row is centred at `centre` would land.
///
/// `first_top` is where row `0`'s slot begins, scroll included, and every row
/// is `row_h` tall. `None` for an empty column.
///
/// - **Past the last row** lands after the last *top-level* row that is not
///   carried, however deep the last visible row is — so a column whose last
///   row is an open branch still lets a drag land after it at the top level
///   rather than inside the branch.
/// - Otherwise the target is the non-carried row whose slot centre is nearest,
///   and the zone is read from where `centre` falls in that slot.
pub fn drop_target(rows: &[Row], row_h: f32, first_top: f32, centre: f32) -> Option<Target> {
    if rows.is_empty() || row_h <= 0.0 {
        return None;
    }
    let last_bottom = first_top + rows.len() as f32 * row_h;
    if centre > last_bottom
        && let Some(index) = rows.iter().rposition(|row| row.depth == 0 && !row.carried)
    {
        return Some(Target::Row {
            index,
            zone: Zone::After,
        });
    }
    let mut best: Option<(usize, f32)> = None;
    for (i, row) in rows.iter().enumerate() {
        if row.carried {
            continue;
        }
        let slot_centre = first_top + (i as f32 + 0.5) * row_h;
        let dist = (centre - slot_centre).abs();
        match best {
            Some((_, nearest)) if dist >= nearest => {}
            _ => best = Some((i, dist)),
        }
    }
    let Some((index, _)) = best else {
        return Some(Target::Root);
    };
    let slot_top = first_top + index as f32 * row_h;
    let rel = ((centre - slot_top) / row_h).clamp(0.0, 1.0);
    let zone = if rel < EDGE {
        Zone::Before
    } else if rel > 1.0 - EDGE {
        Zone::After
    } else if rows[index].can_nest {
        Zone::Nest
    } else if rel < 0.5 {
        Zone::Before
    } else {
        Zone::After
    };
    Some(Target::Row { index, zone })
}

/// How far to scroll this frame while the pointer rests near an edge of the
/// rows' own band, in the wheel's sign: positive reveals what is above.
///
/// `top..bottom` is the band the rows are drawn in. Within `band` of either
/// end the list crawls, faster the nearer the pointer is to the edge, up to
/// `max_speed` pixels a second. A pointer outside the band — in a heading
/// above it, or over a pinned footer below — scrolls nothing.
pub fn edge_scroll(y: f32, top: f32, bottom: f32, band: f32, max_speed: f32, dt: f32) -> f32 {
    if band <= 0.0 {
        return 0.0;
    }
    if y >= top && y - top < band {
        ((band - (y - top)) / band).clamp(0.0, 1.0) * max_speed * dt
    } else if y < bottom && bottom - y < band {
        -((band - (bottom - y)) / band).clamp(0.0, 1.0) * max_speed * dt
    } else {
        0.0
    }
}

/// A shut row the drag is resting on as a [`Zone::Nest`] target, waiting to
/// open so the drag can descend into it.
///
/// `K` is whatever the host names a row by. The wait restarts whenever the
/// candidate changes, so passing over a row does not open it.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct SpringOpen<K> {
    pending: Option<(K, Instant)>,
}

impl<K> Default for SpringOpen<K> {
    fn default() -> Self {
        Self { pending: None }
    }
}

impl<K: Copy + PartialEq> SpringOpen<K> {
    /// Say what the drag rests on now: `Some` only for a shut row targeted
    /// with [`Zone::Nest`], `None` for anything else. The same candidate
    /// twice keeps the time it first arrived.
    pub fn rest_on(&mut self, candidate: Option<K>, now: Instant) {
        match (self.pending, candidate) {
            (Some((held, _)), Some(key)) if held == key => {}
            (_, Some(key)) => self.pending = Some((key, now)),
            (_, None) => self.pending = None,
        }
    }

    /// The row to open, once it has been rested on for `delay_secs`. Answers
    /// it once and forgets it.
    pub fn due(&mut self, now: Instant, delay_secs: f32) -> Option<K> {
        let (key, since) = self.pending?;
        (now.duration_since(since).as_secs_f32() >= delay_secs).then(|| {
            self.pending = None;
            key
        })
    }

    /// What is being waited on, if anything.
    pub fn pending(&self) -> Option<K> {
        self.pending.map(|(key, _)| key)
    }

    pub fn clear(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests;
