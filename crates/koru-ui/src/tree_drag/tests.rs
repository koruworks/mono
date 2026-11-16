// SPDX-License-Identifier: MPL-2.0

use std::time::{Duration, Instant};

use super::*;

const H: f32 = 24.0;

fn row(depth: usize) -> Row {
    Row {
        depth,
        carried: false,
        can_nest: true,
    }
}

fn carried(depth: usize) -> Row {
    Row {
        carried: true,
        ..row(depth)
    }
}

/// The centre of slot `i`, moved `frac` of a row down from its top.
fn at(i: usize, frac: f32) -> f32 {
    100.0 + (i as f32 + frac) * H
}

fn target(rows: &[Row], centre: f32) -> Option<Target> {
    drop_target(rows, H, 100.0, centre)
}

#[test]
fn a_rows_quarters_are_before_into_and_after() {
    let rows = [row(0), row(0), row(0)];
    for (frac, zone) in [
        (0.1, Zone::Before),
        (0.3, Zone::Nest),
        (0.5, Zone::Nest),
        (0.7, Zone::Nest),
        (0.9, Zone::After),
    ] {
        assert_eq!(
            target(&rows, at(1, frac)),
            Some(Target::Row { index: 1, zone }),
            "{frac}"
        );
    }
}

#[test]
fn a_row_that_cannot_take_a_child_splits_its_middle_between_its_edges() {
    let mut rows = [row(0), row(0)];
    rows[1].can_nest = false;
    assert_eq!(
        target(&rows, at(1, 0.4)),
        Some(Target::Row {
            index: 1,
            zone: Zone::Before
        })
    );
    assert_eq!(
        target(&rows, at(1, 0.6)),
        Some(Target::Row {
            index: 1,
            zone: Zone::After
        })
    );
}

/// The carried block is always under the pointer, so it is skipped: the
/// answer is the nearest row that is *not* being carried.
#[test]
fn a_carried_row_is_never_the_target() {
    let rows = [row(0), carried(0), row(0)];
    let Some(Target::Row { index, .. }) = target(&rows, at(1, 0.5)) else {
        panic!("a row target");
    };
    assert_ne!(index, 1);
    assert_eq!(
        target(&rows, at(1, 0.8)),
        Some(Target::Row {
            index: 2,
            zone: Zone::Before
        }),
        "nearer the row below, and above its top edge"
    );
}

#[test]
fn past_the_end_lands_after_the_last_top_level_row() {
    let rows = [row(0), row(0), row(1), row(2)];
    assert_eq!(
        target(&rows, at(4, 0.5)),
        Some(Target::Row {
            index: 1,
            zone: Zone::After
        }),
        "not inside the open branch the column ends with"
    );
    let rows = [row(0), carried(0), carried(1)];
    assert_eq!(
        target(&rows, at(3, 0.5)),
        Some(Target::Row {
            index: 0,
            zone: Zone::After
        }),
        "and never after the carried block itself"
    );
}

#[test]
fn carrying_every_row_lands_on_the_top_level() {
    assert_eq!(
        target(&[carried(0), carried(1)], at(0, 0.5)),
        Some(Target::Root)
    );
    assert_eq!(target(&[], at(0, 0.5)), None);
}

#[test]
fn the_list_crawls_only_inside_its_own_edge_bands() {
    let (top, bottom, band, speed, dt) = (100.0, 500.0, 40.0, 600.0, 0.5);
    let step = |y| edge_scroll(y, top, bottom, band, speed, dt);
    assert_eq!(step(300.0), 0.0, "the middle is still");
    assert_eq!(step(100.0), 300.0, "full speed at the top edge");
    assert_eq!(step(120.0), 150.0, "half way into the band, half speed");
    assert_eq!(step(480.0), -150.0, "and downwards at the bottom");
    assert_eq!(step(90.0), 0.0, "a heading above the rows scrolls nothing");
    assert_eq!(step(510.0), 0.0, "nor does a footer below them");
}

#[test]
fn a_shut_row_opens_only_after_being_rested_on() {
    let t0 = Instant::now();
    let later = |ms| t0 + Duration::from_millis(ms);
    let mut spring = SpringOpen::default();

    spring.rest_on(Some(7), t0);
    assert_eq!(spring.due(later(100), 0.3), None, "passing over opens nothing");
    spring.rest_on(Some(7), later(200));
    assert_eq!(
        spring.due(later(300), 0.3),
        Some(7),
        "resting keeps the first arrival's time"
    );
    assert_eq!(spring.due(later(900), 0.3), None, "answered once");

    spring.rest_on(Some(7), later(1000));
    spring.rest_on(Some(8), later(1200));
    assert_eq!(
        spring.due(later(1400), 0.3),
        None,
        "a new row starts its own wait"
    );
    spring.rest_on(None, later(1450));
    assert_eq!(spring.pending(), None);
    assert_eq!(spring.due(later(5000), 0.3), None);
}
