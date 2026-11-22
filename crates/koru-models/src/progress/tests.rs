// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The number the roadmap names: a 650 MB model in 64 KiB chunks is 9,918
/// updates, and `koru_update` emits an event for every one of them. Driven
/// through the rule with a clock that never moves, all that survives is one per
/// whole percent.
#[test]
fn ten_thousand_chunks_become_a_hundred_events() {
    const TOTAL: u64 = 650 * 1024 * 1024;
    const CHUNK: u64 = 64 * 1024;
    let mut coalescer = Coalescer::new();
    let frozen = Instant::now();
    let mut emitted = 0;
    let mut received = 0;
    while received < TOTAL {
        received = (received + CHUNK).min(TOTAL);
        if coalescer.wants_at(received, TOTAL, frozen) {
            emitted += 1;
        }
    }
    assert!(
        emitted <= 101,
        "{emitted} events for {} chunks — the anti-pattern LOGGING.md names",
        TOTAL / CHUNK
    );
    assert!(emitted >= 100, "a progress bar that never moves is worse");
}

/// Percent is the ceiling and time is the floor. A download that spends a
/// minute inside one percent must still say something, or the bar reads as a
/// hang — which is the half a percent-only rule gets wrong.
#[test]
fn time_alone_is_enough_when_the_percentage_has_not_moved() {
    let mut coalescer = Coalescer::new();
    let start = Instant::now();
    assert!(coalescer.wants_at(0, 1_000_000, start));
    assert!(
        !coalescer.wants_at(10, 1_000_000, start + Duration::from_millis(200)),
        "same percent, inside the interval"
    );
    assert!(
        coalescer.wants_at(20, 1_000_000, start + AT_MOST_EVERY),
        "same percent, but the interval has passed"
    );
}

/// And percent alone is enough inside the interval, which is the half a
/// time-only rule gets wrong on a fast link.
#[test]
fn a_whole_percent_is_enough_inside_the_interval() {
    let mut coalescer = Coalescer::new();
    let start = Instant::now();
    assert!(coalescer.wants_at(0, 100, start));
    assert!(!coalescer.wants_at(0, 100, start + Duration::from_millis(1)));
    assert!(coalescer.wants_at(1, 100, start + Duration::from_millis(2)));
}

/// A total nobody declared is a real state, not a division by zero.
#[test]
fn an_unknown_total_is_zero_percent_rather_than_a_panic() {
    assert_eq!(percent(0, 0), 0);
    assert_eq!(percent(500, 0), 0);
    assert_eq!(percent(u64::MAX, u64::MAX), 100);
    assert_eq!(percent(200, 100), 100, "more than all of it is still all");
}
