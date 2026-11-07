// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn a_span_costs_nothing_when_the_flag_is_off() {
    // The tests run without `KORU_STORE_STATS`, so this is the real path.
    // If it ever became expensive it would be on every commit in the store.
    assert!(
        !enabled(),
        "the flag is set; this test cannot measure the off path"
    );
    let span = Span::start(names::COMMIT);
    assert!(span.at.is_none(), "a disabled span took a clock reading");
    span.stop();
}

#[test]
fn recording_while_disabled_accumulates_nothing() {
    record(names::COMMIT, std::time::Duration::from_millis(5));
    let all = buckets().lock().unwrap();
    assert!(all.is_empty(), "a disabled recorder kept a bucket");
}

#[test]
fn reporting_while_disabled_is_silent() {
    // Called on every shutdown, including the overwhelming majority where
    // nobody asked for statistics.
    report("nothing should appear");
}

#[test]
fn every_measurement_is_named_once() {
    let mut seen = names::ALL.to_vec();
    seen.sort_unstable();
    let before = seen.len();
    seen.dedup();
    assert_eq!(seen.len(), before, "two measurements share a name");
    assert_eq!(
        names::ALL.len(),
        10,
        "including durable authority inspection"
    );
}

#[test]
fn measurement_names_are_ascii() {
    // They go into a table printed to a Windows console, which reads a
    // BOM-less stream as ANSI — the same trap `koru-core::timing` records.
    for name in names::ALL {
        assert!(name.is_ascii(), "{name}");
    }
}
