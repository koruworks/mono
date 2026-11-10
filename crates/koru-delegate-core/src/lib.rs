// SPDX-License-Identifier: MPL-2.0

//! The shared vocabulary of delegated runs and schedule outcomes.
//!
//! Koru can run a coding agent in a tab; delegation is handing it work and
//! coming back to a result you can read, trust and use again. This crate is
//! the *nouns* of that feature and the one *verb* that moves a run between
//! states. It starts nothing, stores nothing, transports nothing and draws
//! nothing — the orchestrator, `runs.db`, the Run document, the `delegate.*`
//! events and every surface depend on it, and it depends on none of them.
//!
//! Three rules shape everything here:
//!
//! - **The sync/execution split is a type boundary.** What a run *means* on
//!   every device is a [`RunHeader`] (D14); how far *this machine* got is an
//!   [`AttemptState`] (D3). They share a [`RunId`] and nothing else, and no
//!   `From` exists between them, so a field cannot land on the wrong side.
//! - **The status moves through one closed function.** [`transition`] is the
//!   only way from one [`RunStatus`] to another; every (state, event) pair it
//!   refuses is asserted illegal by a test. `AwaitingReview` and
//!   `WaitingForParticipant` exist so a later milestone unlocks a transition
//!   rather than rewriting a terminal state, and nothing reaches them yet.
//! - **Privacy is structural.** A [`TimelineEntry`] cannot carry text at all
//!   ([`ContentFree`] is sealed and `String` never implements it); the text a
//!   person writes — the objective, a rating note, an artifact title, a folder
//!   — lives in newtypes whose `Debug` redacts, so no derived `Debug` above
//!   them can leak it.
//!
//! Schedule templates and recurrence remain in `koru-delegate-schedule`; only
//! the content-free pause, context-stage and firing-outcome enums live here so
//! Schedule documents and `delegate.schedule.*` events cannot drift apart.

mod actor;
mod attempt;
#[doc(hidden)]
pub mod bot_wire;
mod bots;
mod context;
mod deletion;
mod economics;
mod failure;
mod header;
mod ids;
mod name;
mod pin;
mod request;
mod schedule;
mod scope;
mod status;
mod text;
mod timeline;
mod tool_failures;
mod turn;

pub use actor::ActorRef;
pub use attempt::{
    AttemptState, HostInstanceId, PendingPublication, ProcessFacts, ProviderSessionKey,
    PublicationKind, StaleAttempt,
};
pub use bots::{
    BotAccess, BotAdmissionBarrier, BotAdmissionState, BotAdmissionView, BotDeliveryReceipt,
    BotExecutionFence, BotOwner, BotPolicyBarrier, BotPolicyBarrierKey, BotPolicyBarrierReceipt,
    BotPolicyBarrierState, BotProvenance, BotRunKey, BotSubmission, BotSubmissionKey,
    BotWorkingFolder, CapturedBotPolicy, ConfirmNativeResult, NativeResultDisposition,
    NativeResultIntentView, NativeResultKey, NativeResultKind, NativeResultObservation,
    NativeResultOperation, NativeResultReference, NativeResultRefusal, NativeResultStage,
    PrepareNativeResult, RunToolAuthority,
};
pub use context::{BlobKind, RunContextItem};
pub use deletion::{ArtifactDisposition, FolderOutcome};
pub use economics::{
    AmendmentRevision, Completeness, EconomicsSummary, IncompleteReason, InvalidModelName,
    ModelIdentity, ModelName, ModelReport, TokenTotal, TurnKey, UsageFact, elapsed_ms,
    interventions, replay,
};
pub use failure::RunFailureReason;
pub use header::{DelegatedRun, Rating, ResultReference, RunHeader, RunRating};
pub use ids::{
    ArtifactVersionId, AttemptGeneration, BlockNodeId, BotId, BotRequestId, HandoffId,
    IdParseError, ModelRef, RunAttemptId, RunId, ScheduleId, TimelineEntryId, UsageFactId,
};
pub use name::RunName;
pub use pin::ContextState;
pub use request::{DelegationRequest, RunOrigin};
pub use schedule::{ScheduleContextFailureStage, ScheduleFiringOutcome, SchedulePauseReason};
pub use scope::{ExecutionEnvironment, ScopeOverlap, WorkspaceScope};
pub use status::{
    Attention, AttentionSet, CancelReason, ControlState, FailureStage, IllegalTransition,
    PriorAttempt, RunEvent, RunStatus, transition,
};
pub use text::{
    ArtifactTitle, MAX_ARTIFACT_TITLE_CHARS, MAX_OBJECTIVE_CHARS, MAX_RATING_NOTE_CHARS,
    MAX_RELATIVE_FOLDER_CHARS, Objective, RatingNote, RelativeFolder, TextError,
};
pub use timeline::{
    ApprovalOutcome, ContentFree, MAX_TIMELINE_REFS, ResumeSeam, StepKind, TimelineEntry,
    TimelineRef, TimelineRefs, TooManyRefs,
};
pub use tool_failures::{
    MAX_TOOL_FAILURE_KINDS, StoredToolFailures, ToolFailure, ToolFailureTally, ToolFailures,
    ToolKind,
};
pub use turn::{TurnIdentity, TurnOriginator};

// The vocabulary this crate speaks but does not own, re-exported so a
// dependant names one crate for a run and its parts.
pub use koru_device::DeviceId;
pub use koru_events::EventId;
pub use koru_session_protocol::{
    AgentEventObservation, AgentTurnId, ApprovalChoiceId, ApprovalDecision, ApprovalId, Attachment,
    AttachmentId, AttachmentSource, IdempotencyKey, PendingInput, ProjectId, ProviderKind,
    QuestionAnswer, QuestionRequestId, SequencedAgentEvent, SessionId, SessionProfileKey,
    StreamSequence,
};
pub use koru_store_core::ids::DocumentVersionId;
pub use koru_store_core::{BlobHash, DocumentId, ProfileId, WorkspaceId};

#[cfg(test)]
mod tests;
