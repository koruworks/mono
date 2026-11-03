// SPDX-License-Identifier: MPL-2.0

//! Drag-to-reorder along one axis: where a carried item may be drawn, and
//! which slot it belongs in.
//!
//! **The tab strip's rule, with the strip taken out.** It was written for tabs
//! — [`tabs::drop_index`](crate::tabs::drop_index) and
//! [`tabs::carry_x`](crate::tabs::carry_x) — and nothing in it was about tabs:
//! a row of slots, an item following the pointer, and the moment it changes
//! places. The apps rail down the sidebar's left edge is the same gesture on
//! the other axis, so the arithmetic lives here once and both read it. What
//! stays each surface's own is the record of the drag (the tab strip's also
//! carries a tear-off), and the slide its neighbours make is
//! `koru_app_shell::TabSlides`, which takes positions on whichever axis the
//! caller lays out on.
//!
//! A slot is `(start, len)` on that axis — a tab's left edge and width, a rail
//! square's top and height. Pure arithmetic over numbers, like everything else
//! in this crate: no font, no clock.

/// Which slot an item being carried belongs in, given its **centre** `centre`.
///
/// **The half-way rule.** The item swaps with a neighbour once its own centre
/// stands inside that neighbour's slot — which is to say once it covers half
/// of it — and not before; in the gap between two slots it keeps `from`. Its
/// centre rather than the pointer, because the item is what the eye judges
/// against and the two only coincide when it was grabbed dead centre. Past
/// either end it is the first or the last slot, so a throw lands at the end
/// rather than nowhere. `0` when there are no slots.
pub fn drop_index(slots: &[(f32, f32)], from: usize, centre: f32) -> usize {
    let (Some(first), Some(last)) = (slots.first(), slots.last()) else {
        return 0;
    };
    let last_index = slots.len() - 1;
    if centre <= first.0 {
        return 0;
    }
    if centre >= last.0 + last.1 {
        return last_index;
    }
    for (i, &(start, len)) in slots.iter().enumerate() {
        if centre >= start && centre <= start + len {
            return i;
        }
    }
    from.min(last_index)
}

/// Where a carried item `len` long may be drawn, given where the pointer would
/// put its leading edge: no further back than `lo`, and with its far edge no
/// further on than `hi`.
///
/// Past either end there is nowhere to land, so an item that kept following
/// would only wander over whatever is beside the row. When the room is shorter
/// than the item, `lo` wins.
pub fn carry(lead: f32, lo: f32, hi: f32, len: f32) -> f32 {
    lead.clamp(lo, (hi - len).max(lo))
}

#[cfg(test)]
mod tests;
