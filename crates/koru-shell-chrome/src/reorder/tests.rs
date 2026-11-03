// SPDX-License-Identifier: MPL-2.0

//! The reorder rule on a bare row of slots — no strip, no rail. The tab
//! strip's own drop tests (`tabs::tests`) are the same rule seen through its
//! layout, and they are unchanged.

use super::*;

/// Five slots 36 long with 4 between: the rail's squares, top to bottom.
fn squares() -> Vec<(f32, f32)> {
    (0..5).map(|i| (10.0 + i as f32 * 40.0, 36.0)).collect()
}

/// **Half over the one below, and it moves there; less, and it stays.** The
/// carried square starts on slot 1 (top 50). Its centre reaches slot 2's top
/// edge — half of it over slot 2 — after 18 of its own length plus the 4 of
/// gap.
#[test]
fn an_item_moves_once_half_of_it_is_over_its_neighbour() {
    let slots = squares();
    let centre = |top: f32| top + 18.0;
    let home = slots[1].0;
    assert_eq!(drop_index(&slots, 1, centre(home)), 1, "at rest");
    assert_eq!(drop_index(&slots, 1, centre(home + 21.0)), 1, "not half");
    assert_eq!(drop_index(&slots, 1, centre(home + 22.0)), 2, "half below");
    assert_eq!(drop_index(&slots, 1, centre(home - 21.0)), 1, "not half");
    assert_eq!(drop_index(&slots, 1, centre(home - 22.0)), 0, "half above");
}

/// Between two slots the item keeps the one it has, from either side — the
/// gap is nobody's, so it cannot flicker between the two.
#[test]
fn in_the_gap_an_item_keeps_its_slot() {
    let slots = squares();
    let gap = slots[2].0 + slots[2].1 + 2.0;
    assert_eq!(drop_index(&slots, 2, gap), 2);
    assert_eq!(drop_index(&slots, 3, gap), 3);
}

/// Thrown past either end it lands at that end, and there is always a real
/// index to land on.
#[test]
fn past_the_ends_it_lands_at_the_ends() {
    let slots = squares();
    assert_eq!(drop_index(&slots, 2, -500.0), 0);
    assert_eq!(drop_index(&slots, 2, 5000.0), 4);
    assert_eq!(drop_index(&[], 3, 10.0), 0, "no slots");
}

/// The carried item stays inside the row: never behind its start, never with
/// its far edge past the row's end — and on a row shorter than itself, at the
/// start.
#[test]
fn a_carried_item_stays_inside_the_row() {
    assert_eq!(carry(50.0, 10.0, 206.0, 36.0), 50.0, "inside");
    assert_eq!(carry(-80.0, 10.0, 206.0, 36.0), 10.0, "before the start");
    assert_eq!(carry(400.0, 10.0, 206.0, 36.0), 170.0, "past the end");
    assert_eq!(carry(30.0, 10.0, 20.0, 36.0), 10.0, "no room");
}
