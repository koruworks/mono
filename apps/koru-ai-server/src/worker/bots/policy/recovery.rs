// SPDX-License-Identifier: MPL-2.0
use std::time::{Duration, Instant};

/// Retry only retained prepared receipt pages; never a document scan or timer write.
pub(in crate::worker) struct PolicyRecovery {
    pub after: Option<i64>,
    retry_at: Option<Instant>,
    pub retry_needed: bool,
    round: u32,
}
impl Default for PolicyRecovery {
    fn default() -> Self {
        Self {
            after: Some(0),
            retry_at: None,
            retry_needed: false,
            round: 0,
        }
    }
}
impl PolicyRecovery {
    pub fn restart(&mut self) {
        self.after = Some(0);
        self.retry_at = None;
    }
    pub fn ready(&mut self, now: Instant) -> bool {
        if self.retry_at.is_some_and(|at| now < at) {
            return false;
        }
        self.retry_at = None;
        true
    }
    pub fn due(&self, now: Instant) -> bool {
        self.retry_at.is_some_and(|at| now >= at)
    }
    pub fn defer(&mut self, now: Instant) {
        self.after.get_or_insert(0);
        self.retry_at = Some(now + Duration::from_secs(1u64 << self.round.min(5)));
        self.round = (self.round + 1).min(5);
    }
    pub fn finish_pass(&mut self, now: Instant) {
        self.after = None;
        if self.retry_needed {
            self.retry_needed = false;
            self.defer(now);
        } else {
            self.round = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transient_failure_retries_without_user_wake_and_backoff_is_bounded() {
        let now = Instant::now();
        let mut recovery = PolicyRecovery::default();
        recovery.after = Some(99);
        recovery.retry_needed = true;
        recovery.finish_pass(now);
        assert!(!recovery.ready(now));
        assert!(!recovery.due(now + Duration::from_millis(999)));
        assert!(recovery.due(now + Duration::from_secs(1)));
        assert!(recovery.ready(now + Duration::from_secs(1)));
        assert_eq!(recovery.after, Some(0));
        recovery.finish_pass(now + Duration::from_secs(1));
        assert_eq!(recovery.after, None);
        for _ in 0..20 {
            recovery.defer(now);
        }
        assert!(!recovery.due(now + Duration::from_secs(31)));
        assert!(recovery.due(now + Duration::from_secs(32)));
    }
}
