// SPDX-License-Identifier: MPL-2.0
//! Headless equivalent of the desktop's clock-change and resume edges.
//!
//! The server already samples every twenty seconds. Comparing clocks and the
//! local zone there avoids turning each idle tick into an engine-wide scan.
use std::time::{Duration, Instant, SystemTime};

use koru_time_zone::SystemZone;

pub(super) struct Sample {
    wall: SystemTime,
    monotonic: Instant,
    zone: SystemZone,
}
impl Sample {
    pub(super) fn now() -> Self {
        // Zone detection can take longer than reading the two clocks. Keep
        // their reads together so that detection time does not look like drift.
        let zone = SystemZone::detect();
        Self {
            wall: SystemTime::now(),
            monotonic: Instant::now(),
            zone,
        }
    }
}

pub(super) struct RoutineClock {
    anchor: Sample,
    last_tick: Instant,
}
impl RoutineClock {
    pub(super) fn new(anchor: Sample) -> Self {
        Self {
            last_tick: anchor.monotonic,
            anchor,
        }
    }

    /// Ignore read jitter, but retain the anchor so gradual wall-clock slew
    /// eventually wakes too. A long gap also covers platforms whose monotonic
    /// clock includes suspend time. Neither path scans on ordinary idle ticks.
    pub(super) fn changed(&mut self, sample: Sample) -> bool {
        let expected = self.anchor.wall.checked_add(
            sample
                .monotonic
                .saturating_duration_since(self.anchor.monotonic),
        );
        let drifted = expected.is_none_or(|expected| {
            sample
                .wall
                .duration_since(expected)
                .unwrap_or_else(|backwards| backwards.duration())
                >= Duration::from_secs(1)
        });
        let interrupted =
            sample.monotonic.saturating_duration_since(self.last_tick) > super::DELEGATE_TICK * 2;
        let changed = drifted || interrupted || sample.zone != self.anchor.zone;
        self.last_tick = sample.monotonic;
        if changed {
            self.anchor = sample;
        }
        changed
    }
}

#[cfg(test)]
mod tests;
