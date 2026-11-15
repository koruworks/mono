// SPDX-License-Identifier: MPL-2.0

//! [`Orchestrator`]: the runs open on this device, and the verbs that move
//! them. Each verb lives beside its subject — `create.rs` is §7.1,
//! `start.rs` the admission-to-submission path, `events.rs` the §7.5
//! table, `control.rs` cancellation, `resume.rs` the fresh attempt after
//! an interruption, `usage.rs` §17.2, `restart.rs` §7.6 — and `record.rs`
//! is the one path every fact takes into the document and out to the bus.

mod bot_boot;
mod bot_controls;
mod bot_delivery;
mod bots;
mod control;
mod create;
mod deletion;
mod events;
mod native_results;
mod publication;
mod record;
mod restart;
mod resume;
mod start;
mod state;
mod usage;

use std::{collections::HashMap, fmt, time::Duration};

use koru_delegate_core::{
    ActorRef, AttemptGeneration, ContextState, DocumentId, IllegalTransition, Rating, RatingNote,
    RunAttemptId, RunHeader, RunId, RunStatus, SessionId, TimelineEntry, UsageFact, WorkspaceId,
    WorkspaceScope,
};
use koru_delegate_document::TimelineItem;
use koru_delegate_scope::{ReconcileReport, ScopeClaims, ScopeIdentity, ScopeRefusal};

use crate::{CaptureFailure, PortError, Ports, SessionEvents, SourceRefusal};

pub use create::CreateIntentStatus;
pub use deletion::{Purged, RunPlace};
pub use events::TOOL_STEP_BOUND;
pub use state::RunState;

/// How the orchestrator behaves at its edges. Everything a host may want
/// to turn, and nothing a run's meaning depends on.
#[derive(Clone, Debug)]
pub struct Options {
    /// Routine per-event tracing — the host reads `KORU_DELEGATE_DEBUG`.
    pub debug: bool,
    /// How many times a creation reservation still in flight is re-asked
    /// on restart (§7.6's *wait for it to resolve, then re-ask*), and how
    /// long each wait is, through the [`crate::Sleep`] seam.
    pub reask_attempts: u32,
    pub reask_delay: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            debug: false,
            reask_attempts: 20,
            reask_delay: Duration::from_millis(250),
        }
    }
}

/// Why a run could not be created (§7.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateError {
    /// Step 2 failed: the request was abandoned with the item named, and no
    /// Run document exists.
    Capture(CaptureFailure),
    Port(PortError),
}

impl fmt::Display for CreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capture(failure) => write!(
                f,
                "context item {} could not be captured at the {:?} stage",
                failure.item, failure.stage
            ),
            Self::Port(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CreateError {}

impl From<PortError> for CreateError {
    fn from(error: PortError) -> Self {
        Self::Port(error)
    }
}

impl From<koru_delegate_sqlite::LedgerError> for CreateError {
    fn from(error: koru_delegate_sqlite::LedgerError) -> Self {
        Self::Port(error.into())
    }
}

/// Why a verb on a run was refused. Stable codes; the status names are
/// the vocabulary's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunError {
    /// No run by that id is known to this device.
    UnknownRun,
    /// The verb needs a status the run is not in.
    WrongStatus {
        status: &'static str,
    },
    /// A resume was asked of a run that is not `Interrupted` (P1.9).
    NotInterrupted {
        status: &'static str,
    },
    /// The session host did not negotiate the delegation family.
    NotDelegatable,
    /// The attempt's session is live: reattach rather than resume (P1.9).
    AttemptLive,
    /// §7.6's last row: nothing proved the earlier attempt stopped. A
    /// person resumes it deliberately, with `confirm`, told earlier work
    /// may exist; nothing resumes it on its own.
    PriorAttemptUnknown,
    /// D48: a source the run was given is now in the trash or gone, so it
    /// resolves for nothing new. The item is named by index.
    SourceRefused(SourceRefusal),
    /// **Emptying the trash could not remove the run's working folder**, so
    /// nothing was deleted and the run is still in the trash. The ordinary
    /// cause on Windows is a file still open in another process. Reported
    /// rather than retried or ignored: a run whose folder is gone but whose
    /// record is not is a run nobody can finish deleting.
    ///
    /// The kind is `std`'s closed vocabulary; the path never travels.
    FolderNotRemoved {
        kind: std::io::ErrorKind,
    },
    Illegal(IllegalTransition),
    Port(PortError),
}

