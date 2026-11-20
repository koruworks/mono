// SPDX-License-Identifier: MPL-2.0

//! The bodies of `/v1/runs`.
//!
//! A run's header and history cross as the vocabulary crate spells them —
//! [`RunHeader`], [`TimelineEntry`], [`UsageFact`], [`RunStatus`] — with
//! their JSON Schema on, the way the artifact wire re-exports its core.
//! What this file adds is the request that creates a run, the envelopes a
//! listing and a window travel in, the stream's items, and the one failure
//! shape. Nothing here carries a path, a document's text or a provider
//! payload; the objective is the one prompt a run holds, and it travels in
//! the request body as the redacted [`Objective`] newtype.

use std::collections::BTreeMap;

use koru_delegate_core::{
    ActorRef, ArtifactDisposition, FolderOutcome, Objective, Rating, RatingNote, RunContextItem,
    RunId, RunStatus, SessionId, SessionProfileKey, WorkspaceId, WorkspaceScope,
};
use koru_delegate_schedule::{
    DelegationTemplate, RecurrenceRule, ScheduleFiring, ScheduleFiringOutcome, ScheduleHeader,
    ScheduleName, ScheduleState,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use koru_delegate_core::{DocumentId, RunHeader, TimelineEntry, UsageFact};
// Run view DTOs are shared vocabulary for the ordinary public API.
pub use koru_delegate_core::{RunExecution, RunView, TimelineItem, TimelineWindow};

/// `POST /v1/schedules`. The client mints the stable schedule identity and
/// supplies only reusable intent. The worker stamps owner, revision,
/// lifecycle, creation time, and revision-effective time.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateScheduleRequest {
    pub schedule: koru_delegate_core::ScheduleId,
    pub workspace: WorkspaceId,
    pub name: ScheduleName,
    pub template: DelegationTemplate,
    pub recurrence: RecurrenceRule,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RenameScheduleRequest {
    pub schedule: koru_delegate_core::ScheduleId,
    pub workspace: WorkspaceId,
    pub expected_name: ScheduleName,
    pub name: ScheduleName,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EditScheduleRequest {
    pub schedule: koru_delegate_core::ScheduleId,
    pub workspace: WorkspaceId,
    pub expected_revision: u32,
    pub expected_name: ScheduleName,
    pub name: ScheduleName,
    pub template: DelegationTemplate,
    pub recurrence: RecurrenceRule,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PauseScheduleRequest {
    pub schedule: koru_delegate_core::ScheduleId,
    pub workspace: WorkspaceId,
    pub expected_revision: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ResumeScheduleRequest {
    pub schedule: koru_delegate_core::ScheduleId,
    pub workspace: WorkspaceId,
    pub expected_revision: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RetireScheduleRequest {
    pub schedule: koru_delegate_core::ScheduleId,
    pub workspace: WorkspaceId,
}

/// What a schedule list can truthfully say about its next occurrence.
/// A floating rule owned by another device has no exact instant here because
/// the schedule schema deliberately stores no owner timezone.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleNextFiring {
    Exact { at_ms: i64 },
    OwnerLocal,
}

/// One schedule firing as a surface may consume it. The schedule ledger mints
/// a planned RunId for every claim, including skips, but only a `fired` claim
/// denotes a Run. Skipped planned ids therefore never cross this boundary.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ScheduleFiringView {
    pub occurrence_ms: i64,
    pub revision: u32,
    pub outcome: ScheduleFiringOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy_holder: Option<RunId>,
    pub at: i64,
}

impl From<ScheduleFiring> for ScheduleFiringView {
    fn from(firing: ScheduleFiring) -> Self {
        let run = (firing.outcome == ScheduleFiringOutcome::Fired).then_some(firing.run);
        Self {
            occurrence_ms: firing.occurrence_ms,
            revision: firing.revision,
            outcome: firing.outcome,
            run,
            busy_holder: firing.busy_holder,
            at: firing.at,
        }
    }
}

/// One canonical schedule projection. `header` is the stored authored value;
/// `state` is the converged ownership/lifecycle reduction.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ScheduleView {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    /// Defaultable for the earlier unreleased v5 response shape. A missing
    /// value is displayed with the objective-derived compatibility fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<ScheduleName>,
    pub header: ScheduleHeader,
    pub state: ScheduleState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_firing: Option<ScheduleNextFiring>,
    pub firings: Vec<ScheduleFiringView>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateScheduleResponse {
    pub schedule: ScheduleView,
}

macro_rules! schedule_mutation_response {
    ($name:ident) => {
        #[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
        pub struct $name {
            pub schedule: ScheduleView,
        }
    };
}

schedule_mutation_response!(RenameScheduleResponse);
schedule_mutation_response!(EditScheduleResponse);
schedule_mutation_response!(PauseScheduleResponse);
schedule_mutation_response!(ResumeScheduleResponse);
schedule_mutation_response!(RetireScheduleResponse);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListSchedulesQuery {
    pub workspace: WorkspaceId,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListSchedulesResponse {
    pub schedules: Vec<ScheduleView>,
    /// At least one schedule-shaped Store entry could not be read while this
    /// projection was made. Healthy rows remain useful and visible; callers
    /// must not present this response as a complete inventory when true.
    #[serde(default)]
    pub incomplete: bool,
}

/// Opaque, content-free invalidation for a schedule list. `instance` changes
/// across worker restarts so a client never mistakes a reset counter for an
/// already-observed generation.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ScheduleChange {
    pub instance: uuid::Uuid,
    pub generation: u64,
}

/// `POST /v1/runs`. The client mints the run id, so a retried request is
/// the same run (§7.1); the executing device is the worker's own and is
/// not a field a caller supplies.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateRunRequest {
    pub run: RunId,
    /// The workspace the Run document is registered in.
    pub workspace: WorkspaceId,
    pub requested_by: ActorRef,
    pub objective: Objective,
    pub agent: SessionProfileKey,
    /// Explicit model intent. New clients use the v2 create route when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<koru_delegate_core::ModelName>,
    pub scope: WorkspaceScope,
    #[serde(default)]
    pub context: Vec<RunContextItem>,
    /// The workspaces the run's tools may reach (D28). Empty denies all
    /// workspace access.
    #[serde(default)]
    pub grants: std::collections::BTreeSet<WorkspaceId>,
}

/// `202 Accepted`: the request is durable and the run exists.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateRunResponse {
    pub run: RunId,
    pub document: DocumentId,
    pub status: RunStatus,
}

/// `GET /v1/runs?workspace=…`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListRunsQuery {
    pub workspace: WorkspaceId,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListRunsResponse {
    pub runs: Vec<RunHeader>,
    /// The provider session each listed run is being driven in **on the
    /// device that answered**, for the runs where there is one.
    ///
    /// The same fact [`RunExecution::session`] carries about a single run,
    /// and deliberately not a header field for the same reason: a header is
    /// what a run means on every device, and which session is driving it
    /// *here* is execution state (D3, D15). The listing carries it so a
    /// surface that draws every run — the Agents navigator — can put a run's
    /// session underneath the run without one `GET /v1/runs/{run}` per row.
    ///
    /// Absent for a run between attempts, for a run that has ended, and for
    /// every run another device is executing. `#[serde(default)]`, so a
    /// listing from a worker that predates the field reads as *no session
    /// known* rather than failing to decode.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sessions: BTreeMap<RunId, SessionId>,
    /// **The Run document each listed run is registered as.**
    ///
    /// A run is a document in the workspace's manifest, and deleting one is
    /// that document going into the workspace's bin like every other
    /// document in Koru — which is a manifest edit, and the manifest belongs
    /// to the window rather than to this worker. So the listing says which
    /// document each run is, and the window that asks for a run to be
    /// deleted has the id it needs to move it.
    ///
    /// `#[serde(default)]`, so a listing from a worker that predates the
    /// field reads as *no documents named* rather than failing to decode.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub documents: BTreeMap<RunId, DocumentId>,
    /// **The runs of this workspace that are in the trash**, newest
    /// deletion first. Absent from `runs`: a trashed run is not a run you
    /// are looking at any more, and the Agents navigator stops listing it
    /// the moment it goes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trashed: Vec<TrashedRun>,
}

/// One run in the trash, as the Trash tab draws it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TrashedRun {
    pub header: RunHeader,
    /// The Run document, so the window can restore or purge it.
    pub document: DocumentId,
    /// When it was put in the trash, as the manifest recorded it. `None`
    /// for a manifest entry that carries no timestamp — an older client
    /// wrote it — and the row then sorts to the bottom rather than claiming
    /// a moment nobody recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed_at_ms: Option<i64>,
    /// What the person said should happen to its results. `Keep` for a run
    /// nobody was asked about, which is every run that produced nothing.
    #[serde(default)]
    pub artifacts: ArtifactDisposition,
}

