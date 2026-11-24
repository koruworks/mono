// SPDX-License-Identifier: MPL-2.0

//! The lane: the window's own band at the top of the sidebar.
//!
//! ```text
//! [●●●]                          [🔍] [+] [≡]      a Mac: the traffic lights lead
//! [K]                            [🔍] [+] [≡]      Windows and Linux: the mark leads
//!  ^                               ^    ^   ^
//!  leading                      search new toggle
//! ```
//!
//! The lane is **window chrome that happens to sit in the pane**
//! (`docs/features/workspace-foundation/TECHNICAL.final.md` §5.1): a fifth
//! rect from [`crate::regions::solve`], the top `CAPTION_H` of the sidebar and
//! zero-width when the pane is shut. Its slots are named after their shape
//! and not their meaning, the rule this crate keeps for every slot: a
//! **leading** slot (the Koru mark, or the room a Mac's traffic lights take),
//! a flexible gap, and up to three squares at the trailing end — `search`,
//! `new` (zero-width when the host offers no primary create) and `toggle`.
//! What pressing each one means is the host's business.
//!
//! When the pane is shut the same controls move into the caption's **leading
//! chain** ([`crate::caption::leading_chain`]), in the order the canvas gives
//! them there: leading · toggle · search · new · divider · tabs. Both
//! arrangements read one [`Leading`] value, so a window cannot show New in one
//! place and not the other.
//!
//! One function returns every rect; drawing and hit-testing both read it, and
//! the tests hit every rect back to itself. Pure arithmetic: no `TextEngine`,
//! no palette.

use koru_draw::geometry::Rect;
use koru_draw::theme;

/// What sits in the lane's leading slot.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub enum Mark {
    /// Nothing: the slot is zero-width. The arrangement every window had
    /// before the lane existed, which is what keeps the old tests true.
    #[default]
    None,
    /// The Koru mark, drawn by the host, not a button. The slot is one caption
    /// square wide so the mark sits where a toggle would and the chain after
    /// it does not move when a platform swaps the mark for the lights.
    Mark,
    /// Room for the platform's own window buttons at the lane's left — a Mac's
    /// traffic lights. The width is the platform's; the host measures it.
    Lights(f32),
}

/// Which lane controls a window offers.
///
/// Carried as one value the way [`crate::banner::Slots`] is, so a caller
/// passes the same answer to the lane and to the caption's chain and the two
/// cannot disagree about whether there is a New button.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub struct Leading {
    pub mark: Mark,
    pub search: bool,
    pub new: bool,
    /// Whether the controls are folded into the caption's leading chain.
    /// **True while the pane is shut** — the lane is gone and its controls
    /// live at the caption's left end — and false while it is open, when the
    /// lane holds them and the caption's left end is bare, the tabs beginning
    /// at its edge (frames 1 and 9). A window with no lane at all (the
    /// Terminal shell) is always folded: its toggle has always been in the
    /// caption.
    pub folded: bool,
}

impl Leading {
    /// A window with no lane controls at all: the caption's chain is then the
    /// sidebar toggle alone, exactly as it was before the lane existed.
    pub const NONE: Self = Self {
        mark: Mark::None,
        search: false,
        new: false,
        folded: true,
    };
}

/// The width reserved left of the vertical header divider (`|`) in the lane so
/// the gap from the trailing icon chip to the divider matches the gap on the
/// tab side (`6.5 + 3.5 = 10 px`).
pub const DIVIDER_W: f32 = 4.0;

/// The lane's rects, left to right. A slot the window does not offer is a
/// **zero-width rect** at the place it would have been, the idiom every
/// collapsed thing in this crate uses, so a caller does one arithmetic and a
/// hit test must exclude it explicitly (`Rect::contains` is inclusive).
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct LaneRects {
    pub leading: Rect,
    pub toggle: Rect,
    pub search: Rect,
    pub new: Rect,
    /// A short vertical rule aligned with the pane seam before the tabs.
    pub divider: Rect,
}

