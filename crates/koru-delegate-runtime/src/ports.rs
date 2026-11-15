// SPDX-License-Identifier: MPL-2.0

//! The ports: what the orchestrator needs from a host, as traits.
//!
//! Each port is one seam a host fills — the ledger, the Run document, the
//! session host, context capture, the event bus, the preamble, the scope
//! roots, the MCP endpoint, artifact registrations, and the two clocks —
//! and each has a fake in `src/tests/mod.rs`. Two of them are deliberately
//! shaped for a plan running in parallel: [`ContextCapture`] is P1.7's to
//! fill and [`RunEvents`] is P1.8's; the fakes pin every item and record
//! every fact so this crate's tests do not wait for either.
//!
//! **Every port error is a [`PortError`]**: which port, a stable code and
//! a safe detail. The ledger's own `LedgerError` converts into one, so the
//! orchestrator branches on codes and a log line names a port — never a
//! path, an objective or a provider payload.

use std::{
    fmt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use koru_delegate_core::decisions::{DecisionPreparedProjection, DecisionTransitionId};
use koru_delegate_core::{
    AgentTurnId, ArtifactDisposition, AttemptGeneration, DelegationRequest, DocumentId, EventId,
    HostInstanceId, IdempotencyKey, ProjectId, ProviderKind, ProviderSessionKey, PublicationKind,
    ResultReference, RunAttemptId, RunContextItem, RunHeader, RunId, SessionId, SessionProfileKey,
    WorkspaceId,
};
pub use koru_delegate_document::{
    AppliedRunProjection, DecisionUsageLookup, DecisionUsageObservation, RecoveredDecisionUsage,
    RunProjectionIntent,
};
use koru_delegate_document::{
    RunDocument, RunDocumentError, ScheduleClaimAuthority, ScheduleClaimPlan, ScheduleDocument,
    ScheduleDocumentError, ScheduleWrite,
};
use koru_delegate_schedule::{ScheduleContextPointer, ScheduleHeader, ScheduleName};
use koru_delegate_sqlite::{
    AttemptPhase, AttemptRow, ClaimedSlot, DeletionRecord, IntentFacts, IntentRecord, LedgerError,
    OpenPublication, PendingIntent, Projection, SkippedSlot, SlotClaim, SlotOutcome, WriteOutcome,
};
use koru_session_protocol::{
    AgentEventObservation, CancelReason, CreationLookup, DelegationLaunch, ModelName,
    ProviderResumeToken, SequencedAgentEvent, SequencedSessionEvent, SessionFailure,
    SessionSummary, StreamSequence,
};

use crate::RunFact;

// ------------------------------------------------------------------ errors

/// Which seam refused.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Port {
    Store,
    Documents,
    Sessions,
    Capture,
    Events,
    Preamble,
    Scope,
    Mcp,
    Registrations,
    Schedules,
}

impl Port {
    pub fn name(self) -> &'static str {
        match self {
            Self::Store => "store",
            Self::Documents => "documents",
            Self::Sessions => "sessions",
            Self::Capture => "capture",
            Self::Events => "events",
            Self::Preamble => "preamble",
            Self::Scope => "scope",
            Self::Mcp => "mcp",
            Self::Registrations => "registrations",
            Self::Schedules => "schedules",
        }
    }
}

/// A port's refusal: which port, a stable code, and a detail that names
/// tables, codes and counts — never content.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PortError {
    pub port: Port,
    pub code: String,
    pub detail: String,
}

impl PortError {
    pub fn new(port: Port, code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            port,
            code: code.into(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} port: {} ({})",
            self.port.name(),
            self.code,
            self.detail
        )
    }
}

impl std::error::Error for PortError {}

impl From<LedgerError> for PortError {
    fn from(error: LedgerError) -> Self {
        Self::new(Port::Store, error.code(), error.to_string())
    }
}

impl From<RunDocumentError> for PortError {
    fn from(error: RunDocumentError) -> Self {
        let code = match &error {
            RunDocumentError::NotARun => "not_a_run",
            RunDocumentError::ReadOnly { .. } => "run_read_only",
            RunDocumentError::AlreadyInitialized => "already_initialized",
            RunDocumentError::Malformed { .. } => "malformed",
            RunDocumentError::Crdt(_) => "crdt",
        };
        Self::new(Port::Documents, code, error.to_string())
    }
}

impl From<ScheduleDocumentError> for PortError {
    fn from(error: ScheduleDocumentError) -> Self {
        let code = match &error {
            ScheduleDocumentError::NotASchedule => "schedule_not_found",
            ScheduleDocumentError::ReadOnly { .. } => "schedule_read_only",
            ScheduleDocumentError::NameConflict => "schedule_name_conflict",
            ScheduleDocumentError::RevisionConflict { .. } => "schedule_revision_conflict",
            ScheduleDocumentError::NotOwner => "schedule_not_owner",
            ScheduleDocumentError::Retired => "schedule_retired",
            ScheduleDocumentError::HandoffPending => "schedule_handoff_pending",
            ScheduleDocumentError::LifecycleConflict => "schedule_state_conflict",
            _ => "schedule_document",
        };
        Self::new(Port::Schedules, code, error.to_string())
    }
}

pub type PortResult<T> = Result<T, PortError>;

// --------------------------------------------------------------- the ledger

