// SPDX-License-Identifier: MPL-2.0
//! Opt-in, in-memory composition support. Never enables worker readiness overrides.

#[cfg(feature = "fixtures")]
pub use koru_ai_sqlite::Repository;

/// A sink that keeps the Bot events a worker, or the gateway's Bot ingress,
/// published (M8), and the check that none of them holds content.
pub use crate::worker::bots::events::recorder::{
    BotEventRecorder, RecordedBotEvent, assert_holds_none_of,
};

/// The private-connection world of M5: scripted vendor, credential store and
/// delegate over the real ai.db writer on an in-memory database.
pub mod connections;

// Unix IPC endpoints use filesystem sockets. Only the named-pipe fixture is
// qualified here; portable repository and pure owner tests remain independent.
#[cfg(windows)]
pub(crate) mod catalog;
#[cfg(all(windows, feature = "fixtures"))]
mod routine;
#[cfg(all(windows, feature = "fixtures"))]
pub use routine::{RoutineCaptureHold, RoutineWorkerFixture};