/// A lane control keeps the caption's control width as the band grows taller.
/// Each icon remains centered in its full-height target.
pub fn square(_band: Rect) -> f32 {
    theme::CAPTION_CONTROL_W
}

/// The leading slot's width for a mark.
pub fn leading_w(band: Rect, mark: Mark) -> f32 {
    match mark {
        Mark::None => 0.0,
        Mark::Mark => square(band),
        Mark::Lights(w) => w.max(0.0),
    }
}

/// Lay the lane out: the leading slot (Koru mark on Windows/Linux, traffic
/// lights on macOS) at the left edge, and the trailing squares packed against
/// the right edge in the order `toggle · search · new · divider` immediately
/// before the tabs, with `divider` aligned to the pane's right edge.
///
/// When `leading.folded` is true (the explorer pane is shut while a persistent
/// rail stays visible), the lane holds only the leading slot; the controls and
/// divider fold into [`crate::caption::leading_chain`].
///
/// In a narrow open lane, optional squares **decline the room** before the
/// toggle — search first, then new — each drawn whole or not at all; the toggle
/// is always kept so an open pane can always be shut.
pub fn solve(lane: Rect, leading: Leading) -> LaneRects {
    let s = square(lane);
    // **The mark sits over the rail.** In the lane its slot is the rail's
    // width, so the mark is centred on the column of app squares under it;
    // in a bare folded caption with no rail under it, the slot stays one square.
    let lead_w = match leading.mark {
        Mark::Mark => theme::RAIL_W.max(s),
        _ => leading_w(lane, leading.mark),
    }
    .min(lane.w);
    let leading_rect = Rect::new(lane.x, lane.y, lead_w, lane.h);
    let right = lane.x + lane.w;
    if leading.folded {
        let zero_at_end = Rect::new(right, lane.y, 0.0, lane.h);
        return LaneRects {
            leading: leading_rect,
            toggle: zero_at_end,
            search: zero_at_end,
            new: zero_at_end,
            divider: zero_at_end,
        };
    }
    let floor = leading_rect.x + leading_rect.w;
    let avail = (right - floor).max(0.0);
    let has_chrome = leading.mark != Mark::None || leading.search || leading.new;
    let div_w = if has_chrome && avail >= s + DIVIDER_W {
        DIVIDER_W
    } else {
        0.0
    };
    let rem = (avail - div_w).max(0.0);
    // Toggle gets first claim on the room; then new, then search — each whole
    // or absent, never clipped. Packed right-to-left ending at `divider`:
    // `toggle · search · new · divider`.
    let toggle_w = s.min(rem);
    let rem_after_toggle = (rem - toggle_w).max(0.0);
    let new_w = if leading.new && rem_after_toggle >= s {
        s
    } else {
        0.0
    };
    let rem_after_new = (rem_after_toggle - new_w).max(0.0);
    let search_w = if leading.search && rem_after_new >= s {
        s
    } else {
        0.0
    };
    let divider = Rect::new(right - div_w, lane.y, div_w, lane.h);
    let new = Rect::new(divider.x - new_w, lane.y, new_w, lane.h);
    let search = Rect::new(new.x - search_w, lane.y, search_w, lane.h);
    let toggle = Rect::new(search.x - toggle_w, lane.y, toggle_w, lane.h);
    LaneRects {
        leading: leading_rect,
        toggle,
        search,
        new,
        divider,
    }
}

/// What is under a point in the lane. `None` is the empty lane — the part a
/// person drags the window by — and the leading slot counts as empty: the
/// mark is not a verb.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Hit {
    Search,
    New,
    Toggle,
}

/// Resolve a point already known to be inside the lane.
pub fn hit(lane: Rect, leading: Leading, x: f32, y: f32) -> Option<Hit> {
    let r = solve(lane, leading);
    let owns = |rect: Rect| rect.w > 0.0 && rect.contains(x, y);
    if owns(r.toggle) {
        Some(Hit::Toggle)
    } else if owns(r.new) {
        Some(Hit::New)
    } else if owns(r.search) {
        Some(Hit::Search)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
