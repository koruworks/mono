// SPDX-License-Identifier: MPL-2.0
use super::*;

fn sample(start: Instant, monotonic_ms: u64, wall_ms: u64, zone: &str) -> Sample {
    Sample {
        wall: SystemTime::UNIX_EPOCH + Duration::from_millis(wall_ms),
        monotonic: start + Duration::from_millis(monotonic_ms),
        zone: SystemZone::from_name(&koru_core::time::TimeZoneName::parse(zone).unwrap()).unwrap(),
    }
}

#[test]
fn normal_idle_ticks_and_sampling_jitter_do_not_wake_the_engine() {
    let start = Instant::now();
    let mut clock = RoutineClock::new(sample(start, 0, 0, "UTC"));
    for tick in 1..=180 {
        let at = tick * 20_000;
        assert!(!clock.changed(sample(start, at, at + tick % 5, "UTC")));
    }
}

#[test]
fn forward_and_backward_steps_each_wake_once_then_reanchor() {
    let start = Instant::now();
    let mut clock = RoutineClock::new(sample(start, 0, 100_000, "UTC"));
    assert!(clock.changed(sample(start, 20_000, 180_000, "UTC")));
    assert!(!clock.changed(sample(start, 40_000, 200_000, "UTC")));
    assert!(clock.changed(sample(start, 60_000, 160_000, "UTC")));
    assert!(!clock.changed(sample(start, 80_000, 180_000, "UTC")));
}

#[test]
fn gradual_slew_is_compared_to_the_retained_anchor() {
    let start = Instant::now();
    let mut clock = RoutineClock::new(sample(start, 0, 0, "UTC"));
    for tick in 1..=3 {
        assert!(!clock.changed(sample(start, tick * 20_000, tick * 20_250, "UTC")));
    }
    assert!(clock.changed(sample(start, 80_000, 81_000, "UTC")));
    assert!(!clock.changed(sample(start, 100_000, 101_000, "UTC")));
}

#[test]
fn resume_is_seen_whether_monotonic_time_includes_sleep_or_not() {
    let start = Instant::now();
    for monotonic in [20_000, 120_000] {
        let mut clock = RoutineClock::new(sample(start, 0, 0, "UTC"));
        assert!(clock.changed(sample(start, monotonic, 120_000, "UTC")));
        assert!(!clock.changed(sample(start, monotonic + 20_000, 140_000, "UTC")));
    }
}

#[test]
fn a_zone_change_wakes_even_when_the_current_utc_offsets_match() {
    let start = Instant::now();
    let mut clock = RoutineClock::new(sample(start, 0, 0, "UTC"));
    // London and UTC agree at this winter instant; future floating rules differ.
    assert!(clock.changed(sample(start, 20_000, 20_000, "Europe/London")));
    assert!(!clock.changed(sample(start, 40_000, 40_000, "Europe/London")));
}
