// SPDX-License-Identifier: MPL-2.0

//! The two clocks, and the preamble bound to the installation.
//!
//! What was here before is gone: the MCP endpoint that named a URL and
//! issued no authority, and the registrations that always answered
//! "nothing". Both are real in `authority.rs` now, against P1.11's run
//! authority — a stub that says a run registered nothing is a stub that
//! makes every run finish needing attention.
//!
//! - [`InstalledPreamble`] is P1.12's resolver bound to the installation.
//! - [`WallClock`] and [`ThreadSleep`] are the two clocks the orchestrator
//!   asks a host for, so library code never reads the clock or sleeps.

use std::time::Duration;

use koru_delegate_runtime::{Clock, Preamble, ScheduleClock, Sleep};

pub struct InstalledPreamble {
    installation_dir: std::path::PathBuf,
    orientation_override: Option<std::path::PathBuf>,
}

impl InstalledPreamble {
    pub fn new(installation_dir: std::path::PathBuf) -> Self {
        Self {
            installation_dir,
            orientation_override: None,
        }
    }

    pub fn with_orientation_override(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.orientation_override = path;
        self
    }
}

impl Preamble for InstalledPreamble {
    fn resolve(&mut self) -> koru_delegate_preamble::Resolved {
        koru_delegate_preamble::resolve(&self.installation_dir)
    }

    fn orientation(&mut self) -> koru_delegate_preamble::Resolved {
        koru_delegate_preamble::session::resolve(
            &self.installation_dir,
            self.orientation_override.as_deref(),
        )
    }
}

pub struct WallClock;

pub(crate) fn wall_now_ms() -> i64 {
    #[cfg(feature = "test-support")]
    if let Some(path) = std::env::var_os("DELEGATE_PROCESS_TEST_CLOCK") {
        // One source feeds both the scheduler deadline and its final
        // post-preflight revalidation. A bad fixture stays behind the first
        // epoch rather than falling through to a nondeterministic real clock.
        return std::fs::read_to_string(path)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0);
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl Clock for WallClock {
    fn now_ms(&mut self) -> i64 {
        wall_now_ms()
    }
}

// Activation timestamps and active-budget checks must share one clock domain,
// including the existing controlled clock in opt-in process fixtures.
impl koru_delegate_sqlite::Clock for WallClock {
    fn now_ms(&self) -> i64 {
        wall_now_ms()
    }
}

impl ScheduleClock for WallClock {
    fn now_ms(&mut self, _evaluation_now_ms: i64) -> i64 {
        wall_now_ms()
    }
}

pub struct ThreadSleep;

impl Sleep for ThreadSleep {
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}
