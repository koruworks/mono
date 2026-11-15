// SPDX-License-Identifier: MPL-2.0

//! The typed boundary of `/v1/runs`, and the transport seam behind it.
//!
//! Three things, each in its own file:
//!
//! - [`wire`] — the bodies a route reads and writes: the request that
//!   creates a run, the run's header and a window of its history as the
//!   vocabulary crate spells them, the stream items, and the one failure
//!   shape every Koru domain answers with (`{ code, message, retryable }`).
//! - [`DelegatePort`] — what the gateway's `runs.rs` calls. The routes speak
//!   this trait and nothing else, so **D12's cloud worker is a second
//!   implementation of one trait**, and the gateway does not change.
//! - [`LocalDelegate`] — the implementation for this device: one private
//!   IPC connection per call to `koru-agent.exe --worker delegate`, the way
//!   the gateway's session bridge reaches the sessions worker. The frames
//!   are [`ipc`]'s; the worker's server side speaks the same types.
//!
//! **Capability and scopes.** `GET /v1/status` carries [`CAPABILITY`]
//! when the host composed a delegate transport; `runs.read` and
//! `runs.write` are the two scopes, both in the gateway's ordinary default
//! set — a window creates a run and reads its activity, and a person can
//! see both.

pub mod bots;
pub mod decision_bridge;
pub mod ipc;
mod local;
mod port;
pub mod routines;
pub mod wire;

pub use local::LocalDelegate;
pub use port::{
    DelegatePort, DelegatePrincipal, DelegateScope, RequestContext, RunStream, ScheduleStream,
    ScheduleStreamCancellation,
};
pub use wire::{
    CancelRunResponse, CreateRunRequest, CreateRunResponse, CreateScheduleRequest,
    CreateScheduleResponse, EditScheduleRequest, EditScheduleResponse, Failure, ListRunsQuery,
    ListRunsResponse, ListSchedulesQuery, ListSchedulesResponse, PauseScheduleRequest,
    PauseScheduleResponse, PurgeRunResponse, RenameScheduleRequest, RenameScheduleResponse,
    RestoreRunResponse, ResumeRunRequest, ResumeRunResponse, ResumeScheduleRequest,
    ResumeScheduleResponse, RetireScheduleRequest, RetireScheduleResponse, RunStreamItem, RunView,
    ScheduleChange, ScheduleFiringView, ScheduleNextFiring, ScheduleView, TimelineItem,
    TimelineQuery, TimelineWindow, TrashRunRequest, TrashRunResponse, TrashedRun,
};

/// The capability the gateway publishes when it composed a delegate
/// transport. Absent when it did not, and delegated run/schedule routes then
/// answer 503 `delegate_unavailable`.
pub const CAPABILITY: &str = "delegate";

/// The largest delegated run or schedule body the gateway reads.
pub const REQUEST_BODY_LIMIT: usize = 64 * 1024;

#[cfg(test)]
mod tests;
