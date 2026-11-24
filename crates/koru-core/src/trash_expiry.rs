// SPDX-License-Identifier: MPL-2.0

//! When something in a trash is removed for good: both bins — the Workspace's
//! trashed documents and the rows in a document's own bin — empty themselves
//! 30 days after the item went in, and say so while it waits
//! (docs/cloud-platform/koru-free/COLLECTOR.final.md, rung 2;
//! ADR-000-trashempties).
//!
//! **The notice promises 30 days; the purge waits 31.** The extra day is the
//! margin between the clock the notice is drawn with (this device's) and the
//! clock the purge trusts, so "Removed today" is never a lie told late.
//!
//! **Whose clock decides.** The purge is permanent and syncs, so one device
//! whose clock runs a year ahead must not empty every member's trash. For a
//! Cloud Workspace the caller passes a time the Cloud vouched for — the issue
//! time of the credential its last successful session used — and an item is
//! purged only when its stamp is not later than that time. A local-only
//! Workspace passes the device clock; a wrong clock there reaches only that
//! person's own device.

/// One day, in milliseconds.
pub const DAY_MS: i64 = 86_400_000;

/// What each trash promises: "Removed in N days" counts down from this.
pub const NOTICE_DAYS: i64 = 30;

/// How long an item stays before the sweep removes it: the notice plus one
/// day of margin.
pub const PURGE_AFTER_MS: i64 = (NOTICE_DAYS + 1) * DAY_MS;

/// Whether an item trashed at `trashed_at_ms` is due for removal by
/// `trusted_now_ms`.
///
/// A stamp later than the trusted time is never due: it was written by a clock
/// ahead of the one being trusted, and its age is unknown. An item with no
/// stamp at all — binned by a build that did not record when — is not handled
/// here; callers keep it, because its age cannot be known either.
pub fn expired(trashed_at_ms: i64, trusted_now_ms: i64) -> bool {
    trashed_at_ms <= trusted_now_ms && trusted_now_ms - trashed_at_ms >= PURGE_AFTER_MS
}

/// Whole days left in the notice for an item trashed at `trashed_at_ms`, as
/// this device sees it at `now_ms`: 30 on the day it went in, 0 on the last
/// day ("Removed today"), and 0 after that until the sweep runs.
///
/// A stamp ahead of this device's clock reads as just trashed.
pub fn days_left(trashed_at_ms: i64, now_ms: i64) -> u32 {
    let age = now_ms.saturating_sub(trashed_at_ms).max(0);
    let left = NOTICE_DAYS * DAY_MS - age;
    if left <= 0 {
        return 0;
    }
    // Ceiling, so an item binned an hour ago still has its 30 days.
    (left.div_euclid(DAY_MS) + i64::from(left.rem_euclid(DAY_MS) != 0)).min(NOTICE_DAYS) as u32
}

#[cfg(test)]
mod tests;
