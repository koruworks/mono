// SPDX-License-Identifier: MPL-2.0
//! Opt-in, in-memory composition support. Never enables worker readiness overrides.

#[cfg(feature = "fixtures")]
pub use koru_ai_sqlite::Repository;

// Unix IPC endpoints use filesystem sockets. Only the named-pipe fixture is
// qualified here; portable repository and pure owner tests remain independent.
#[cfg(windows)]
pub(crate) mod catalog;
#[cfg(all(windows, feature = "fixtures"))]
mod routine;
#[cfg(all(windows, feature = "fixtures"))]
pub use routine::{RoutineCaptureHold, RoutineWorkerFixture};
