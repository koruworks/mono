// SPDX-License-Identifier: MPL-2.0
//! Actor-local deadline state. Idle ticks do no SQL or filesystem work.
use crate::{Database, Error};
use koru_time_zone::SystemZone;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub(crate) struct Clock {
    zone: SystemZone,
    next: Option<Instant>,
    scan: Option<Option<Uuid>>,
    changed: bool,
    retry_after: Option<Instant>,
}
impl Clock {
    pub(crate) fn new() -> Self {
        Self {
            zone: SystemZone::detect(),
            next: None,
            scan: Some(None),
            changed: true,
            retry_after: None,
        }
    }
    pub(crate) fn changed(&mut self) {
        self.changed = true;
        self.retry_after = None;
    }
    pub(crate) fn wake(&mut self) {
        self.zone = SystemZone::detect();
        self.scan = Some(None);
        self.changed = true;
        self.retry_after = None;
    }
    pub(crate) fn poll(
        &mut self,
        db: &mut Database,
        now: i64,
        instant: Instant,
    ) -> Result<bool, Error> {
        if self.retry_after.is_some_and(|until| instant < until) {
            return Ok(false);
        }
        self.retry_after = None;
        if !self.changed && self.scan.is_none() && self.next.is_none_or(|next| instant < next) {
            return Ok(false);
        }
        let scanned = self.scan.is_some();
        let more = db.evaluate_routines(now, &self.zone, self.scan)?;
        self.scan = if scanned { more.map(Some) } else { None };
        self.next = db
            .routine_deadline()?
            .map(|due| instant + Duration::from_millis(due.saturating_sub(now).max(0) as u64));
        self.changed = false;
        Ok(true)
    }
    pub(crate) fn retry(&mut self, instant: Instant) {
        // Retain the exact startup/zone cursor. Future deadlines on the
        // remaining page may need recomputation even though not yet due.
        self.changed = true;
        self.retry_after = Some(instant + Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests;
