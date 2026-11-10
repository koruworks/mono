// SPDX-License-Identifier: MPL-2.0

//! The frames the local transport speaks to the delegate worker.
//!
//! One connection per call: the client writes one [`Request`], the worker
//! answers one [`Response`], and the connection closes — except a stream,
//! where the worker keeps writing [`Response::Item`] frames until
//! [`Response::End`]. `koru_ipc::frame` carries them; [`WIRE_VERSION`] is
//! stamped on every frame and a peer that reads another version closes.
//!
//! The worker's own verbs — the readiness hello the supervisor probes and
//! the authenticated stop — ride the same frames so the endpoint has one
//! reader.

use koru_delegate_core::{RunId, WorkspaceId};
use serde::{Deserialize, Serialize};

use crate::wire::{
    CancelRunResponse, CreateRunRequest, CreateRunResponse, CreateScheduleRequest,
    CreateScheduleResponse, EditScheduleRequest, EditScheduleResponse, Failure, ListRunsResponse,
    ListSchedulesResponse, PauseScheduleRequest, PauseScheduleResponse, PurgeRunResponse,
    RateRunRequest, RateRunResponse, RenameScheduleRequest, RenameScheduleResponse,
    RestoreRunResponse, ResumeRunResponse, ResumeScheduleRequest, ResumeScheduleResponse,
    RetireScheduleRequest, RetireScheduleResponse, RunStreamItem, RunView, ScheduleChange,
    TimelineQuery, TrashRunResponse,
};
use koru_delegate_core::ArtifactDisposition;

/// The frame version. Both sides are this same executable's build, so a
/// mismatch is a stale worker, and it is refused rather than reinterpreted.
// Version 9 adds an explicit fenced skip for a pending native question set.
// Version 8 adds private, positively classified Run tool authority.
// Version 7 adds the separate authenticated Bot-owner command channel.
// Version 6 adds the complete schedule-management vocabulary and named
// schedule projections. Version 5 added live schedule diagnostics. Version 4 added typed schedule
// create/list/watch frames. Version 3 added the
// authenticated, content-free native schedule wake. Version 2 preserved
// explicit model intent on creation and every later start.
pub const WIRE_VERSION: u8 = 9;

/// The product a caller names on its frames; a log line names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caller {
    pub product: String,
    pub trace_id: String,
}

/// The frames are tagged `frame`, not `kind`: a stream item is itself
/// tagged `kind`, and an internally tagged enum wrapping another with the
/// same tag key writes the key twice and refuses to read itself back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum Request {
    /// Separate Agent-run capability; Caller labels never authorize Bots.
    Bot {
        authority: crate::bots::BotDelegateAuthority,
        command: crate::bots::BotCommand,
    },
    /// Who is serving: the readiness probe.
    Hello,
    /// Stop, with the per-child authority the supervisor minted.
    Stop {
        authority: String,
    },
    /// Content-free control wake forwarded from the Agent's native
    /// timezone/resume notifications.
    WakeSchedules {
        authority: String,
    },
    CreateSchedule {
        caller: Caller,
        request: CreateScheduleRequest,
    },
    RenameSchedule {
        caller: Caller,
        request: RenameScheduleRequest,
    },
    EditSchedule {
        caller: Caller,
        request: EditScheduleRequest,
    },
    PauseSchedule {
        caller: Caller,
        request: PauseScheduleRequest,
    },
    ResumeSchedule {
        caller: Caller,
        request: ResumeScheduleRequest,
    },
    RetireSchedule {
        caller: Caller,
        request: RetireScheduleRequest,
    },
    ListSchedules {
        caller: Caller,
        workspace: WorkspaceId,
    },
    WatchSchedules {
        caller: Caller,
        workspace: WorkspaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<ScheduleChange>,
    },
    /// Ends the schedule watch carried by this same connection. It has no
    /// authority fields because admission already happened before Ready.
    CancelScheduleWatch,
    Create {
        caller: Caller,
        request: CreateRunRequest,
    },
    List {
        caller: Caller,
        workspace: WorkspaceId,
    },
    Run {
        caller: Caller,
        run: RunId,
        window: TimelineQuery,
    },
    Cancel {
        caller: Caller,
        run: RunId,
    },
    Resume {
        caller: Caller,
        run: RunId,
        /// §7.6's last row: a person's explicit request for a run whose
        /// prior attempt is unknown.
        #[serde(default)]
        confirm: bool,
    },
    Rate {
        caller: Caller,
        run: RunId,
        request: RateRunRequest,
    },
    Trash {
        caller: Caller,
        run: RunId,
        /// What to do with the run's results when the trash is emptied.
        #[serde(default)]
        artifacts: ArtifactDisposition,
    },
    Restore {
        caller: Caller,
        run: RunId,
    },
    Purge {
        caller: Caller,
        run: RunId,
    },
    Stream {
        caller: Caller,
        run: RunId,
        /// See [`crate::wire::TimelineQuery::workspace`].
        #[serde(default)]
        workspace: Option<WorkspaceId>,
        after: Option<u64>,
    },
}