impl RunError {
    pub fn publication_pending(&self) -> bool {
        matches!(self, Self::Port(error) if error.port == crate::Port::Documents && error.code == "publication_pending")
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownRun => "unknown_run",
            Self::WrongStatus { .. } => "wrong_status",
            Self::NotInterrupted { .. } => "not_interrupted",
            Self::NotDelegatable => "not_delegatable",
            Self::AttemptLive => "attempt_live",
            Self::PriorAttemptUnknown => "prior_attempt_unknown",
            Self::SourceRefused(refusal) => refusal.code(),
            Self::FolderNotRemoved { .. } => "run_folder_not_removed",
            Self::Illegal(_) => "illegal_transition",
            Self::Port(_) => "port",
        }
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownRun => f.write_str("no such run on this device"),
            Self::WrongStatus { status } => write!(f, "the run is {status}"),
            Self::NotInterrupted { status } => {
                write!(f, "only an interrupted run resumes; the run is {status}")
            }
            Self::NotDelegatable => {
                f.write_str("the session host did not negotiate the delegation family")
            }
            Self::AttemptLive => f.write_str("the attempt's session is still live"),
            Self::PriorAttemptUnknown => f.write_str(
                "nothing proved the earlier attempt stopped; a person confirms the resume",
            ),
            Self::SourceRefused(refusal) => refusal.fmt(f),
            Self::FolderNotRemoved { kind } => {
                write!(f, "the run's working folder could not be removed ({kind})")
            }
            Self::Illegal(illegal) => illegal.fmt(f),
            Self::Port(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RunError {}

impl From<PortError> for RunError {
    fn from(error: PortError) -> Self {
        Self::Port(error)
    }
}

impl From<koru_delegate_sqlite::LedgerError> for RunError {
    fn from(error: koru_delegate_sqlite::LedgerError) -> Self {
        Self::Port(error.into())
    }
}

impl From<koru_delegate_document::RunDocumentError> for RunError {
    fn from(error: koru_delegate_document::RunDocumentError) -> Self {
        Self::Port(error.into())
    }
}

impl From<IllegalTransition> for RunError {
    fn from(error: IllegalTransition) -> Self {
        Self::Illegal(error)
    }
}

/// What `create` answered: the run exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    pub run: RunId,
    pub workspace: WorkspaceId,
    pub document: DocumentId,
}

/// What a start, a resume or a reattach answered. `events` is the stream
/// the host pumps into [`Orchestrator::handle`]; `None` when the attempt
/// ended before a provider was up, and `status` says how.
pub struct Started {
    pub run: RunId,
    pub attempt: RunAttemptId,
    pub generation: AttemptGeneration,
    pub status: RunStatus,
    pub events: Option<Box<dyn SessionEvents>>,
}

impl fmt::Debug for Started {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Started")
            .field("run", &self.run)
            .field("attempt", &self.attempt)
            .field("generation", &self.generation)
            .field("status", &self.status.name())
            .field("events", &self.events.is_some())
            .finish()
    }
}

/// What the host does after one update: keep pumping, or stop — the
/// attempt is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleOutcome {
    Continue,
    Ended,
}