/// The execution ledger — `runs.db` (P1.3) behind it, an in-memory map in a
/// test. The method set is the ledger's own; the port exists so the
/// orchestrator can be driven with no file.
pub trait RunStore: ScheduleLedger {
    fn pending_publication_run_anchor(
        &self,
    ) -> Result<Option<koru_delegate_sqlite::PendingPublicationRunCursor>, LedgerError>;
    fn next_pending_publication_run(
        &self,
        after: Option<koru_delegate_sqlite::PendingPublicationRunCursor>,
        through: koru_delegate_sqlite::PendingPublicationRunCursor,
        before_or_at: Option<koru_delegate_sqlite::PendingPublicationRunCursor>,
    ) -> Result<koru_delegate_sqlite::PendingPublicationRunSeek, LedgerError>;
    fn decision_open_publications_for(
        &self,
        run: RunId,
        limit: usize,
    ) -> Result<Vec<OpenPublication>, LedgerError>;
    /// Native intent/effect evidence on the same sole-writer connection.
    fn native_results(&mut self) -> Option<&mut dyn koru_delegate_sqlite::NativeResultLedger> {
        None
    }
    /// The same ledger connection's Bot domain. Ordinary fakes may omit it;
    /// Bot callers must return unavailable and never fall back to CreateRun.
    fn bots(&mut self) -> Option<&mut dyn koru_delegate_sqlite::BotLedger> {
        None
    }
    /// Validate an existing RunId without creating an intent. Used before
    /// scope preflight so an exact satisfied retry keeps its §7.1 answer.
    fn validate_intent(
        &self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<Option<IntentRecord>, LedgerError>;
    fn record_intent(
        &mut self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<WriteOutcome, LedgerError>;
    fn satisfy_intent(
        &mut self,
        run: RunId,
        run_document: DocumentId,
    ) -> Result<WriteOutcome, LedgerError>;
    fn abandon_intent(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError>;
    fn pending_intents(&self) -> Result<Vec<PendingIntent>, LedgerError>;
    fn intent(&self, run: RunId) -> Result<Option<IntentRecord>, LedgerError>;
    /// Every intent this device recorded, oldest first, payload excluded.
    fn intents(&self) -> Result<Vec<IntentRecord>, LedgerError>;
    /// The settled facts of one creation, objective excluded — **which is
    /// where the working scope is**. Read when the trash is emptied, to ask
    /// whether the folder the run worked in is one Koru made.
    fn intent_facts(&self, run: RunId) -> Result<Option<IntentFacts>, LedgerError>;
    /// What the person said should happen to a trashed run's results.
    fn record_deletion(
        &mut self,
        run: RunId,
        artifacts: ArtifactDisposition,
    ) -> Result<WriteOutcome, LedgerError>;
    /// The run came back out of the trash: the decision goes with it.
    fn clear_deletion(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError>;
    fn deletion(&self, run: RunId) -> Result<Option<DeletionRecord>, LedgerError>;
    /// **The trash was emptied**: every row of the run leaves the ledger.
    fn forget_run(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError>;
    fn open_attempt(&mut self, run: RunId) -> Result<AttemptRow, LedgerError>;
    fn advance(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        phase: AttemptPhase,
    ) -> Result<AttemptRow, LedgerError>;
    fn bind_session(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        session: ProviderSessionKey,
        host: HostInstanceId,
    ) -> Result<AttemptRow, LedgerError>;
    fn retire(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
    ) -> Result<AttemptRow, LedgerError>;
    fn current_attempt(&self, run: RunId) -> Result<Option<AttemptRow>, LedgerError>;
    fn live_attempts(&self) -> Result<Vec<AttemptRow>, LedgerError>;
    fn enqueue(
        &mut self,
        event: EventId,
        run: RunId,
        kind: PublicationKind,
        body_json: &str,
    ) -> Result<WriteOutcome, LedgerError>;
    fn acknowledge(&mut self, event: EventId) -> Result<WriteOutcome, LedgerError>;
    fn bump_attempts(&mut self, event: EventId) -> Result<u32, LedgerError>;
    fn open_publications(&self, limit: usize) -> Result<Vec<OpenPublication>, LedgerError>;
    fn open_publications_for(
        &self,
        run: RunId,
        limit: usize,
    ) -> Result<Vec<OpenPublication>, LedgerError>;
    fn publication(
        &self,
        event: EventId,
    ) -> Result<Option<koru_delegate_sqlite::PublicationRecord>, LedgerError>;
    fn advance_cursor(
        &mut self,
        run: RunId,
        projection: Projection,
        timeline_index: u64,
    ) -> Result<WriteOutcome, LedgerError>;
}

mod publication;
mod routines;
pub use publication::*;
pub use routines::*;

/// The local half of schedule execution. The authority token is minted only
/// after reading the synced document, so the ledger cannot be asked to claim
/// from an unowned or retired schedule through this production seam.
pub trait RoutineLedger: Send {
    fn routine_firing(
        &self,
        slot: &koru_delegate_core::RoutineSlot,
    ) -> Result<Option<koru_delegate_sqlite::SlotClaim>, koru_delegate_sqlite::BotLedgerError>;
    fn routine_route(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Result<
        Option<koru_delegate_core::RoutineRouteIdentity>,
        koru_delegate_sqlite::BotLedgerError,
    >;
    fn held_routine_page(
        &self,
        after: Option<koru_delegate_sqlite::RoutineHeldCursor>,
    ) -> Result<
        koru_delegate_sqlite::RoutinePage<
            koru_delegate_sqlite::RoutineHeldCursor,
            koru_delegate_sqlite::RoutineRecoveryEntry,
        >,
        koru_delegate_sqlite::BotLedgerError,
    >;
    fn queued_routine_page(
        &self,
        after: Option<koru_delegate_sqlite::RoutineQueuedCursor>,
    ) -> Result<
        koru_delegate_sqlite::RoutinePage<
            koru_delegate_sqlite::RoutineQueuedCursor,
            Option<koru_delegate_sqlite::RoutineRecoveryEntry>,
        >,
        koru_delegate_sqlite::BotLedgerError,
    >;
    fn routine_slots_page(
        &self,
        route: &koru_delegate_core::RoutineRouteIdentity,
        after: Option<koru_delegate_sqlite::RoutineSlotCursor>,
    ) -> Result<
        koru_delegate_sqlite::RoutinePage<
            koru_delegate_sqlite::RoutineSlotCursor,
            koru_delegate_sqlite::RoutineOccurrenceFact,
        >,
        koru_delegate_sqlite::BotLedgerError,
    >;
    fn record_expired_routine(
        &mut self,
        authority: koru_delegate_document::RoutineExpiryAuthority,
    ) -> Result<
        (
            koru_delegate_core::RoutineClaimObservation,
            Option<koru_delegate_core::RoutineSkippedClaim>,
        ),
        koru_delegate_sqlite::BotLedgerError,
    >;
    fn claim_bot_routine(
        &mut self,
        authority: ScheduleClaimAuthority,
        admission: &koru_delegate_core::BotAdmissionBarrier,
        fresh: &koru_delegate_core::FreshBotPolicyProof,
    ) -> Result<
        (
            koru_delegate_core::RoutineClaimObservation,
            Option<koru_delegate_core::RoutineSkippedClaim>,
        ),
        koru_delegate_sqlite::BotLedgerError,
    >;
    fn read_routine_claim(
        &self,
        slot: &koru_delegate_core::RoutineSlot,
    ) -> Result<koru_delegate_core::RoutineClaimObservation, koru_delegate_sqlite::BotLedgerError>;
    fn acknowledge_routine_link(
        &mut self,
        acknowledgement: &koru_delegate_core::RoutineLinkAcknowledgement,
    ) -> Result<koru_delegate_core::RoutineClaimRecord, koru_delegate_sqlite::BotLedgerError>;
}

pub trait ScheduleLedger: Send {
    fn claim_run(
        &mut self,
        authority: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<ClaimedSlot, LedgerError>;
    fn claim_run_with_missed(
        &mut self,
        run: ScheduleClaimAuthority,
        missed: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<(ClaimedSlot, SkippedSlot), LedgerError>;
    fn claim_skip(
        &mut self,
        authority: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<SkippedSlot, LedgerError>;
    fn claim_skip_with_missed(
        &mut self,
        due: ScheduleClaimAuthority,
        missed: ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<(SkippedSlot, SkippedSlot), LedgerError>;
    /// Settle a launch that already has its pending intent. The production
    /// implementations accept only `Fired`: pause, retirement, miss and busy
    /// are pre-claim decisions and never cancel a valid claimed launch.
    fn settle(
        &mut self,
        schedule: koru_delegate_core::ScheduleId,
        occurrence_ms: i64,
        outcome: SlotOutcome,
    ) -> Result<WriteOutcome, LedgerError>;
    fn unsettled(&self) -> Result<Vec<SlotClaim>, LedgerError>;
    fn by_run(&self, run: RunId) -> Result<Option<SlotClaim>, LedgerError>;
    /// The authored scope retained by the claimed run's immutable intent.
    fn intent_scope(
        &self,
        run: RunId,
    ) -> Result<Option<koru_delegate_core::WorkspaceScope>, LedgerError>;
    fn observed_through_ms(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Result<Option<i64>, LedgerError>;
    /// Every durable slot for one schedule, including settled skips whose
    /// synced firing append may have been interrupted by a crash.
    fn claims_of(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Result<Vec<SlotClaim>, LedgerError>;
}

/// The identity needed to address one schedule's event chain. This contains
/// no authored content and is safe to retain in the local retry ledger.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScheduleEventRef {
    pub schedule: koru_delegate_core::ScheduleId,
    pub workspace: WorkspaceId,
    pub document: DocumentId,
}

/// A definition transition classified exhaustively from the committed
/// lifecycle. There is no production edit/resume endpoint yet; keeping the
/// active arm typed means adding one cannot silently omit its event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScheduleDefinitionEvent {
    Updated,
    Paused(koru_delegate_schedule::SchedulePauseReason),
}

pub fn schedule_definition_event(
    definition: &koru_delegate_schedule::ScheduleDefinition,
) -> ScheduleDefinitionEvent {
    match definition.lifecycle {
        koru_delegate_schedule::ScheduleLifecycle::Active => ScheduleDefinitionEvent::Updated,
        koru_delegate_schedule::ScheduleLifecycle::Paused { reason } => {
            ScheduleDefinitionEvent::Paused(reason)
        }
    }
}

/// The content-free occurrence fact known before its ledger claim. The
/// supplied time becomes the claim's durable `claimed_at_ms`; retries that
/// discover an older winning claim rebuild from that stored claim instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScheduleOccurrenceEvent {
    pub schedule: ScheduleEventRef,
    pub revision: u32,
    pub occurrence_ms: i64,
    pub run: RunId,
    pub outcome: koru_delegate_schedule::ScheduleFiringOutcome,
    pub busy_holder: Option<RunId>,
    pub claimed_at_ms: i64,
}

/// Opaque identity of a prepared schedule publication. The envelope itself
/// remains inside the host adapter and `runs.db`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PreparedScheduleEvent {
    pub event: koru_delegate_core::EventId,
}

/// Result of one bounded repair/drain pass. Authority-ambiguous rows remain
/// prepared but do not request polling; they wait for a Store/schedule wake.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ScheduleEventMaintenance {
    pub retry_needed: bool,
}

/// Schedule activity is a host boundary: the runtime decides mutation order,
/// while the host constructs validated envelopes, persists exact bytes and
/// talks to the optional event journal.
pub trait ScheduleEvents: Send {
    /// Reconcile prepared rows and drain a bounded ready batch.
    fn maintain(&mut self) -> PortResult<ScheduleEventMaintenance>;

    /// Publish and acknowledge a bounded ready batch without reconciling
    /// prepared authority. Hosts can run this outside their mutation gate.
    fn drain_ready(&mut self, limit: usize) -> PortResult<ScheduleEventMaintenance>;

    /// Reconcile a bounded rotating page without journal I/O. The scheduler
    /// uses a deliberately tiny page before evaluation so restart repair is
    /// ordered ahead of new authority without putting a backlog ahead of a
    /// person's command.
    fn reconcile_prepared(&mut self, limit: usize) -> PortResult<ScheduleEventMaintenance>;

    /// Reconcile only this ScheduleId's prepared Created root before a person
    /// retries creation. This targeted preflight does no ready publication.
    fn reconcile_created(&mut self, schedule: koru_delegate_core::ScheduleId) -> PortResult<()>;

    /// Seed or reconcile the root for a schedule already proven durable.
    fn existing_created(
        &mut self,
        schedule: ScheduleEventRef,
        header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<()>;

    fn prepare_created(
        &mut self,
        schedule: ScheduleEventRef,
        header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<PreparedScheduleEvent>;

    fn prepare_definition(
        &mut self,
        schedule: ScheduleEventRef,
        definition: &koru_delegate_schedule::ScheduleDefinition,
    ) -> PortResult<PreparedScheduleEvent>;

    fn prepare_occurrence(
        &mut self,
        fact: ScheduleOccurrenceEvent,
    ) -> PortResult<Option<PreparedScheduleEvent>>;

    /// A claim retry can return an older durable winner. Preserve the
    /// original preparation when it matches; otherwise abandon only that
    /// prepared intent and prepare the exact stored claim.
    fn reconcile_occurrence(
        &mut self,
        schedule: ScheduleEventRef,
        prepared: Option<PreparedScheduleEvent>,
        claim: &SlotClaim,
    ) -> PortResult<Option<PreparedScheduleEvent>>;

    fn ready(&mut self, prepared: PreparedScheduleEvent) -> PortResult<()>;
    fn abandon(&mut self, prepared: PreparedScheduleEvent) -> PortResult<()>;
}

/// Unit-test/default adapter. Shipping composition replaces this explicitly.
pub struct NoScheduleEvents;

impl ScheduleEvents for NoScheduleEvents {
    fn maintain(&mut self) -> PortResult<ScheduleEventMaintenance> {
        Ok(ScheduleEventMaintenance::default())
    }

    fn drain_ready(&mut self, _limit: usize) -> PortResult<ScheduleEventMaintenance> {
        Ok(ScheduleEventMaintenance::default())
    }

    fn reconcile_prepared(&mut self, _limit: usize) -> PortResult<ScheduleEventMaintenance> {
        Ok(ScheduleEventMaintenance::default())
    }

    fn reconcile_created(&mut self, _schedule: koru_delegate_core::ScheduleId) -> PortResult<()> {
        Ok(())
    }

    fn existing_created(
        &mut self,
        _schedule: ScheduleEventRef,
        _header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<()> {
        Ok(())
    }

    fn prepare_created(
        &mut self,
        _schedule: ScheduleEventRef,
        _header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<PreparedScheduleEvent> {
        Ok(PreparedScheduleEvent {
            event: koru_delegate_core::EventId::mint(),
        })
    }

    fn prepare_definition(
        &mut self,
        _schedule: ScheduleEventRef,
        _definition: &koru_delegate_schedule::ScheduleDefinition,
    ) -> PortResult<PreparedScheduleEvent> {
        Ok(PreparedScheduleEvent {
            event: koru_delegate_core::EventId::mint(),
        })
    }

    fn prepare_occurrence(
        &mut self,
        _fact: ScheduleOccurrenceEvent,
    ) -> PortResult<Option<PreparedScheduleEvent>> {
        Ok(Some(PreparedScheduleEvent {
            event: koru_delegate_core::EventId::mint(),
        }))
    }

    fn reconcile_occurrence(
        &mut self,
        _schedule: ScheduleEventRef,
        prepared: Option<PreparedScheduleEvent>,
        _claim: &SlotClaim,
    ) -> PortResult<Option<PreparedScheduleEvent>> {
        Ok(prepared)
    }

    fn ready(&mut self, _prepared: PreparedScheduleEvent) -> PortResult<()> {
        Ok(())
    }

    fn abandon(&mut self, _prepared: PreparedScheduleEvent) -> PortResult<()> {
        Ok(())
    }
}

// ------------------------------------------------------------ the document

/// One open Run document on this device: read it in place, and commit one
/// fact at a time durably.
///
/// `commit` applies and sends the write. The actual host receives its durable
/// acknowledgement separately; callers use `settle` and `durable` where the
/// answer requires it. Decision projections additionally qualify the exact VV.
pub trait RunDocumentHandle: Send {
    fn document(&self) -> &RunDocument;
    fn commit(
        &mut self,
        write: &mut dyn FnMut(&RunDocument) -> Result<(), RunDocumentError>,
    ) -> PortResult<()>;
    /// Specialized, unexposed decision path. No success default: the owning
    /// adapter must settle the causal base before preparing on private staging.
    fn prepare_decision_projection(
        &mut self,
        intent: &RunProjectionIntent,
    ) -> PortResult<DecisionPreparedProjection>;
    fn apply_prepared_projection(
        &mut self,
        prepared: &DecisionPreparedProjection,
    ) -> PortResult<AppliedRunProjection>;
    fn discard_prepared_projection(&mut self, transition: DecisionTransitionId) -> PortResult<()>;
    /// Take in whatever the host has already been sent for this document
    /// and answer whether anything arrived. **Never waits**: a reader that
    /// blocks here is a reader that stalls the one thread the worker
    /// answers on. A run this device drives never needs it — the
    /// orchestrator is the writer — and a run it is only reading does, so
    /// a second device sees the executing one's progress (D15, UC5).
    fn absorb(&mut self) -> PortResult<bool> {
        Ok(false)
    }
    /// Whether every commit so far is durable on the host. A handle that
    /// answers `false` has sent updates it has not yet seen acknowledged,
    /// and nothing that must follow durability — a published fact (§17.1)
    /// — goes out while it does.
    fn durable(&self) -> bool {
        true
    }
    /// Wait, bounded, until every sent commit is acknowledged durable. The
    /// one place waiting belongs: before a person's request is answered.
    fn settle(&mut self) -> PortResult<()> {
        Ok(())
    }
}

/// The Run documents of a workspace, through the Store.
pub trait RunDocuments {
    /// Freeze a cold-open specification without opening a canonical connection.
    /// A host without this seam refuses; it never opens on the actor as fallback.
    fn publication_open(
        &mut self,
        _workspace: WorkspaceId,
        _document: DocumentId,
    ) -> PortResult<Box<dyn RunDocumentOpen>> {
        Err(publication_pending())
    }
    /// §7.1 step 3: commit the Run document with its header, atomically and
    /// idempotently by run id — a retry after a lost answer finds the one
    /// it made. This is the moment a run exists.
    fn create(&mut self, workspace: WorkspaceId, header: &RunHeader) -> PortResult<DocumentId>;
    /// Open a run this device executes, for writing.
    fn open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn RunDocumentHandle>>;
    /// Every run registered in the workspace, with its header as stored
    /// and whether the workspace's bin holds it.
    ///
    /// **Trashed runs are listed, not filtered out.** A run in the trash is
    /// still a run — the Trash tab draws it, restoring puts it back whole —
    /// and a port that hid it would leave nothing able to answer *what is in
    /// the bin*. Deciding what to do with the two halves is the
    /// orchestrator's.
    fn list(&mut self, workspace: WorkspaceId) -> PortResult<Vec<ListedRun>>;
    /// The document of one run in a workspace, when it is registered
    /// there. How a device that did not create a run finds it: a run that
    /// arrived by sync has no creation record here, so the workspace is
    /// the only thing a reader can name it by (D15).
    fn find(&mut self, workspace: WorkspaceId, run: RunId) -> PortResult<Option<DocumentId>> {
        Ok(self
            .list(workspace)?
            .into_iter()
            .find(|listed| listed.header.run == run)
            .map(|listed| listed.document))
    }
}

/// One run as the workspace's manifest holds it: the document it is, the
/// header inside it, and when it was put in the bin if it was.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ListedRun {
    pub document: DocumentId,
    pub header: RunHeader,
    /// `Some` for a run in the workspace's trash. A trashing that carries
    /// no timestamp — an older client wrote the entry — is still `Some`,
    /// with `None` inside; see [`ListedRun::trashed`].
    pub trashed_at_ms: Option<Option<i64>>,
}

impl ListedRun {
    pub fn trashed(&self) -> bool {
        self.trashed_at_ms.is_some()
    }
}

// ---------------------------------------------------------- the schedules

/// One content-free coalesced wake. The Store-side sets it after an exact
/// manifest or schedule-document room changes; the dedicated scheduler takes
/// it and re-reads schedule authority.
///
/// The notifier is the scheduler's inbox/condition wake, so an idle evaluator can
/// block indefinitely instead of polling this flag. It runs once while a wake
/// remains pending; [`take`](Self::take) rearms it.
#[derive(Clone)]
pub struct ScheduleWake {
    pending: Arc<AtomicBool>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl ScheduleWake {
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            pending: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(notify),
        }
    }

    pub fn signal(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            crate::counters::schedule_woken();
            (self.notify)();
        }
    }

    pub fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }
}

impl fmt::Debug for ScheduleWake {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScheduleWake")
            .field("pending", &self.pending.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

pub trait ScheduleChangeWatch: Send {
    fn close(self: Box<Self>);
}

pub trait ScheduleDocumentHandle: Send {
    fn document(&self) -> &ScheduleDocument;
    /// Expiry-only live authority. The implementation must bind its actual
    /// physical Store DocumentId to the current associated header ScheduleId.
    fn expired_routine_authority(
        &self,
        device: koru_delegate_core::DeviceId,
        plan: koru_delegate_document::RoutineExpiryPlan,
    ) -> PortResult<Option<koru_delegate_document::RoutineExpiryAuthority>>;
    /// Bind a single-use claim to this handle's real Store workspace. The
    /// caller supplies only the occurrence plan; it cannot pair the document
    /// with an arbitrary workspace or later substitute another slot/request.
    /// For BotRoutine, implementations must also bind the physical Store
    /// DocumentId to the candidate ScheduleId. Header identity alone cannot
    /// prove the canonical physical location of a copied Schedule document.
    fn claim_authority(
        &self,
        device: koru_delegate_core::DeviceId,
        plan: ScheduleClaimPlan,
    ) -> PortResult<Option<ScheduleClaimAuthority>>;
    fn commit(
        &mut self,
        write: &mut dyn FnMut(&ScheduleDocument) -> Result<(), ScheduleDocumentError>,
    ) -> PortResult<()>;
    fn absorb(&mut self) -> PortResult<bool> {
        Ok(false)
    }
    /// The first half of schedule deletion. A surface must await this durable
    /// commit before it mutates the manifest's presentation trash state.
    fn retire_before_trash(
        &mut self,
        by: koru_delegate_core::DeviceId,
        at_ms: i64,
    ) -> PortResult<ScheduleWrite> {
        let mut outcome = None;
        self.commit(&mut |document| {
            outcome = Some(document.retire(by, at_ms)?);
            Ok(())
        })?;
        outcome.ok_or_else(|| PortError::new(Port::Schedules, "retire", "retirement was not run"))
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ListedSchedule {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    pub header: ScheduleHeader,
    /// Presentation trash never hides execution authority. A delete flow
    /// first retires durably, then trashes; discovery retains the row so a
    /// crash between those steps can be completed and restore cannot revive it.
    pub trashed_at_ms: Option<i64>,
}

/// The rooted catalog result. Deleted workspaces never appear. Archived
/// workspaces and trashed schedule rows do: those are presentation state,
/// not execution authority.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScheduleCatalog {
    pub workspaces: Vec<WorkspaceId>,
    pub schedules: Vec<ListedSchedule>,
    /// Workspaces whose manifest could not be read. Kept separately so a
    /// public list can be honest about only the workspace it projects.
    pub failed_workspaces: Vec<WorkspaceId>,
    /// Schedule manifest entries whose current document could not be read.
    /// They remain exact watch targets while the scheduler retries; no error
    /// text or document content crosses this boundary.
    pub unavailable_documents: Vec<(WorkspaceId, DocumentId)>,
    /// Content-free count of workspace/document reads or schedule decodes
    /// that failed while producing this partial catalog.
    pub failures: u32,
}

/// Exact Store sync rooms whose content-free updates can change scheduling.
/// Document targets retain their workspace because snapshots are scoped by it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScheduleWatchTargets {
    pub workspaces: Vec<WorkspaceId>,
    pub documents: Vec<(WorkspaceId, DocumentId)>,
}

impl ScheduleWatchTargets {
    pub fn from_catalog(catalog: &ScheduleCatalog) -> Self {
        let mut workspaces = catalog.workspaces.clone();
        workspaces.sort_unstable();
        workspaces.dedup();
        let mut documents = catalog
            .schedules
            .iter()
            .map(|listed| (listed.workspace, listed.document))
            .chain(catalog.unavailable_documents.iter().copied())
            .collect::<Vec<_>>();
        documents.sort_unstable();
        documents.dedup();
        Self {
            workspaces,
            documents,
        }
    }
}

/// Synced schedule documents. Discovery starts at the Store's workspace
/// catalog, so a remotely created schedule does not need a UI to name its
/// workspace first. `watch_changes` joins only the exact Store sync rooms in
/// `targets`; update content and room identity never cross into the run actor.
/// A terminal room/socket problem signals the same content-free wake and the
/// adapter reconnects with bounded backoff after rediscovering the current
/// exact room set. Healthy idle blocks until Store activity or explicit close.
/// A Store-owned catalog subscription supplies the non-polling first-workspace
/// discovery wake even when the initial exact room set is empty.
pub trait ScheduleDocuments: Send {
    fn create(&mut self, workspace: WorkspaceId, header: &ScheduleHeader)
    -> PortResult<DocumentId>;
    fn create_named(
        &mut self,
        workspace: WorkspaceId,
        name: &ScheduleName,
        header: &ScheduleHeader,
    ) -> PortResult<DocumentId> {
        let _ = name;
        self.create(workspace, header)
    }
    fn open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>>;
    fn discover_active(&mut self) -> PortResult<ScheduleCatalog>;
    /// Active readable identities used to resolve canonical replays before a
    /// create. This is not the sole durable identity authority: the Store v2
    /// deterministic-candidate seed checks hidden and deleted workspace
    /// storage atomically before it mutates anything.
    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog>;
    fn watch_changes(
        &mut self,
        targets: &ScheduleWatchTargets,
        wake: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>>;
}

/// Potentially blocking preparation for one scheduled occurrence. Production
/// resolves provider readiness, filesystem scope, and Store context on the
/// dedicated scheduler thread. The run actor receives only the resulting
/// opaque scope identity for short admission arbitration, followed by a
/// durable claim for ordinary background launch.
pub trait SchedulePreparation: Send {
    fn profile_delegatable(&mut self, profile: &SessionProfileKey) -> PortResult<bool>;
    fn resolve_scope(
        &mut self,
        run: RunId,
        scope: &koru_delegate_core::WorkspaceScope,
    ) -> Result<koru_delegate_scope::ResolvedScope, koru_delegate_scope::ScopeRefusal>;
    fn resolve_and_pin(
        &mut self,
        run: RunId,
        pointers: &[ScheduleContextPointer],
        workspace: WorkspaceId,
    ) -> Result<Vec<RunContextItem>, CaptureFailure>;
    /// Release adapter scratch state for a candidate that did not reach the
    /// durable schedule-claim boundary. Production temporary planning creates
    /// no candidate run folder, so its implementation is intentionally a
    /// no-op; test adapters may still use this seam to prove cleanup.
    fn discard_candidate(
        &mut self,
        _run: RunId,
        _scope: &koru_delegate_core::WorkspaceScope,
    ) -> PortResult<()> {
        Ok(())
    }
}

/// The scheduler's injected clock. Classification is checked again after
/// potentially slow preparation so a due occurrence cannot become catch-up
/// work while the scheduler waits on the world.
pub trait ScheduleClock: Send {
    fn now_ms(&mut self, evaluation_now_ms: i64) -> i64;
}

// ------------------------------------------------------------ the sessions

/// What a delegated creation asks the host for.
pub struct SessionLaunch {
    /// Derived by the execution owner from validated immutable Run provenance.
    /// This internal hint selects negotiated composer capabilities, not authority.
    pub is_bot: bool,
    pub key: IdempotencyKey,
    pub profile: SessionProfileKey,
    pub project: Option<ProjectId>,
    pub delegation: DelegationLaunch,
    pub working_directory: PathBuf,
}

/// What a creation answered with: the session, the provider that runs it
/// and the model it was asked for (§17.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedSession {
    pub session: SessionId,
    pub provider: ProviderKind,
    pub requested_model: Option<ModelName>,
}

/// One thing a session said. The host merges the agent-event stream and
/// the session-event stream into this; the orchestrator reads one order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionUpdate {
    Agent(SequencedAgentEvent),
    /// The agent-event stream skipped; what was between is gone (§7.1:
    /// shown as a gap, never invented).
    AgentGap {
        expected: StreamSequence,
        actual: StreamSequence,
    },
    Session(SequencedSessionEvent),
    /// Explicit Session-stream loss, distinct from the Agent stream's fence.
    SessionGap {
        expected: StreamSequence,
        actual: StreamSequence,
    },
    /// The stream ended: the session is gone, or the host is.
    Closed {
        error: Option<SessionFailure>,
    },
}

/// A run's live event stream, pumped by the host on a thread of its own.
pub trait SessionEvents: Send {
    /// The next update, or `None` when `timeout` passed with nothing.
    fn next(&mut self, timeout: Duration) -> Option<SessionUpdate>;
}

/// The session host at protocol v15 (P1.1, P2.3): creation, the non-creating
/// lookup and retirement, the controller, submission, cancellation with a
/// reason, and the streams. The port keeps the controller lease it holds
/// per session; the orchestrator never sees a token.
pub trait Sessions {
    /// Decision-window stream: bounded merge, independent retained frontiers,
    /// and explicit gap observations. An unqualified adapter refuses.
    fn subscribe_decision(
        &mut self,
        _session: SessionId,
        _after: &koru_delegate_core::decisions::DecisionSourceFrontiers,
    ) -> PortResult<Box<dyn SessionEvents>> {
        Err(PortError::new(
            Port::Sessions,
            "decision_stream_unavailable",
            "the session adapter does not support decision source ownership",
        ))
    }
    fn supports_delegation(&self) -> bool;
    /// Whether this exact authored profile is ready for unattended
    /// delegation according to the session host's cached provider verdict.
    fn profile_delegatable(&mut self, profile: &SessionProfileKey) -> PortResult<bool>;
    fn host_instance(&mut self) -> PortResult<HostInstanceId>;
    fn create_delegated(&mut self, launch: SessionLaunch) -> PortResult<CreatedSession>;
    fn lookup(&mut self, key: IdempotencyKey) -> PortResult<(HostInstanceId, CreationLookup)>;
    fn retire(&mut self, key: IdempotencyKey) -> PortResult<(HostInstanceId, CreationLookup)>;
    fn summary(&mut self, session: SessionId) -> PortResult<Option<SessionSummary>>;
    /// Take the controller for `session` on a connection the port keeps.
    fn attach(&mut self, session: SessionId) -> PortResult<()>;
    /// Give the lease up for a handover (§7.4).
    fn release_control(&mut self, session: SessionId) -> PortResult<()>;
    fn submit(&mut self, session: SessionId, text: String) -> PortResult<AgentTurnId>;
    /// Exact typed Bot input. The controller remains inside this port.
    fn bot_submit(
        &mut self,
        _session: SessionId,
        _input: koru_delegate_core::PendingInput,
    ) -> PortResult<AgentTurnId> {
        Err(PortError::new(
            Port::Sessions,
            "bot_control_unsupported",
            "Typed Bot input is unsupported.",
        ))
    }
    /// Protocol25 conditional steering; never starts a new turn.
    fn bot_steer(
        &mut self,
        _session: SessionId,
        _turn: AgentTurnId,
        _input: koru_delegate_core::PendingInput,
    ) -> PortResult<AgentTurnId> {
        Err(PortError::new(
            Port::Sessions,
            "bot_control_unsupported",
            "Conditional steering is unsupported.",
        ))
    }
    fn bot_answer_approval(
        &mut self,
        _session: SessionId,
        _approval: koru_delegate_core::ApprovalId,
        _decision: koru_delegate_core::ApprovalDecision,
        _choice: Option<koru_delegate_core::ApprovalChoiceId>,
    ) -> PortResult<()> {
        Err(PortError::new(
            Port::Sessions,
            "bot_control_unsupported",
            "Native approval answers are unsupported.",
        ))
    }
    fn bot_answer_questions(
        &mut self,
        _session: SessionId,
        _request: koru_delegate_core::QuestionRequestId,
        _answers: Vec<koru_delegate_core::QuestionAnswer>,
    ) -> PortResult<()> {
        Err(PortError::new(
            Port::Sessions,
            "bot_control_unsupported",
            "Native question answers are unsupported.",
        ))
    }
    fn bot_ingest_attachment(
        &mut self,
        _session: SessionId,
        _id: koru_delegate_core::AttachmentId,
        _source: koru_delegate_core::AttachmentSource,
        _data: String,
    ) -> PortResult<koru_delegate_core::Attachment> {
        Err(PortError::new(
            Port::Sessions,
            "bot_control_unsupported",
            "Session attachment staging is unsupported.",
        ))
    }
    fn bot_skip_questions(
        &mut self,
        _session: SessionId,
        _request: koru_delegate_core::QuestionRequestId,
    ) -> PortResult<()> {
        Err(PortError::new(
            Port::Sessions,
            "bot_control_unsupported",
            "Native question skips are unsupported.",
        ))
    }
    fn bot_remove_attachment(
        &mut self,
        _session: SessionId,
        _id: koru_delegate_core::AttachmentId,
    ) -> PortResult<koru_delegate_core::Attachment> {
        Err(PortError::new(
            Port::Sessions,
            "bot_control_unsupported",
            "Session attachment removal is unsupported.",
        ))
    }
    fn bot_attachments(
        &mut self,
        _session: SessionId,
    ) -> PortResult<Vec<koru_delegate_core::Attachment>> {
        Err(PortError::new(
            Port::Sessions,
            "bot_control_unsupported",
            "Session attachment listing is unsupported.",
        ))
    }
    fn cancel_turn(
        &mut self,
        session: SessionId,
        turn: AgentTurnId,
        reason: CancelReason,
    ) -> PortResult<()>;
    fn terminate(&mut self, session: SessionId) -> PortResult<()>;
    /// Automatic completion may terminate only under a current controller lease.
    /// Never fall back to delegated/user-Stop authority or seize a human lease.
    fn terminate_owned(&mut self, _session: SessionId) -> PortResult<()> {
        Err(PortError::new(
            Port::Sessions,
            "bot_control_unsupported",
            "Owned termination is unsupported.",
        ))
    }
    /// The adapter's resume seam (§7.6): the token an adapter that declared
    /// native resume minted for `session`, so a fresh creation can continue
    /// the provider session. `None` is every other adapter — Codex declares
    /// no native resume — and the recorded-context fallback follows.
    fn resume_token(&mut self, session: SessionId) -> PortResult<Option<ProviderResumeToken>>;
    fn observe(&mut self, session: SessionId) -> PortResult<AgentEventObservation>;
    fn subscribe(
        &mut self,
        session: SessionId,
        after: Option<StreamSequence>,
    ) -> PortResult<Box<dyn SessionEvents>>;
}

// ------------------------------------------------------------- the context

/// The pinned identity of one context item (§7.2): a `DocumentVersionId`
/// for a document or block, a `BlobHash` for a file. The shape is the run
/// vocabulary's own — a pin *is* a `RunContextItem`, because the item was
/// already resolved to a version when the person attached it, and capture
/// commits that version and asserts retention.
pub type ContextPin = RunContextItem;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CapturedContext {
    pub pins: Vec<ContextPin>,
}

/// Where a capture failed. A closed vocabulary: the stage names what could
/// not be done, never what the item was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CaptureStage {
    /// The item could not be resolved to a concrete identity.
    Resolve,
    /// The `capture_history` version could not be committed.
    Commit,
    /// The blob's retention could not be asserted.
    Retention,
}

/// A capture failure names the item by index and the stage (§7.1 step 2:
/// *a capture that fails abandons the request with the item named*).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureFailure {
    pub item: usize,
    pub stage: CaptureStage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializedContext {
    /// `<run-workspace>/.koru/context/<RunId>/`.
    pub directory: PathBuf,
    pub items: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MaterializeStage {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaterializeFailure {
    pub item: usize,
    pub stage: MaterializeStage,
}

/// Why a pin may not start a new attempt (D48). A closed vocabulary with a
/// stable code; the item is named by index, never by title.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceRefusalReason {
    /// The source is in the trash; restoring it lifts the refusal.
    InTrash,
    /// The source can no longer be opened here.
    Unavailable,
    /// The store could not answer for the item.
    Unanswered,
}

/// D48's refusal: the first pin that no longer resolves for anything new.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceRefusal {
    pub item: usize,
    pub reason: SourceRefusalReason,
}

impl SourceRefusal {
    /// The stable code a surface reads: `source_in_trash`,
    /// `source_unavailable`, `source_unanswered`.
    pub fn code(&self) -> &'static str {
        match self.reason {
            SourceRefusalReason::InTrash => "source_in_trash",
            SourceRefusalReason::Unavailable => "source_unavailable",
            SourceRefusalReason::Unanswered => "source_unanswered",
        }
    }
}

impl fmt::Display for SourceRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (item {})", self.code(), self.item)
    }
}

/// §7.2, filled by P1.7. The fake pins every item and writes nothing.
pub trait ContextCapture {
    /// D26: resolve a schedule's live pointers at the instant one occurrence
    /// is prepared. Document and block sources receive fresh history-version
    /// pins; blobs are rechecked for retention. The complete returned slice
    /// is bound into the schedule claim before any Run document or provider
    /// exists. A failure returns no usable partial request.
    fn resolve_and_pin(
        &mut self,
        run: RunId,
        pointers: &[ScheduleContextPointer],
        workspace: WorkspaceId,
    ) -> Result<Vec<RunContextItem>, CaptureFailure>;
    /// D48: may these pins start a new attempt? Asked before a resume;
    /// every pin must be live now. Reads only, refuses on the first that
    /// is not.
    fn admit(&mut self, run: RunId, pins: &[ContextPin]) -> Result<(), SourceRefusal>;
    fn capture(
        &mut self,
        run: RunId,
        items: &[RunContextItem],
        workspace: WorkspaceId,
    ) -> Result<CapturedContext, CaptureFailure>;
    fn materialize(
        &mut self,
        run: RunId,
        captured: &CapturedContext,
        run_workspace: &std::path::Path,
    ) -> Result<MaterializedContext, MaterializeFailure>;
    fn cleanup(&mut self, run: RunId, run_workspace: &std::path::Path);
    /// What each pin points at *now* — live, in trash, or no longer
    /// available (D46, D47). Read at the moment of the question and never
    /// stored; the pin itself is unchanged by anything that happens to the
    /// source. An implementation that cannot answer returns an empty
    /// vector, and a surface then says nothing about the source rather
    /// than saying it is gone.
    fn states(&mut self, items: &[RunContextItem]) -> Vec<koru_delegate_core::ContextState> {
        let _ = items;
        Vec::new()
    }
}

// --------------------------------------------------------------- the events

/// The bus refused a publication. `retryable` says whether §7.1's queue
/// should try the same id again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishFailure {
    pub retryable: bool,
}

/// The `delegate.*` events, filled by P1.8. The orchestrator mints the
/// [`EventId`] and commits the fact under it *before* calling this, so a
/// retry after a lost answer carries the same id and the journal dedupes
/// it (§7.1, §17.2).
pub trait RunEvents {
    fn publish(&mut self, event: EventId, fact: &RunFact) -> Result<(), PublishFailure>;
}

// ------------------------------------------------------------- the preamble

/// P1.12's `resolve`, bound to the installation by the host.
pub trait Preamble {
    fn resolve(&mut self) -> koru_delegate_preamble::Resolved;
}

// ------------------------------------------------------------- the endpoint

/// What issuing a run's authority takes (§7.3, ADR-348): everything the
/// record is made of, read from the header the orchestrator already holds
/// and the folder it already resolved. Nothing here is re-derived by the
/// adapter — a second arithmetic for the working folder is a second answer
/// to which paths a registration may name.
pub struct McpLaunch<'a> {
    pub authority: koru_delegate_core::RunToolAuthority,
    pub run: RunId,
    /// The attempt this token is for. A generation that moved is what makes
    /// the previous attempt's token answer `stale_generation`.
    pub generation: AttemptGeneration,
    /// The run's own workspace — where a `create` with no grant still
    /// lands.
    pub workspace: WorkspaceId,
    /// The grant set as the person left it (D28). Re-read per attempt and
    /// never widened.
    pub grants: &'a std::collections::BTreeSet<WorkspaceId>,
    /// The run's working folder, whose `.artifacts/<RunId>/` is the only
    /// place a registration may name a file from.
    pub run_workspace: &'a std::path::Path,
}

/// The stable per-run MCP endpoint, its bearer and its tool allowlist
/// (§7.3), filled by P1.11's run authority.
pub trait McpEndpoints {
    /// Publish immutable discovery metadata before a provider can synchronously
    /// discover its tools. This grants no live effect authority.
    fn publish_authority(
        &mut self,
        _run: RunId,
        _generation: AttemptGeneration,
        _authority: koru_delegate_core::RunToolAuthority,
    ) -> PortResult<()> {
        Ok(())
    }

    /// Issue this attempt's authority — or advance the generation, when the
    /// run already has one — and answer with what the adapter must render:
    /// the endpoint, the bearer shown once, and the tools.
    ///
    /// A refusal fails the start. A provider launched without an authority
    /// would reach an endpoint that answers `401` to every call, and the
    /// run would finish needing attention with nothing registered and no
    /// visible cause — which is exactly the defect this port closes.
    fn launch(&mut self, launch: McpLaunch<'_>) -> PortResult<DelegationLaunch>;

    /// The run reached a terminal status: revoke its authority.
    ///
    /// Separate from `launch` because a run can end without any attempt
    /// having started one, and because the fence and the revocation answer
    /// different questions — advancing the generation retires *this
    /// attempt's* token, revoking closes the endpoint for good. It is
    /// best-effort by design: an authority that outlived its run is refused
    /// by the next call it serves anyway (the record is read every time),
    /// so a gateway that did not answer must not turn a completed run into
    /// a failed one. Never errors; a failure is the adapter's to trace.
    fn revoke(&mut self, run: RunId) {
        let _ = run;
    }

    /// **Where the run worked**, as this device resolved it when the run
    /// started, read back from the durable authority record.
    ///
    /// The record is the only place that folder is written down. The scope
    /// says what *kind* of folder was asked for; re-resolving it now would
    /// name a different directory whenever a project has moved or the
    /// temporary base has been repointed, which is how a delete removes the
    /// wrong thing. `None` for a run that never started, and for one this
    /// device did not run.
    fn run_workspace(&mut self, run: RunId) -> PortResult<Option<std::path::PathBuf>> {
        let _ = run;
        Ok(None)
    }

    /// **The run was deleted: forget its authority.** The record, its
    /// grants and its tokens go — there is nothing left for them to
    /// authorize, and the record holds the person's own path — and the
    /// store keeps one row saying the run was deleted, so a result they
    /// chose to keep can say so instead of naming an id that resolves to
    /// nothing. Idempotent.
    fn forget(&mut self, run: RunId) -> PortResult<()> {
        let _ = run;
        Ok(())
    }
}

// -------------------------------------------------------- the registrations

/// What a turn registered through the artifact tools (D6, P1.11). Asked
/// when a turn completes, so D29's rule — an ended turn that registered
/// nothing needs attention — has its count.
pub trait ArtifactRegistrations {
    /// **What is new since the last ask**, not everything the run has ever
    /// registered: the answer becomes `artifacts_registered` on the turn,
    /// and a turn that registered nothing must count nothing however much
    /// the run produced before it.
    ///
    /// `turn` is `None` when the turn ended while nobody watched and its id
    /// is not known (a reattach, §7.6).
    fn registered(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        turn: Option<AgentTurnId>,
    ) -> Vec<ResultReference>;

    /// **Retire every result family the run created**, because the person
    /// asked for its results to go with it. Answers how many were retired.
    ///
    /// Retired, not erased: a version is immutable and the store marks
    /// rather than deletes, so the column stops offering them and nothing
    /// that already points at one breaks. A run that produced nothing
    /// retires nothing and that is not a failure.
    fn retire_results(&mut self, run: RunId, workspace: WorkspaceId) -> PortResult<u32> {
        let _ = (run, workspace);
        Ok(0)
    }
}

// ------------------------------------------------------------- the clocks

pub trait Clock {
    /// Epoch milliseconds UTC.
    fn now_ms(&mut self) -> i64;
}

/// The one wait the orchestrator performs itself — re-asking a creation
/// reservation still in flight (§7.6) — behind a seam, so library code
/// never calls `std::thread::sleep` and a test counts the wait.
pub trait Sleep {
    fn sleep(&mut self, duration: Duration);
}

// ------------------------------------------------------------- the bundle

/// Everything a host hands the orchestrator.
pub struct Ports {
    pub store: Box<dyn RunStore>,
    pub documents: Box<dyn RunDocuments>,
    pub sessions: Box<dyn Sessions>,
    pub capture: Box<dyn ContextCapture>,
    pub events: Box<dyn RunEvents>,
    pub preamble: Box<dyn Preamble>,
    pub roots: Box<dyn koru_delegate_scope::ScopeRoots>,
    pub mcp: Box<dyn McpEndpoints>,
    pub registrations: Box<dyn ArtifactRegistrations>,
    pub clock: Box<dyn Clock>,
    pub sleep: Box<dyn Sleep>,
}
