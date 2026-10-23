// SPDX-License-Identifier: MPL-2.0

//! The apps row: one chip per app under the lane, the active app wearing its
//! label, and a *Manage apps* square at the end.
//!
//! ```text
//! [⌂ Home] [▤] [☑] [☰] [⚙]                 [⊞]
//!   ^ active, labelled   ^ icons   ^ manage apps
//! ```
//!
//! **What the row does when it does not fit** (QUESTIONS.md Q-J): labels
//! give way first — the active app is always labelled, the others only from
//! `LABELS_FROM` and only when every label fits on one line — and when the
//! icons themselves do not fit, the row **wraps** to another line. Nothing is
//! clipped and every app stays one click away, which is §3's promise that
//! nothing a person has today moves out of reach.
//!
//! Pure arithmetic: the chips are laid out from cached label widths, so the
//! hit test never needs a `TextEngine`. A label whose width is not known yet
//! (before the first draw, or in a test) lays out as an icon-only chip.

use koru_draw::geometry::Rect;

use crate::panes::explore::NavigatorMode;

/// A chip's side — the icon squares on the canvas, and the height of a
/// labelled chip. Two pixels more than a nav row, because a chip is a
/// control and a row is a line.
pub const SQUARE: f32 = 28.0;
/// Between chips, and between lines when the row wraps.
pub const GAP: f32 = 2.0;
/// Inside a labelled chip: icon, then this gap, then the label, then the
/// trailing pad.
pub const LABEL_GAP: f32 = 6.0;
pub const LABEL_PAD_RIGHT: f32 = 10.0;
/// From this pane width every app wears its label (Decision 4, in the
/// workspace window only), provided every label then fits on one line.
pub const LABELS_FROM: f32 = 400.0;

/// One app the row shows, with its label's measured width — `0.0` while
/// unknown, which lays out as icon-only.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Item {
    pub mode: NavigatorMode,
    pub label_w: f32,
}

/// One chip, placed.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Chip {
    pub mode: NavigatorMode,
    pub rect: Rect,
    pub labelled: bool,
}

/// The row, placed.
#[derive(Clone, PartialEq, Debug)]
pub struct Layout {
    pub chips: Vec<Chip>,
    /// The *Manage apps* square, at the end of the last line.
    pub manage: Rect,
    /// How tall the row is: one line, or more when it wrapped.
    pub height: f32,
    /// How many lines the chips took.
    pub lines: usize,
}

fn chip_w(item: Item, labelled: bool) -> f32 {
    if labelled && item.label_w > 0.0 {
        SQUARE + LABEL_GAP + item.label_w + LABEL_PAD_RIGHT
    } else {
        SQUARE
    }
}

/// Whether every chip, labelled, plus the manage square fits on one line of
/// `width`.
fn all_labelled_fit(items: &[Item], width: f32) -> bool {
    let chips: f32 = items.iter().map(|i| chip_w(*i, true)).sum();
    let gaps = items.len() as f32 * GAP;
    chips + gaps + SQUARE <= width
}

/// Lay the row out in `band` — its top-left and width; the height is the
/// answer, not an input. `active` is the app that wears its label; `wide`
/// is whether the pane is at or past [`LABELS_FROM`] (the caller says, so a
/// standalone window can say no).
pub fn solve(band: Rect, items: &[Item], active: NavigatorMode, wide: bool) -> Layout {
    let width = band.w.max(0.0);
    let everyone = wide && all_labelled_fit(items, width);
    let mut chips = Vec::with_capacity(items.len());
    let mut x = band.x;
    let mut y = band.y;
    let mut lines = 1usize;
    let right = band.x + width;
    let place = |w: f32, x: &mut f32, y: &mut f32, lines: &mut usize| -> Rect {
        // A chip that does not fit on this line starts the next one — unless
        // it is the first thing on the line, in which case it takes what room
        // there is rather than an endless staircase of empty lines.
        if *x > band.x && *x + w > right {
            *x = band.x;
            *y += SQUARE + GAP;
            *lines += 1;
        }
        let r = Rect::new(*x, *y, w.min((right - *x).max(0.0)), SQUARE);
        *x += w + GAP;
        r
    };
    for item in items {
        let labelled = item.label_w > 0.0 && (item.mode == active || everyone);
        let w = chip_w(*item, labelled);
        let rect = place(w, &mut x, &mut y, &mut lines);
        chips.push(Chip {
            mode: item.mode,
            rect,
            labelled,
        });
    }
    let manage = place(SQUARE, &mut x, &mut y, &mut lines);
    Layout {
        chips,
        manage,
        height: lines as f32 * SQUARE + (lines as f32 - 1.0) * GAP,
        lines,
    }
}

/// The chip under a point, if any.
pub fn chip_at(layout: &Layout, x: f32, y: f32) -> Option<NavigatorMode> {
    layout
        .chips
        .iter()
        .find(|c| c.rect.w > 0.0 && c.rect.contains(x, y))
        .map(|c| c.mode)
}

#[cfg(test)]
mod tests;