/// What a restart did, in ids.
#[derive(Debug, Default)]
pub struct RestartReport {
    /// Intents completed forward from their payload (§7.1).
    pub completed_forward: Vec<RunId>,
    /// Runs that existed and had never started, started now.
    pub started: Vec<Started>,
    /// Live attempts whose session the host still holds (§7.6 *Known*).
    pub reattached: Vec<Started>,
    /// Attempts a re-ask never resolved: left as running, claim kept.
    pub unresolved: Vec<RunId>,
    /// Attempts reconciled from the host's retained answer (*Known-ended*).
    pub reconciled: Vec<RunId>,
    /// Attempts marked `Interrupted` (never `Failed`).
    pub interrupted: Vec<RunId>,
    /// Durable scheduled runs that remain Created because their exact scope
    /// could not be launched yet. The host reserves these identities before
    /// it accepts another start after restart.
    pub pending_scheduled_scopes: Vec<ScheduledScopeReservation>,
    /// Manual Created runs whose restart launch waited behind one of those
    /// scheduled reservations. The host retries them when the reservation is
    /// released; there is no separate public Start operation.
    pub deferred_manual_starts: Vec<RunId>,
    pub claims: ReconcileReport,
}

/// A durable scheduled claim that must keep precedence while launch retries.
/// The logical scope is always available from the intent/Run document; the
/// physical identity is present when this device could resolve it at restart.
#[derive(Clone, Debug)]
pub struct ScheduledScopeReservation {
    pub run: RunId,
    pub scope: WorkspaceScope,
    pub identity: Option<ScopeIdentity>,
    pub resolution: ScopeReservationResolution,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScopeReservationResolution {
    /// A live same-process claim keeps the exact admitted identity.
    Exact,
    /// Restart retained only logical authority; resolve it again whenever a
    /// later admission asks whether it overlaps.
    Refresh,
}

/// One item of a timeline window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowItem {
    Step(TimelineEntry),
    Usage(UsageFact),
    /// An entry this build cannot decode, in its place.
    Unrecognized,
}

impl From<TimelineItem> for WindowItem {
    fn from(item: TimelineItem) -> Self {
        match item {
            TimelineItem::Step(entry) => Self::Step(entry),
            TimelineItem::Usage(fact) => Self::Usage(fact),
            TimelineItem::Unrecognized => Self::Unrecognized,
        }
    }
}

/// A page of a run's history by index, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineWindow {
    pub total: u64,
    pub from: u64,
    pub items: Vec<WindowItem>,
}

/// A run as an API reads it: where it lives, its header, what this device
/// holds, what its pins point at now, and a window of the history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunView {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    pub header: RunHeader,
    /// The provider session of the attempt this device is driving, when it
    /// is driving one. `None` on every other device and between attempts —
    /// which is how a surface knows it may not offer a handover (D15).
    pub session: Option<SessionId>,
    /// The current state of each of the header's context pins, in order,
    /// or empty when this device could not resolve them (D46).
    pub context: Vec<ContextState>,
    /// The folder the agent is working in, on the device driving the run.
    /// `None` elsewhere and before an attempt has one.
    pub run_workspace: Option<std::path::PathBuf>,
    pub timeline: TimelineWindow,
}

#[derive(Clone)]
struct CachedNativePolicy {
    barrier: koru_delegate_core::BotPolicyBarrier,
    /// Exact owner-computed digest validated when this snapshot was installed.
    digest: [u8; 32],
}

/// The runs this device executes, and the ports they are driven through.
pub struct Orchestrator {
    pub(crate) ports: Ports,
    pub(crate) options: Options,
    pub(crate) claims: ScopeClaims,
    /// A retained live Run had neither a readable scope nor a prior exact claim.
    /// Only a later complete reconciliation may clear this fresh-work fence.
    scope_recovery_incomplete: bool,
    pub(crate) runs: HashMap<RunId, RunState>,
    // Bounded transient current-policy proofs; never reconstructed from ledger hashes.
    native_policies: Vec<CachedNativePolicy>,
    native_routine_policies: Vec<koru_delegate_core::RoutinePolicyBarrier>,
    bot_boot: bot_boot::BootCoordination,
    publication_ticket: Option<crate::RunPublicationTicket>,
    publication_retry: record::PublicationRetry,
}

impl Orchestrator {
    pub fn new(ports: Ports, options: Options) -> Self {
        Self {
            ports,
            options,
            claims: ScopeClaims::new(),
            runs: HashMap::new(),
            native_policies: Vec::new(),
            native_routine_policies: Vec::new(),
            bot_boot: bot_boot::BootCoordination::new(),
            publication_ticket: None,
            scope_recovery_incomplete: false,
            publication_retry: record::PublicationRetry::default(),
        }
    }

