// SPDX-License-Identifier: MPL-2.0

use super::*;

const NOW: i64 = 1_700_000_000_000;
const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;
const MONTH: i64 = 30 * DAY;

fn ago_of(secs: i64) -> String {
    ago(NOW - secs * 1000, NOW)
}

fn deleted_of(secs: i64) -> String {
    deleted_ago(Some(NOW - secs * 1000), NOW)
}

/// **A regression guard, not a specification.** The workspace manager's two
/// date columns depend on this exact wording, and `deleted_ago` was added as a
/// sibling rather than a flag on this so that changing one cannot change the
/// other by accident.
#[test]
fn ago_is_unchanged() {
    assert_eq!(ago_of(0), "Just now");
    assert_eq!(ago_of(59), "Just now");
    assert_eq!(ago_of(60), "1 minutes ago");
    assert_eq!(ago_of(3 * HOUR), "3 hours ago");
    assert_eq!(ago_of(DAY + 1), "Yesterday");
    assert_eq!(ago_of(3 * DAY), "3 days ago");
    assert_eq!(ago_of(2 * MONTH), "2 months ago");
    assert_eq!(
        ago(NOW + 10_000, NOW),
        "Just now",
        "a clock that ran backwards is not a negative age"
    );
}

#[test]
fn deleted_ago_says_today_yesterday_and_days() {
    assert_eq!(deleted_of(0), "Deleted today");
    assert_eq!(deleted_of(6 * HOUR), "Deleted today");
    assert_eq!(deleted_of(DAY + 1), "Deleted yesterday");
    assert_eq!(deleted_of(3 * DAY), "Deleted 3 days ago");
    assert_eq!(deleted_of(35 * DAY), "Deleted last month");
    assert_eq!(deleted_of(4 * MONTH), "Deleted 4 months ago");
}

/// An item binned by a build that recorded no time is a normal state. The column
/// says what it knows and no more.
#[test]
fn a_missing_timestamp_still_says_something() {
    assert_eq!(deleted_ago(None, NOW), "Deleted");
}

/// "1 nested items" is the sort of thing that makes a person distrust the count
/// beside it.
#[test]
fn the_nested_suffix_is_singular_at_one_and_absent_at_none() {
    assert_eq!(nested_suffix(0), None);
    assert_eq!(nested_suffix(1).as_deref(), Some("1 nested"));
    assert_eq!(nested_suffix(23).as_deref(), Some("23 nested"));
}

#[test]
fn bytes_reads_like_a_file_manager() {
    assert_eq!(bytes(0), "0 B");
    assert_eq!(bytes(999), "999 B");
    assert_eq!(bytes(1024), "1.0 KB");
    assert_eq!(bytes(1024 * 20), "20 KB");
    assert_eq!(bytes(1024 * 1024 * 3), "3.0 MB");
}

#[test]
fn the_trash_says_when_it_removes_an_item() {
    let when = |secs: i64| trash_when(Some(NOW - secs * 1000), NOW);
    assert_eq!(when(0), "Deleted today · Removed in 30 days");
    assert_eq!(when(3 * DAY), "Deleted 3 days ago · Removed in 27 days");
    assert_eq!(when(29 * DAY), "Deleted 29 days ago · Removed in 1 day");
    assert_eq!(when(30 * DAY), "Deleted last month · Removed today");
    assert_eq!(
        trash_when(None, NOW),
        deleted_ago(None, NOW),
        "no recorded time, no promise: the sweep keeps it"
    );
}