/// `POST /v1/runs/{run}/trash`: the run is going into the trash, and this
/// is the answer to the one question the trash asks about its results.
///
/// The body is optional and its one field defaults, so trashing a run that
/// produced nothing is a bodyless POST.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct TrashRunRequest {
    #[serde(default)]
    pub artifacts: ArtifactDisposition,
}

/// `POST /v1/runs/{run}/trash`: the header, and the document the caller now
/// has to move into the workspace's bin.
///
/// **The worker records; the window moves.** Trashing a run is a manifest
/// edit and the manifest is the window's, so this call is the half only the
/// worker can do — refusing a run that is still working, and remembering
/// what to do with its results — and it answers with what the other half
/// needs.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TrashRunResponse {
    pub header: RunHeader,
    pub document: DocumentId,
}

/// `POST /v1/runs/{run}/restore`: the run is coming back out of the trash,
/// whole. Nothing was deleted, so there is nothing to undo — the decision
/// about its results is forgotten and the document is the caller's to move
/// back.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RestoreRunResponse {
    pub header: RunHeader,
    pub document: DocumentId,
}

/// `POST /v1/runs/{run}/purge`: **the trash was emptied, and this run is
/// gone.**
///
/// Everything this device holds about it: the ledger's rows, its authority
/// record, and its working folder **when Koru made that folder**. A run
/// scoped to a project ran inside the person's own repository, and that
/// directory was theirs before the run and is theirs after — `folder` says
/// which of those happened, because a trash that quietly did less than it
/// offered teaches somebody that it did more.
///
/// The Run document is not purged here: it is a document of the workspace's
/// manifest, and the caller removes it once this has answered. Ordered that
/// way deliberately — a folder the filesystem refuses is
/// `run_folder_not_removed` with nothing changed, so the run stays in the
/// trash and can be deleted again.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PurgeRunResponse {
    pub run: RunId,
    pub document: DocumentId,
    pub folder: FolderOutcome,
    /// How many result families were retired because the person asked for
    /// them to go with the run. Zero when they asked to keep them, and for
    /// a run that produced nothing.
    pub artifacts_retired: u32,
}