    /// The ports, for a host that composes something beside the
    /// orchestrator over the same objects.
    pub fn ports(&self) -> &Ports {
        &self.ports
    }

    pub fn ports_mut(&mut self) -> &mut Ports {
        &mut self.ports
    }

    /// Give the ports back — a host that restarts the orchestrator over
    /// the same objects, and a test that does the same.
    pub fn into_ports(self) -> Ports {
        self.ports
    }

    /// The scope claims held right now.
    pub fn claims(&self) -> &ScopeClaims {
        &self.claims
    }

    /// Resolve a scheduled candidate and name the first overlapping holder.
    /// This is the pre-claim half of D34; a durable launch claim is created
    /// only after this answer is `None`.
    pub fn scheduled_scope_holder(
        &self,
        run: RunId,
        scope: &koru_delegate_core::WorkspaceScope,
    ) -> Result<Option<RunId>, ScopeRefusal> {
        let identity = ScopeIdentity::resolve(scope, run, self.ports.roots.as_ref())?;
        Ok(self
            .claims
            .holders_of(&identity)
            .first()
            .map(|holder| holder.run))
    }

    pub fn resolve_scope_identity(
        &self,
        run: RunId,
        scope: &koru_delegate_core::WorkspaceScope,
    ) -> Result<ScopeIdentity, ScopeRefusal> {
        ScopeIdentity::resolve(scope, run, self.ports.roots.as_ref())
    }

    pub fn plan_scope_identity(
        &self,
        run: RunId,
        scope: &koru_delegate_core::WorkspaceScope,
    ) -> Result<ScopeIdentity, ScopeRefusal> {
        self.plan_scope(run, scope)
            .map(|resolved| resolved.identity)
    }

    pub fn plan_scope(
        &self,
        run: RunId,
        scope: &koru_delegate_core::WorkspaceScope,
    ) -> Result<koru_delegate_scope::ResolvedScope, ScopeRefusal> {
        if self.scope_recovery_incomplete {
            return Err(ScopeRefusal::Busy);
        }
        if matches!(scope, koru_delegate_core::WorkspaceScope::Temporary) {
            return self.ports.roots.planned_temporary(run);
        }
        koru_delegate_scope::ResolvedScope::resolve(scope, run, self.ports.roots.as_ref())
    }

    pub(crate) fn ensure_scope_recovery(&self) -> Result<(), RunError> {
        if self.scope_recovery_incomplete {
            return Err(crate::PortError::new(
                crate::Port::Scope,
                "scope_recovery_pending",
                "a retained working scope has not been recovered",
            )
            .into());
        }
        Ok(())
    }