/// What the worker says about itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The launch the supervisor minted; readiness proves it.
    pub launch_id: String,
    /// This worker's process id, so a test can end it.
    pub pid: u32,
    /// The device this worker executes runs on.
    pub device_id: String,
    /// Runs with a live attempt right now. A gauge, and the only one.
    pub active_runs: u32,
    /// What this worker has done since it started (P1.17).
    ///
    /// **On the hello and nowhere else.** The readiness verb is answered
    /// without the actor, so these numbers reach a person while every run
    /// is busy; and they ride a frame that already exists rather than a
    /// new verb, which is the same choice `koru-agent status` made when
    /// the gateway's three one-shot commands became one line of an answer
    /// that was already there.
    #[serde(default)]
    pub counters: WorkerCounters,
}

/// The worker's totals, as the wire carries them.
///
/// Declared here rather than shared with `koru-delegate-runtime`, for the
/// reason `koru_store_wire::internal::Metrics` is declared beside the
/// store's own counters rather than being them: this crate is the frames,
/// and the orchestrator is host-agnostic — an edge from one to the other
/// would put a transport in a library that must not have one. The host
/// maps between them in one place (`server.rs`), and both sides break
/// together when a field is renamed because both are typed.
///
/// Every field is a monotonic total since process start, so two readings
/// subtract into what happened in between. Counts only: no id, no path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCounters {
    pub intents_recorded: u64,
    pub attempts_opened: u64,
    pub contexts_captured: u64,
    pub captures_failed: u64,
    pub publications_queued: u64,
    pub artifacts_registered: u64,
    pub publications_acknowledged: u64,
    pub publication_retries: u64,
    pub restarts: u64,
    pub restart_completed_forward: u64,
    pub restart_started: u64,
    pub restart_reattached: u64,
    pub restart_reconciled: u64,
    pub restart_interrupted: u64,
    pub restart_unresolved: u64,
    pub schedule_wakes: u64,
    pub schedule_evaluations: u64,
    pub schedule_documents_evaluated: u64,
    pub schedule_evaluation_failures: u64,
    pub schedule_fired: u64,
    pub schedule_skipped_missed: u64,
    pub schedule_skipped_busy: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum Response {
    Bot(crate::bots::BotReply),
    Hello(Hello),
    Stopping,
    SchedulesWoken,
    ScheduleCreated(CreateScheduleResponse),
    ScheduleRenamed(RenameScheduleResponse),
    ScheduleEdited(EditScheduleResponse),
    SchedulePaused(PauseScheduleResponse),
    ScheduleResumed(ResumeScheduleResponse),
    ScheduleRetired(RetireScheduleResponse),
    Schedules(ListSchedulesResponse),
    ScheduleWatchReady,
    ScheduleChanged(ScheduleChange),
    ScheduleEnd,
    Created(CreateRunResponse),
    Runs(ListRunsResponse),
    Run(RunView),
    Cancelled(CancelRunResponse),
    Resumed(ResumeRunResponse),
    Rated(RateRunResponse),
    Trashed(TrashRunResponse),
    Restored(RestoreRunResponse),
    Purged(PurgeRunResponse),
    Item(RunStreamItem),
    End,
    Failure(Failure),
}
