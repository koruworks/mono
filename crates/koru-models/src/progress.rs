// SPDX-License-Identifier: MPL-2.0

//! **At most one progress event per 250 ms or per whole percent.**
//!
//! `koru_update::controller::download_verified` sends one `WorkerEvent::Progress`
//! per 64 KiB chunk. Over a 650 MB model that is a little under ten thousand
//! events, each of which crosses a channel, wakes a loop and may reach a log —
//! and `docs/contributing/operations/LOGGING.md` names per-chunk progress as the
//! anti-pattern by name. The type declaring [`InstallProgress`](crate::InstallProgress)
//! says the coalescing is the emitter's job; this is the emitter's job, in one
//! place, so that a second caller cannot get it subtly different.
//!
//! **It is a separate type from the download loop because the rule is
//! testable and the loop is not.** Driving ten thousand chunks through a real
//! socket to count events would be a slow test of the wrong thing; driving them
//! through [`Coalescer::wants`] with a clock the test controls is a fast test of
//! the rule itself.
//!
//! Two events are never suppressed: the first — a progress bar that appears at
//! 3% looks like a hang — and the last, because a bar that stops at 99% and then
//! jumps to a finished install is the same complaint in reverse. The caller
//! emits those directly rather than asking.

use std::time::{Duration, Instant};

/// One event per this much wall-clock time, when the percentage has not moved.
///
/// A 650 MB download over a slow link can spend a minute inside one percent, so
/// time is the floor and percent is the ceiling; either alone leaves a bar that
/// looks stuck.
pub(crate) const AT_MOST_EVERY: Duration = Duration::from_millis(250);

/// Whether enough has changed to be worth telling anyone.
#[derive(Clone, Debug)]
pub(crate) struct Coalescer {
    interval: Duration,
    last_at: Option<Instant>,
    last_percent: Option<u64>,
}

impl Coalescer {
    pub(crate) fn new() -> Coalescer {
        Coalescer {
            interval: AT_MOST_EVERY,
            last_at: None,
            last_percent: None,
        }
    }

    /// Ask, using the real clock.
    pub(crate) fn wants(&mut self, received: u64, total: u64) -> bool {
        self.wants_at(received, total, Instant::now())
    }

    /// Ask, with the clock passed in. **The whole rule**, and the reason it is
    /// a method taking a time rather than one reading it: a test that had to
    /// sleep 250 ms per assertion would be relaxed into asserting nothing.
    pub(crate) fn wants_at(&mut self, received: u64, total: u64, now: Instant) -> bool {
        let percent = percent(received, total);
        let moved = self.last_percent != Some(percent);
        let elapsed = self
            .last_at
            .is_none_or(|last| now.duration_since(last) >= self.interval);
        if moved || elapsed {
            self.last_at = Some(now);
            self.last_percent = Some(percent);
            return true;
        }
        false
    }

    /// Record an event the caller emitted without asking — the first and the
    /// last — so the next question is asked against it.
    pub(crate) fn emitted(&mut self, received: u64, total: u64) {
        self.last_at = Some(Instant::now());
        self.last_percent = Some(percent(received, total));
    }
}

/// Whole percent, and `0` for a total nobody declared — which is a real state:
/// a source may be copying a file whose length it has not asked for.
///
/// **The multiply is widened rather than saturated**, and that is not
/// fastidiousness about a size no model will ever be. `saturating_mul` clamps
/// *before* the divide, so a large enough pair answered `1` where it meant
/// `100` — the progress bar would have crawled at one percent for the whole
/// download, and every event would have passed the coalescer because the
/// percentage never moved. Found by
/// `an_unknown_total_is_zero_percent_rather_than_a_panic`, which asserts the
/// extremes for exactly this reason.
fn percent(received: u64, total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    ((received.min(total) as u128 * 100) / total as u128) as u64
}

#[cfg(test)]
mod tests;