    /// The runs open on this device — every run with a live attempt, plus
    /// the ones opened for reading since.
    pub fn open_runs(&self) -> impl Iterator<Item = RunId> + '_ {
        self.runs.keys().copied()
    }

    /// The header as the Run document holds it.
    pub fn header(&mut self, run: RunId) -> Result<RunHeader, RunError> {
        let state = self.open(run)?;
        Ok(state.doc.document()?.header()?)
    }

    /// The header and a window of the history, oldest first from `from`.
    ///
    /// `workspace` is the fallback for a run this device did not create —
    /// one that arrived by sync — which has no creation record here to be
    /// found by. A device reading such a run takes no verb on it (D15),
    /// and `session` says so by answering `None`.
    pub fn view(
        &mut self,
        run: RunId,
        workspace: Option<WorkspaceId>,
        from: u64,
        len: usize,
    ) -> Result<RunView, RunError> {
        let state = self.open_in(run, workspace)?;
        let session = state.attempt.as_ref().and_then(|attempt| attempt.session);
        // **Where the agent is actually working.** The scope says what kind
        // of folder was chosen; only the device driving the run knows which
        // one that turned out to be, and a person asked to trust a run they
        // did not watch is entitled to be told. Absent on every other device.
        let live = state
            .attempt
            .as_ref()
            .map(|attempt| attempt.run_workspace.clone())
            .filter(|path| !path.as_os_str().is_empty());
        let document = state.doc.document()?;
        let header = document.header()?;
        let all = document.timeline_all();
        let total = all.len() as u64;
        let start = usize::try_from(from.min(total)).unwrap_or(usize::MAX);
        let items = all
            .into_iter()
            .skip(start)
            .take(len)
            .map(WindowItem::from)
            .collect();
        let (workspace, document_id) = (state.workspace, state.document);
        // **A finished run still has a folder, and this is where it comes
        // from.** `end_attempt` takes the attempt the moment a run ends, and
        // a restart rebuilds the state with no attempt at all — so a run a
        // person comes back to read, which is every run they judge, had no
        // folder to show at all. The durable authority record does, and it is
        // the same one a delete reads (§the folder rule, ADR-374): the path
        // *as this device resolved it when the run started*, never
        // re-derived, because a project that has moved or a temporary base
        // that has been repointed would name a different directory.
        //
        // Asked only when the live attempt has none, so a run in flight costs
        // nothing, and a refusal is not one: a folder this device cannot say
        // is `None`, which the surface renders as *not known here* rather
        // than failing a read of the run.
        let run_workspace = match live {
            Some(path) => Some(path),
            None => self.ports.mcp.run_workspace(run).unwrap_or_default(),
        };
        // Read at the moment of the question and never stored: the pin is
        // what the run received, this is what it points at now (D46, D47).
        let context = self.ports.capture.states(&header.context);
        Ok(RunView {
            workspace,
            document: document_id,
            header,
            session,
            run_workspace,
            context,
            timeline: TimelineWindow {
                total,
                from: from.min(total),
                items,
            },
        })
    }

    /// Record the person's one gesture on a finished run (D44).
    ///
    /// Refused while the run is still working: a rating is about how it
    /// went. The note is committed to the run's own content and travels no
    /// further — the fact this publishes carries the rating and nothing of
    /// the note.
    pub fn rate(
        &mut self,
        run: RunId,
        rating: Rating,
        note: Option<RatingNote>,
        rated_by: ActorRef,
    ) -> Result<RunHeader, RunError> {
        let status = self.status(run)?;
        if !rateable(&status) {
            return Err(RunError::WrongStatus {
                status: status.name(),
            });
        }
        let rated_at_ms = self.ports.clock.now_ms();
        let record = koru_delegate_core::RunRating {
            rating,
            note,
            rated_by,
            rated_at_ms,
        };
        let state = self.open(run)?;
        state
            .doc
            .commit(&mut |document| document.set_rating(&record))?;
        self.step(
            run,
            koru_delegate_core::StepKind::Rated { rating },
            koru_delegate_core::TimelineRefs::new([koru_delegate_core::TimelineRef::Actor {
                actor: rated_by,
            }])
            .expect("one ref"),
            // D44: the fact says a rating happened, who made it and that a
            // note exists — never the note, which stays in the run's content.
            Some(crate::RunFact::Rated {
                run,
                rating,
                rated_by,
                has_note: record.note.is_some(),
            }),
        )?;
        self.header(run)
    }

    /// Take in whatever the Store has sent for a run this device is only
    /// reading, and say whether anything arrived. A run this device drives
    /// is written by the orchestrator itself and needs none of this.
    pub fn absorb(&mut self, run: RunId) -> Result<bool, RunError> {
        let state = self.open(run)?;
        if state.attempt.is_some() {
            return Ok(false);
        }
        Ok(state.doc.absorb()?)
    }

    /// Every run this device created in `workspace`, by its intent — the
    /// request that became a run — with the header as stored.
    pub fn list(&mut self, workspace: WorkspaceId) -> Result<Vec<crate::ListedRun>, RunError> {
        Ok(self.ports.documents.list(workspace)?)
    }

    /// The provider session this device is driving `run` in, when it is
    /// driving one right now.
    ///
    /// The same value [`Self::view`] answers with, asked without opening a
    /// document: it is read off the live attempt this orchestrator already
    /// holds in memory, so a caller may ask it about every run of a listing
    /// without paying a read per run. `None` for a run between attempts, a
    /// run that has ended, and every run another device is executing.
    pub fn driving(&self, run: RunId) -> Option<SessionId> {
        self.runs
            .get(&run)?
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.session)
    }

    /// Whether this device holds a live attempt for `run`.
    pub fn is_active(&self, run: RunId) -> bool {
        self.runs
            .get(&run)
            .is_some_and(|state| state.attempt.is_some())
    }

    /// The run's state, opening the document through the intent that names
    /// it if this device created the run but is not holding it open.
    pub(crate) fn open(&mut self, run: RunId) -> Result<&mut RunState, RunError> {
        self.open_in(run, None)
    }

    /// The same, with the workspace a reader may name when this device has
    /// no creation record for the run — the second device's case (D15).
    pub(crate) fn open_in(
        &mut self,
        run: RunId,
        workspace: Option<WorkspaceId>,
    ) -> Result<&mut RunState, RunError> {
        self.restore_publication_owner(run)?;
        if !self.runs.contains_key(&run) {
            let placed = self
                .ports
                .store
                .intent(run)?
                .and_then(|record| record.run_document.map(|id| (record.workspace, id)));
            let (workspace, document) = match (placed, workspace) {
                (Some(placed), _) => placed,
                (None, Some(workspace)) => (
                    workspace,
                    self.ports
                        .documents
                        .find(workspace, run)?
                        .ok_or(RunError::UnknownRun)?,
                ),
                (None, None) => return Err(RunError::UnknownRun),
            };
            let doc = self.ports.documents.open(workspace, document)?;
            let requested_model = doc.document().header()?.requested_model;
            let mut state = RunState::new(workspace, document, doc);
            state.requested_model = requested_model.map(|model| model.as_str().to_owned());
            self.runs.insert(run, state);
        }
        Ok(self.runs.get_mut(&run).expect("inserted above"))
    }

    /// Wait, bounded, for the run's commits to be durable. The one place
    /// waiting belongs: before a person's request about the run is answered.
    pub fn settle(&mut self, run: RunId) -> Result<(), RunError> {
        if let Some(state) = self.runs.get_mut(&run) {
            state.doc.settle()?;
        }
        Ok(())
    }

    /// Whether any open run has a commit the Store has not yet called
    /// durable — the actor's reason to keep polling for acknowledgements.
    pub fn has_unsettled(&self) -> bool {
        // In-flight publication completion wakes the actor itself. It must not
        // create a recurring Store polling timer while the handle is away.
        self.runs
            .values()
            .any(|state| state.doc.attached_unsettled())
    }

    /// Take in the acknowledgements that have already arrived, without
    /// waiting, and answer whether every open run is durable now. A handle
    /// that cannot be read is left for its next commit to report.
    pub fn absorb_acks(&mut self) -> bool {
        for state in self.runs.values_mut() {
            if state.doc.attached_unsettled() {
                let _ = state.doc.absorb();
            }
        }
        !self.has_unsettled()
    }

    /// Every open run, settled: the last thing before the worker stops, so
    /// a commit in flight is not a commit the next process has to guess at.
    pub fn settle_all(&mut self) {
        for (run, state) in &mut self.runs {
            if let Err(error) = state.doc.settle() {
                tracing::warn!(target: "koru::delegate", run = ?run, %error, "a run's last commit did not settle before stopping");
            }
        }
    }

    pub(crate) fn trace(&self, run: RunId, what: &'static str, detail: fmt::Arguments<'_>) {
        if self.options.debug {
            tracing::debug!(target: "koru::delegate", run = ?run, what, "{detail}");
        }
    }
}

/// D44's *finished*: a run that ended, or one whose orchestrator turn
/// finished having registered nothing (D29) — over, as far as the person
/// is concerned, and exactly the case a rating is most worth having. A run
/// still working is not rated: there is nothing yet to rate.
fn rateable(status: &RunStatus) -> bool {
    status.is_terminal()
        || status.attention().is_some_and(|attention| {
            attention.contains(koru_delegate_core::Attention::FinishedNeedingAttention)
        })
}
