// SPDX-License-Identifier: MPL-2.0

use super::*;

const T: i64 = 1_800_000_000_000;

#[test]
fn an_item_is_removed_after_thirty_days_and_one_of_margin() {
    assert!(!expired(T, T + 29 * DAY_MS));
    assert!(!expired(T, T + 30 * DAY_MS));
    assert!(!expired(T, T + 31 * DAY_MS - 1));
    assert!(expired(T, T + 31 * DAY_MS));
    assert!(expired(T, T + 400 * DAY_MS));
}

#[test]
fn a_stamp_later_than_the_trusted_time_is_never_due() {
    // Written by a clock a year ahead of the one being trusted.
    assert!(!expired(T + 365 * DAY_MS, T));
    assert!(!expired(T + 1, T));
}

#[test]
fn the_notice_counts_thirty_days_down_to_removed_today() {
    assert_eq!(days_left(T, T), 30);
    assert_eq!(days_left(T, T + 3_600_000), 30);
    assert_eq!(days_left(T, T + DAY_MS), 29);
    assert_eq!(days_left(T, T + 29 * DAY_MS), 1);
    assert_eq!(days_left(T, T + 29 * DAY_MS + 1), 1);
    assert_eq!(days_left(T, T + 30 * DAY_MS - 1), 1);
    assert_eq!(days_left(T, T + 30 * DAY_MS), 0);
    assert_eq!(days_left(T, T + 45 * DAY_MS), 0);
}

#[test]
fn a_stamp_ahead_of_this_device_reads_as_just_trashed() {
    assert_eq!(days_left(T + DAY_MS, T), 30);
}