/// `GET /v1/runs/{run}?from=…&len=…&workspace=…`: a window of the history
/// by index, oldest first. `len` is bounded by [`MAX_WINDOW`].
///
/// `workspace` is how a device that did not create the run finds it. A run
/// this device executes is known by its own creation record; a run that
/// arrived by sync has no such record, and the only thing a reader can
/// name it by is the workspace its Run document is registered in (D15: the
/// second device reads, and reads through the same route).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct TimelineQuery {
    #[serde(default)]
    pub from: u64,
    #[serde(default = "default_window")]
    pub len: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceId>,
}

/// The most entries one window carries.
pub const MAX_WINDOW: u32 = 500;

fn default_window() -> u32 {
    100
}

impl TimelineQuery {
    pub fn bounded_len(self) -> usize {
        self.len.clamp(1, MAX_WINDOW) as usize
    }
}

/// `POST /v1/runs/{run}/cancel`: the header after the cancellation was
/// asked for. During a turn the run is still `Running` until the provider
/// answers; the stream carries the end.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CancelRunResponse {
    pub header: RunHeader,
}

/// `POST /v2/runs/{run}/resume`. The body is optional and every field
/// defaults, so a resume of an ordinary interruption is a bodyless POST.
///
/// `confirm` is **a person deciding, never evidence**. §7.6's last row —
/// no evidence from a host that did not restart — proves that nothing is
/// running and nothing can start under the retired key, and leaves earlier
/// work possibly done and unobserved. Such a run is never resumed on its
/// own; a person is told that and asks for it deliberately, which is what
/// this flag carries. It changes nothing about the fence: the key is
/// retired at the host before the next generation opens either way, and a
/// key found live still refuses with `attempt_live`. Ignored for a run
/// whose prior attempt is known to have ended.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct ResumeRunRequest {
    #[serde(default)]
    pub confirm: bool,
}

/// `POST /v2/runs/{run}/resume`: the header after the fresh attempt began.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ResumeRunResponse {
    pub header: RunHeader,
}

/// `POST /v1/runs/{run}/rate`: one gesture on a finished run (D44).
///
/// The note travels here because it lives in the run's own content and
/// nowhere else — never an event attribute, never a metric. The event the
/// worker publishes carries the rating and whether a note exists, and not
/// a byte of the note itself.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RateRunRequest {
    pub rating: Rating,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<RatingNote>,
    pub rated_by: ActorRef,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RateRunResponse {
    pub header: RunHeader,
}

/// `GET /v1/runs/{run}/stream?after=…`, one SSE event each: a history
/// entry with its index, a status change, or the end of the stream.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunStreamItem {
    Entry {
        index: u64,
        item: TimelineItem,
    },
    Status {
        status: RunStatus,
    },
    /// The worker stopped streaming — the run ended, or the worker is
    /// going away. A client resumes with `after` set to the last index.
    Closed,
}

/// The one failure shape, the same fields as the artifact and AI failures.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "DelegateFailure")]
pub struct Failure {
    /// A stable domain code. Run codes include `unknown_run`, `wrong_status`, `not_interrupted`,
    /// `not_delegatable`, `attempt_live`, `prior_attempt_unknown`,
    /// `source_in_trash`, `source_unavailable`, `source_unanswered`,
    /// `context_capture`, `invalid_request`, `forbidden`,
    /// `delegate_unavailable`, and `payload_too_large`. Schedule codes include
    /// `invalid_schedule`, `schedule_conflict`, and `schedule_unavailable`;
    /// transport compatibility may report `delegate_incompatible`.
    pub code: String,
    /// Safe developer-facing detail. Never an objective, a path or a
    /// provider payload.
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, serde_json::Value>,
}

impl Failure {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
            details: BTreeMap::new(),
        }
    }

    pub fn unavailable() -> Self {
        Self::new(
            "delegate_unavailable",
            "The delegate worker is not answering.",
            true,
        )
    }

    pub fn with_detail(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        self.details.insert(key.to_owned(), value.into());
        self
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Failure {}
