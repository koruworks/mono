// SPDX-License-Identifier: MPL-2.0

//! Every scenario drives the orchestrator over fakes: an in-memory ledger
//! (the real `RunsDb` on `:memory:`, wrapped to record the order of its
//! writes and to fail one on demand), in-memory Run documents, a scripted
//! session host, a capture that pins everything and writes nothing, a bus
//! that records every fact, a counted sleep and a stepped clock. The one
//! thing a scenario touches on disk is the working folder admission has
//! to canonicalize — a directory named for this process, under the
//! system temp dir, removed when the scenario ends. Nothing reaches a
//! store, a process or a socket.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

use koru_delegate_core::{
    ActorRef, AgentTurnId, AttemptGeneration, DelegationRequest, DeviceId, DocumentId, EventId,
    HostInstanceId, IdempotencyKey, Objective, ProfileId, ProjectId, ProviderSessionKey,
    PublicationKind, ResultReference, RunAttemptId, RunContextItem, RunHeader, RunId, SessionId,
    SessionProfileKey, ToolFailure, ToolFailures, ToolKind, WorkspaceId, WorkspaceScope,
};
use koru_delegate_document::{RunDocument, RunDocumentError};
use koru_delegate_scope::{RootLookup, ScopeRoots};
use koru_delegate_sqlite::{
    AttemptPhase, AttemptRow, IntentRecord, LedgerError, OpenPublication, PendingIntent,
    Projection, RunsDb, WriteOutcome,
};
use koru_session_protocol::{
    AgentEvent, AgentEventObservation, CancelReason, ControllerState, CreationLookup,
    DelegationLaunch, ManagedState, McpToolAllowlist, ProcessState, ProviderResumeToken,
    SequencedAgentEvent, SequencedSessionEvent, SessionEvent, SessionMode, SessionSummary,
    StreamSequence,
};
use uuid::Uuid;

use crate::*;

mod authority;
mod creation;
mod deletion;
mod dependency;
mod diagnostics;
mod events;
mod handover;
mod interruption;
mod privacy;
mod rating;
mod reading;
mod restart;
mod resume;
mod schedules;
mod usage;

/// A breakdown built the way the fold builds one: one `record` per failed
/// call, so a fixture can never assert a shape the runtime cannot produce.
pub(crate) fn breakdown(pairs: &[(ToolKind, ToolFailure, u32)]) -> ToolFailures {
    let mut failures = ToolFailures::default();
    for (tool, failure, calls) in pairs {
        for _ in 0..*calls {
            failures.record(*tool, *failure);
        }
    }
    failures
}

// ------------------------------------------------------------------- ids

pub(crate) fn v4(n: u8) -> Uuid {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}

pub(crate) fn run_id(n: u8) -> RunId {
    RunId::from_uuid(v4(n))
}

pub(crate) fn workspace() -> WorkspaceId {
    WorkspaceId::from_uuid(v4(0xa0))
}

pub(crate) fn project(n: u8) -> ProjectId {
    v4(n).hyphenated().to_string().parse().unwrap()
}

pub(crate) fn turn(n: u8) -> AgentTurnId {
    v4(0xb0 + n).hyphenated().to_string().parse().unwrap()
}

pub(crate) fn session(n: u8) -> SessionId {
    v4(0xc0 + n).hyphenated().to_string().parse().unwrap()
}

pub(crate) fn host(n: u8) -> HostInstanceId {
    HostInstanceId::from_uuid(v4(0xd0 + n))
}

/// Text no real objective carries, so a leak is unmistakable.
pub(crate) const OBJECTIVE_SENTINEL: &str = "OBJECTIVE-SENTINEL-write the quarterly summary";
pub(crate) const PREAMBLE_SENTINEL: &str = "PREAMBLE-SENTINEL you are a delegated agent";

pub(crate) fn request(run: RunId, scope: WorkspaceScope) -> DelegationRequest {
    DelegationRequest {
        run,
        requested_by: ActorRef::Person {
            profile: ProfileId::from_uuid(v4(0xe0)),
        },
        objective: Objective::new(OBJECTIVE_SENTINEL).unwrap(),
        agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
        requested_model: None,
        scope,
        context: vec![RunContextItem::Document {
            workspace: workspace(),
            document: DocumentId::from_uuid(v4(0xf0)),
            version: koru_delegate_core::DocumentVersionId::from_uuid(v4(0xf1)),
        }],
        grants: BTreeSet::from([workspace()]),
        executing_device: DeviceId::from_uuid(v4(0xe1)),
        origin: Default::default(),
    }
}

// --------------------------------------------------------------- the disk

/// A directory of this process's own, removed when the fixture drops —
/// the working folder admission canonicalizes.
pub(crate) struct TempDir {
    pub root: PathBuf,
    cleanup: bool,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "koru-delegate-runtime-{}-{}-{}",
            std::process::id(),
            label,
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self {
            root,
            cleanup: true,
        }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

// -------------------------------------------------------------- the ledger

/// Ledger writes and failures the scenario controls.
#[derive(Default)]
pub(crate) struct LedgerScript {
    pub ops: Vec<&'static str>,
    /// The next call named here fails with `Storage`.
    pub fail: Option<&'static str>,
}

#[derive(Clone)]
pub(crate) struct RecordingStore {
    inner: Arc<Mutex<RunsDb>>,
    script: Arc<Mutex<LedgerScript>>,
}

impl RecordingStore {
    fn check(&self, op: &'static str) -> Result<(), LedgerError> {
        let mut script = self.script.lock().unwrap();
        script.ops.push(op);
        if script.fail == Some(op) {
            script.fail = None;
            return Err(LedgerError::Storage {
                operation: op,
                detail: "injected".into(),
            });
        }
        Ok(())
    }
}

impl RunStore for RecordingStore {
    fn validate_intent(
        &self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<Option<IntentRecord>, LedgerError> {
        self.inner
            .lock()
            .unwrap()
            .validate_intent(request, workspace)
    }

    fn record_intent(
        &mut self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<WriteOutcome, LedgerError> {
        self.check("record_intent")?;
        self.inner.lock().unwrap().record_intent(request, workspace)
    }
    fn satisfy_intent(
        &mut self,
        run: RunId,
        run_document: DocumentId,
    ) -> Result<WriteOutcome, LedgerError> {
        self.check("satisfy_intent")?;
        self.inner.lock().unwrap().satisfy_intent(run, run_document)
    }
    fn abandon_intent(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        self.check("abandon_intent")?;
        self.inner.lock().unwrap().abandon_intent(run)
    }
    fn intent_facts(
        &self,
        run: RunId,
    ) -> Result<Option<koru_delegate_sqlite::IntentFacts>, LedgerError> {
        self.inner.lock().unwrap().intent_facts(run)
    }
    fn record_deletion(
        &mut self,
        run: RunId,
        artifacts: koru_delegate_core::ArtifactDisposition,
    ) -> Result<WriteOutcome, LedgerError> {
        self.check("record_deletion")?;
        self.inner.lock().unwrap().record_deletion(run, artifacts)
    }
    fn clear_deletion(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        self.check("clear_deletion")?;
        self.inner.lock().unwrap().clear_deletion(run)
    }
    fn deletion(
        &self,
        run: RunId,
    ) -> Result<Option<koru_delegate_sqlite::DeletionRecord>, LedgerError> {
        self.inner.lock().unwrap().deletion(run)
    }
    fn forget_run(&mut self, run: RunId) -> Result<WriteOutcome, LedgerError> {
        self.check("forget_run")?;
        self.inner.lock().unwrap().forget_run(run)
    }
    fn pending_intents(&self) -> Result<Vec<PendingIntent>, LedgerError> {
        self.inner.lock().unwrap().pending_intents()
    }
    fn intent(&self, run: RunId) -> Result<Option<IntentRecord>, LedgerError> {
        self.inner.lock().unwrap().intent(run)
    }
    fn intents(&self) -> Result<Vec<IntentRecord>, LedgerError> {
        self.inner.lock().unwrap().intents()
    }
    fn open_attempt(&mut self, run: RunId) -> Result<AttemptRow, LedgerError> {
        self.check("open_attempt")?;
        self.inner.lock().unwrap().open_attempt(run)
    }
    fn advance(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        phase: AttemptPhase,
    ) -> Result<AttemptRow, LedgerError> {
        self.check(match phase {
            AttemptPhase::Starting => "advance:starting",
            AttemptPhase::Running => "advance:running",
            AttemptPhase::Ended => "advance:ended",
            AttemptPhase::Interrupted => "advance:interrupted",
            AttemptPhase::Unknown => "advance:unknown",
            AttemptPhase::Created => "advance:created",
        })?;
        self.inner.lock().unwrap().advance(run, generation, phase)
    }
    fn bind_session(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        session: ProviderSessionKey,
        host: HostInstanceId,
    ) -> Result<AttemptRow, LedgerError> {
        self.check(if session.session.is_some() {
            "bind_session:id"
        } else {
            "bind_session:key"
        })?;
        self.inner
            .lock()
            .unwrap()
            .bind_session(run, generation, session, host)
    }
    fn retire(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
    ) -> Result<AttemptRow, LedgerError> {
        self.check("retire")?;
        self.inner.lock().unwrap().retire(run, generation)
    }
    fn current_attempt(&self, run: RunId) -> Result<Option<AttemptRow>, LedgerError> {
        self.inner.lock().unwrap().current_attempt(run)
    }
    fn live_attempts(&self) -> Result<Vec<AttemptRow>, LedgerError> {
        self.inner.lock().unwrap().live_attempts()
    }
    fn enqueue(
        &mut self,
        event: EventId,
        run: RunId,
        kind: PublicationKind,
        body_json: &str,
    ) -> Result<WriteOutcome, LedgerError> {
        self.check("enqueue")?;
        self.inner
            .lock()
            .unwrap()
            .enqueue(event, run, kind, body_json)
    }
    fn acknowledge(&mut self, event: EventId) -> Result<WriteOutcome, LedgerError> {
        self.check("acknowledge")?;
        self.inner.lock().unwrap().acknowledge(event)
    }
    fn bump_attempts(&mut self, event: EventId) -> Result<u32, LedgerError> {
        self.check("bump_attempts")?;
        self.inner.lock().unwrap().bump_attempts(event)
    }
    fn open_publications(&self, limit: usize) -> Result<Vec<OpenPublication>, LedgerError> {
        self.inner.lock().unwrap().open_publications(limit)
    }

    fn open_publications_for(
        &self,
        run: RunId,
        limit: usize,
    ) -> Result<Vec<OpenPublication>, LedgerError> {
        self.inner
            .lock()
            .unwrap()
            .open_publications_for(Some(run), limit)
    }

    fn publication(
        &self,
        event: EventId,
    ) -> Result<Option<koru_delegate_sqlite::PublicationRecord>, LedgerError> {
        self.inner.lock().unwrap().publication(event)
    }
    fn advance_cursor(
        &mut self,
        run: RunId,
        projection: Projection,
        timeline_index: u64,
    ) -> Result<WriteOutcome, LedgerError> {
        self.check("advance_cursor")?;
        self.inner
            .lock()
            .unwrap()
            .advance_cursor(run, projection, timeline_index)
    }
}

impl ScheduleLedger for RecordingStore {
    fn claim_run(
        &mut self,
        authority: koru_delegate_document::ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<koru_delegate_sqlite::ClaimedSlot, LedgerError> {
        ScheduleLedger::claim_run(&mut *self.inner.lock().unwrap(), authority, claimed_at_ms)
    }

    fn claim_run_with_missed(
        &mut self,
        run: koru_delegate_document::ScheduleClaimAuthority,
        missed: koru_delegate_document::ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<
        (
            koru_delegate_sqlite::ClaimedSlot,
            koru_delegate_sqlite::SkippedSlot,
        ),
        LedgerError,
    > {
        ScheduleLedger::claim_run_with_missed(
            &mut *self.inner.lock().unwrap(),
            run,
            missed,
            claimed_at_ms,
        )
    }

    fn claim_skip(
        &mut self,
        authority: koru_delegate_document::ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<koru_delegate_sqlite::SkippedSlot, LedgerError> {
        ScheduleLedger::claim_skip(&mut *self.inner.lock().unwrap(), authority, claimed_at_ms)
    }

    fn claim_skip_with_missed(
        &mut self,
        due: koru_delegate_document::ScheduleClaimAuthority,
        missed: koru_delegate_document::ScheduleClaimAuthority,
        claimed_at_ms: i64,
    ) -> Result<
        (
            koru_delegate_sqlite::SkippedSlot,
            koru_delegate_sqlite::SkippedSlot,
        ),
        LedgerError,
    > {
        ScheduleLedger::claim_skip_with_missed(
            &mut *self.inner.lock().unwrap(),
            due,
            missed,
            claimed_at_ms,
        )
    }

    fn settle(
        &mut self,
        schedule: koru_delegate_core::ScheduleId,
        occurrence_ms: i64,
        outcome: koru_delegate_sqlite::SlotOutcome,
    ) -> Result<WriteOutcome, LedgerError> {
        self.check("schedule:settle")?;
        ScheduleLedger::settle(
            &mut *self.inner.lock().unwrap(),
            schedule,
            occurrence_ms,
            outcome,
        )
    }

    fn unsettled(&self) -> Result<Vec<koru_delegate_sqlite::SlotClaim>, LedgerError> {
        ScheduleLedger::unsettled(&*self.inner.lock().unwrap())
    }

    fn by_run(&self, run: RunId) -> Result<Option<koru_delegate_sqlite::SlotClaim>, LedgerError> {
        self.check("schedule:by_run")?;
        ScheduleLedger::by_run(&*self.inner.lock().unwrap(), run)
    }

    fn intent_scope(&self, run: RunId) -> Result<Option<WorkspaceScope>, LedgerError> {
        ScheduleLedger::intent_scope(&*self.inner.lock().unwrap(), run)
    }

    fn observed_through_ms(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Result<Option<i64>, LedgerError> {
        ScheduleLedger::observed_through_ms(&*self.inner.lock().unwrap(), schedule)
    }

    fn claims_of(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Result<Vec<koru_delegate_sqlite::SlotClaim>, LedgerError> {
        ScheduleLedger::claims_of(&*self.inner.lock().unwrap(), schedule)
    }
}

// ----------------------------------------------------------- the documents

/// The Run documents this device "stored": snapshots by id, shared with
/// every handle so a reopen reads what a commit wrote.
#[derive(Default)]
pub(crate) struct DocumentScript {
    pub snapshots: HashMap<DocumentId, Vec<u8>>,
    pub by_run: HashMap<RunId, DocumentId>,
    /// Documents the workspace's bin holds, and when they went in. The
    /// window moves a run into the bin, so a scenario about the trash puts
    /// the entry here by hand rather than through a verb this crate has.
    pub trashed: HashMap<DocumentId, Option<i64>>,
    pub fail_create: bool,
    pub fail_commit: bool,
    /// The host has not yet called the commits durable: a fact must wait.
    pub not_durable: bool,
    pub commits: usize,
    pub committed_snapshots: Vec<Vec<u8>>,
}

pub(crate) struct MemoryDocuments {
    script: Arc<Mutex<DocumentScript>>,
}

struct MemoryHandle {
    id: DocumentId,
    doc: RunDocument,
    script: Arc<Mutex<DocumentScript>>,
}

impl RunDocumentHandle for MemoryHandle {
    fn document(&self) -> &RunDocument {
        &self.doc
    }

    fn commit(
        &mut self,
        write: &mut dyn FnMut(&RunDocument) -> Result<(), RunDocumentError>,
    ) -> PortResult<()> {
        let mut script = self.script.lock().unwrap();
        if script.fail_commit {
            return Err(PortError::new(
                Port::Documents,
                "injected",
                "commit refused",
            ));
        }
        write(&self.doc)?;
        script.commits += 1;
        script
            .committed_snapshots
            .push(self.doc.export_snapshot().unwrap());
        script
            .snapshots
            .insert(self.id, self.doc.export_snapshot().unwrap());
        Ok(())
    }

    fn durable(&self) -> bool {
        !self.script.lock().unwrap().not_durable
    }

    fn settle(&mut self) -> PortResult<()> {
        // The scripted host settles the moment it is asked to.
        self.script.lock().unwrap().not_durable = false;
        Ok(())
    }
}

impl RunDocuments for MemoryDocuments {
    fn create(&mut self, _workspace: WorkspaceId, header: &RunHeader) -> PortResult<DocumentId> {
        let mut script = self.script.lock().unwrap();
        if script.fail_create {
            return Err(PortError::new(
                Port::Documents,
                "injected",
                "create refused",
            ));
        }
        if let Some(existing) = script.by_run.get(&header.run) {
            return Ok(*existing);
        }
        let id = DocumentId::from_uuid(Uuid::new_v4());
        let document = RunDocument::create(header).unwrap();
        script
            .snapshots
            .insert(id, document.export_snapshot().unwrap());
        script.by_run.insert(header.run, id);
        Ok(id)
    }

    fn open(
        &mut self,
        _workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn RunDocumentHandle>> {
        let script = self.script.lock().unwrap();
        let bytes = script
            .snapshots
            .get(&document)
            .ok_or_else(|| PortError::new(Port::Documents, "not_found", "no such document"))?;
        let doc = RunDocument::from_snapshot(bytes)?;
        Ok(Box::new(MemoryHandle {
            id: document,
            doc,
            script: Arc::clone(&self.script),
        }))
    }

    fn list(&mut self, _workspace: WorkspaceId) -> PortResult<Vec<crate::ListedRun>> {
        let script = self.script.lock().unwrap();
        let mut out = Vec::new();
        for (id, bytes) in &script.snapshots {
            let doc = RunDocument::from_snapshot(bytes)?;
            out.push(crate::ListedRun {
                document: *id,
                header: doc.header()?,
                trashed_at_ms: script.trashed.get(id).copied(),
            });
        }
        Ok(out)
    }
}

// ------------------------------------------------------------ the sessions

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Call {
    Create {
        key: IdempotencyKey,
        /// Whether the launch carried the adapter's resume token (§7.6).
        resumes: bool,
    },
    Lookup(IdempotencyKey),
    Retire(IdempotencyKey),
    Attach(SessionId),
    Release(SessionId),
    Submit {
        session: SessionId,
        text: String,
    },
    CancelTurn {
        session: SessionId,
        turn: AgentTurnId,
        reason: CancelReason,
    },
    Terminate(SessionId),
    ResumeToken(SessionId),
    Observe(SessionId),
    Subscribe(SessionId),
}

pub(crate) struct SessionScript {
    pub supports_delegation: bool,
    pub profile_delegatable: bool,
    pub host: HostInstanceId,
    pub calls: Vec<Call>,
    /// Every delegated launch the host was handed, in order: what the
    /// adapter would have rendered.
    pub launched: Vec<DelegationLaunch>,
    /// What the next creation answers with; `None` refuses it.
    pub create: Option<CreatedSession>,
    pub create_error: Option<&'static str>,
    /// The turn ids `submit` hands back, in order.
    pub turns: VecDeque<AgentTurnId>,
    pub lookups: HashMap<IdempotencyKey, (HostInstanceId, CreationLookup)>,
    pub summaries: HashMap<SessionId, SessionSummary>,
    pub observation: AgentEventObservation,
    pub attach_fails: bool,
    /// Keys the host has been asked about, oldest first, for the re-ask
    /// scenario: each lookup pops the next answer when several are given.
    pub lookup_sequence: VecDeque<(HostInstanceId, CreationLookup)>,
    /// What the adapter's resume seam answers for a session. An empty map
    /// is every adapter that declared no native resume — Codex is one —
    /// and the recorded-context fallback follows.
    pub resume_tokens: HashMap<SessionId, ProviderResumeToken>,
    /// The next `cancel_turn` is refused with this code.
    pub cancel_refused: Option<&'static str>,
}

impl Default for SessionScript {
    fn default() -> Self {
        Self {
            supports_delegation: true,
            profile_delegatable: true,
            create_error: None,
            host: host(1),
            calls: Vec::new(),
            launched: Vec::new(),
            create: Some(CreatedSession {
                session: session(1),
                provider: "codex".parse().unwrap(),
                requested_model: Some("fixture-model".parse().unwrap()),
            }),
            turns: VecDeque::from([turn(1), turn(2), turn(3), turn(4)]),
            lookups: HashMap::new(),
            summaries: HashMap::new(),
            observation: AgentEventObservation {
                events: Vec::new(),
                cursor: StreamSequence::new(1).unwrap(),
                earlier_truncated: false,
                commands: None,
                session_cursor: None,
            },
            attach_fails: false,
            lookup_sequence: VecDeque::new(),
            resume_tokens: HashMap::new(),
            cancel_refused: None,
        }
    }
}

pub(crate) struct ScriptedSessions {
    script: Arc<Mutex<SessionScript>>,
}

/// A stream the scenario never pumps: the tests call `handle` directly.
pub(crate) struct NoEvents;

impl SessionEvents for NoEvents {
    fn next(&mut self, _timeout: Duration) -> Option<SessionUpdate> {
        None
    }
}

impl Sessions for ScriptedSessions {
    fn supports_delegation(&self) -> bool {
        self.script.lock().unwrap().supports_delegation
    }
    fn profile_delegatable(&mut self, _profile: &SessionProfileKey) -> PortResult<bool> {
        Ok(self.script.lock().unwrap().profile_delegatable)
    }
    fn host_instance(&mut self) -> PortResult<HostInstanceId> {
        Ok(self.script.lock().unwrap().host)
    }
    fn create_delegated(&mut self, launch: SessionLaunch) -> PortResult<CreatedSession> {
        let mut script = self.script.lock().unwrap();
        script.calls.push(Call::Create {
            key: launch.key,
            resumes: launch.delegation.resume.is_some(),
        });
        script.launched.push(launch.delegation.clone());
        if let Some(code) = script.create_error {
            return Err(PortError::new(
                Port::Sessions,
                code,
                "SECRET provider message",
            ));
        }
        script
            .create
            .clone()
            .ok_or_else(|| PortError::new(Port::Sessions, "provider_failed", "refused"))
    }
    fn lookup(&mut self, key: IdempotencyKey) -> PortResult<(HostInstanceId, CreationLookup)> {
        let mut script = self.script.lock().unwrap();
        script.calls.push(Call::Lookup(key));
        if let Some(next) = script.lookup_sequence.pop_front() {
            return Ok(next);
        }
        let host = script.host;
        Ok(script
            .lookups
            .get(&key)
            .cloned()
            .unwrap_or((host, CreationLookup::NoEvidence)))
    }
    fn retire(&mut self, key: IdempotencyKey) -> PortResult<(HostInstanceId, CreationLookup)> {
        let mut script = self.script.lock().unwrap();
        script.calls.push(Call::Retire(key));
        let host = script.host;
        Ok(script
            .lookups
            .get(&key)
            .cloned()
            .unwrap_or((host, CreationLookup::NoEvidence)))
    }
    fn summary(&mut self, session: SessionId) -> PortResult<Option<SessionSummary>> {
        Ok(self.script.lock().unwrap().summaries.get(&session).cloned())
    }
    fn attach(&mut self, session: SessionId) -> PortResult<()> {
        let mut script = self.script.lock().unwrap();
        script.calls.push(Call::Attach(session));
        if script.attach_fails {
            return Err(PortError::new(
                Port::Sessions,
                "controller_conflict",
                "held",
            ));
        }
        Ok(())
    }
    fn release_control(&mut self, session: SessionId) -> PortResult<()> {
        self.script
            .lock()
            .unwrap()
            .calls
            .push(Call::Release(session));
        Ok(())
    }
    fn submit(&mut self, session: SessionId, text: String) -> PortResult<AgentTurnId> {
        let mut script = self.script.lock().unwrap();
        script.calls.push(Call::Submit { session, text });
        script
            .turns
            .pop_front()
            .ok_or_else(|| PortError::new(Port::Sessions, "exhausted", "no scripted turn"))
    }
    fn cancel_turn(
        &mut self,
        session: SessionId,
        turn: AgentTurnId,
        reason: CancelReason,
    ) -> PortResult<()> {
        let mut script = self.script.lock().unwrap();
        script.calls.push(Call::CancelTurn {
            session,
            turn,
            reason,
        });
        match script.cancel_refused.take() {
            Some(code) => Err(PortError::new(Port::Sessions, code, "refused")),
            None => Ok(()),
        }
    }
    fn terminate(&mut self, session: SessionId) -> PortResult<()> {
        self.script
            .lock()
            .unwrap()
            .calls
            .push(Call::Terminate(session));
        Ok(())
    }
    fn resume_token(&mut self, session: SessionId) -> PortResult<Option<ProviderResumeToken>> {
        let mut script = self.script.lock().unwrap();
        script.calls.push(Call::ResumeToken(session));
        Ok(script.resume_tokens.get(&session).cloned())
    }
    fn observe(&mut self, session: SessionId) -> PortResult<AgentEventObservation> {
        let mut script = self.script.lock().unwrap();
        script.calls.push(Call::Observe(session));
        Ok(script.observation.clone())
    }
    fn subscribe(
        &mut self,
        session: SessionId,
        _after: Option<StreamSequence>,
    ) -> PortResult<Box<dyn SessionEvents>> {
        self.script
            .lock()
            .unwrap()
            .calls
            .push(Call::Subscribe(session));
        Ok(Box::new(NoEvents))
    }
}

// ------------------------------------------------------------- the capture

#[derive(Default)]
pub(crate) struct CaptureScript {
    /// Schedule pointer resolutions, before a claim is allowed to exist.
    pub resolved: Vec<(
        RunId,
        Vec<koru_delegate_schedule::ScheduleContextPointer>,
        WorkspaceId,
    )>,
    pub fail_resolve: Option<CaptureFailure>,
    pub fail_capture: Option<CaptureFailure>,
    pub fail_materialize: Option<MaterializeFailure>,
    /// D48: what admission answers for a resume. `None` admits every pin.
    pub refuse_admission: Option<SourceRefusal>,
    /// Every admission asked for, with the pins it was asked about — so a
    /// scenario can prove a resume kept the original pins (§7.2, D10).
    pub admitted: Vec<(RunId, Vec<ContextPin>)>,
    pub materialized: Vec<PathBuf>,
    pub cleaned: Vec<PathBuf>,
    /// What each pin points at now (D46). Empty is *not resolved*, which
    /// is what a device that cannot reach the sources answers.
    pub states: Vec<koru_delegate_core::ContextState>,
}

pub(crate) struct PinningCapture {
    script: Arc<Mutex<CaptureScript>>,
}

impl ContextCapture for PinningCapture {
    fn resolve_and_pin(
        &mut self,
        run: RunId,
        pointers: &[koru_delegate_schedule::ScheduleContextPointer],
        workspace: WorkspaceId,
    ) -> Result<Vec<RunContextItem>, CaptureFailure> {
        let mut script = self.script.lock().unwrap();
        script.resolved.push((run, pointers.to_vec(), workspace));
        if let Some(failure) = script.fail_resolve {
            return Err(failure);
        }
        let version = koru_delegate_core::DocumentVersionId::from_uuid(v4(0xc0
            + u8::try_from(script.resolved.len())
                .unwrap_or(0x0f)
                .min(0x0f)));
        Ok(pointers
            .iter()
            .map(|pointer| match pointer {
                koru_delegate_schedule::ScheduleContextPointer::Document {
                    workspace,
                    document,
                } => RunContextItem::Document {
                    workspace: *workspace,
                    document: *document,
                    version,
                },
                koru_delegate_schedule::ScheduleContextPointer::Block {
                    workspace,
                    document,
                    node,
                } => RunContextItem::Block {
                    workspace: *workspace,
                    document: *document,
                    node: *node,
                    version,
                },
                koru_delegate_schedule::ScheduleContextPointer::Blob {
                    workspace,
                    blob,
                    blob_kind,
                } => RunContextItem::Blob {
                    workspace: *workspace,
                    blob: blob.clone(),
                    blob_kind: *blob_kind,
                },
            })
            .collect())
    }

    fn admit(&mut self, run: RunId, pins: &[ContextPin]) -> Result<(), SourceRefusal> {
        let mut script = self.script.lock().unwrap();
        script.admitted.push((run, pins.to_vec()));
        match script.refuse_admission {
            Some(refusal) => Err(refusal),
            None => Ok(()),
        }
    }
    fn capture(
        &mut self,
        _run: RunId,
        items: &[RunContextItem],
        _workspace: WorkspaceId,
    ) -> Result<CapturedContext, CaptureFailure> {
        if let Some(failure) = self.script.lock().unwrap().fail_capture {
            return Err(failure);
        }
        Ok(CapturedContext {
            pins: items.to_vec(),
        })
    }
    fn materialize(
        &mut self,
        _run: RunId,
        captured: &CapturedContext,
        run_workspace: &Path,
    ) -> Result<MaterializedContext, MaterializeFailure> {
        let mut script = self.script.lock().unwrap();
        if let Some(failure) = script.fail_materialize {
            return Err(failure);
        }
        script.materialized.push(run_workspace.to_path_buf());
        Ok(MaterializedContext {
            directory: run_workspace.join(".koru/context"),
            items: captured.pins.len() as u32,
        })
    }
    fn cleanup(&mut self, _run: RunId, run_workspace: &Path) {
        self.script
            .lock()
            .unwrap()
            .cleaned
            .push(run_workspace.to_path_buf());
    }
    fn states(&mut self, _items: &[RunContextItem]) -> Vec<koru_delegate_core::ContextState> {
        self.script.lock().unwrap().states.clone()
    }
}

// --------------------------------------------------------------- the bus

#[derive(Default)]
pub(crate) struct BusScript {
    pub published: Vec<(EventId, RunFact)>,
    pub fail_next: Option<PublishFailure>,
}

pub(crate) struct RecordingBus {
    script: Arc<Mutex<BusScript>>,
}

impl RunEvents for RecordingBus {
    fn publish(&mut self, event: EventId, fact: &RunFact) -> Result<(), PublishFailure> {
        let mut script = self.script.lock().unwrap();
        if let Some(failure) = script.fail_next.take() {
            return Err(failure);
        }
        script.published.push((event, fact.clone()));
        Ok(())
    }
}

// ------------------------------------------------------------ the rest

/// **A resolver that says which time it was asked.** P1.12 re-reads the
/// preamble file at the start of every run rather than caching it, so that
/// a person who edits it needs no restart; a fixture that answered the same
/// bytes forever could not tell that apart from a preamble read once at
/// startup. Each answer carries its own ordinal.
pub(crate) struct FixedPreamble {
    pub resolves: Arc<Mutex<u32>>,
}

impl Preamble for FixedPreamble {
    fn resolve(&mut self) -> koru_delegate_preamble::Resolved {
        let mut resolves = self.resolves.lock().unwrap();
        *resolves += 1;
        koru_delegate_preamble::Resolved {
            text: std::borrow::Cow::Owned(format!("{PREAMBLE_SENTINEL} (read {resolves})")),
            source: koru_delegate_preamble::Source::Bundled,
            notice: None,
            path: None,
        }
    }
}

pub(crate) struct FakeRoots {
    pub projects: HashMap<ProjectId, PathBuf>,
    pub temporary: PathBuf,
}

impl ScopeRoots for FakeRoots {
    fn project_root(&self, project: ProjectId) -> RootLookup {
        match self.projects.get(&project) {
            Some(root) => RootLookup::Directory(root.clone()),
            None => RootLookup::UnknownProject,
        }
    }
    fn temporary_directory(&self, run: RunId) -> Option<PathBuf> {
        let directory = self.temporary.join(run.to_storage());
        std::fs::create_dir_all(&directory).ok()?;
        Some(directory)
    }
}

/// What one `launch` was asked for, as a fake authority would record it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IssuedLaunch {
    pub run: RunId,
    pub generation: AttemptGeneration,
    pub workspace: WorkspaceId,
    pub grants: BTreeSet<WorkspaceId>,
    pub run_workspace: PathBuf,
}

#[derive(Default)]
pub(crate) struct AuthorityLog {
    pub issued: Vec<IssuedLaunch>,
    pub revoked: Vec<RunId>,
    /// The working folder each run's record holds, as the real store would
    /// have kept it from the launch.
    pub workspaces: HashMap<RunId, PathBuf>,
    /// Runs whose record has been forgotten because the run was deleted.
    pub forgotten: Vec<RunId>,
    /// When set, the next `launch` refuses with this code.
    pub refuse: Option<&'static str>,
}

/// A run authority the tests can read: it mints a distinct bearer per
/// attempt, allows the six tools the run endpoint publishes, and records
/// every issue and revocation.
pub(crate) struct StubMcp {
    log: Arc<Mutex<AuthorityLog>>,
}

/// The six, as `run_mcp::tools` spells them.
pub(crate) const RUN_TOOLS: [&str; 6] = [
    "attach_to_run",
    "create",
    "create_version",
    "get",
    "read",
    "search",
];

impl McpEndpoints for StubMcp {
    fn launch(&mut self, launch: McpLaunch<'_>) -> PortResult<DelegationLaunch> {
        let mut log = self.log.lock().unwrap();
        if let Some(code) = log.refuse.take() {
            return Err(PortError::new(Port::Mcp, code, "the fake refused"));
        }
        log.issued.push(IssuedLaunch {
            run: launch.run,
            generation: launch.generation,
            workspace: launch.workspace,
            grants: launch.grants.clone(),
            run_workspace: launch.run_workspace.to_path_buf(),
        });
        log.workspaces
            .insert(launch.run, launch.run_workspace.to_path_buf());
        Ok(DelegationLaunch {
            model: None,
            mcp_endpoint: format!("http://127.0.0.1:1/v1/runs/{}/mcp", launch.run.to_storage())
                .parse()
                .unwrap(),
            credential: Some(
                format!(
                    "koru_run_{}_{}",
                    launch.run.to_storage().replace('-', ""),
                    launch.generation.get()
                )
                .parse()
                .unwrap(),
            ),
            tools: McpToolAllowlist::new(RUN_TOOLS.iter().map(|name| name.parse().unwrap()))
                .unwrap(),
            resume: None,
        })
    }

    fn revoke(&mut self, run: RunId) {
        self.log.lock().unwrap().revoked.push(run);
    }

    fn run_workspace(&mut self, run: RunId) -> PortResult<Option<PathBuf>> {
        Ok(self.log.lock().unwrap().workspaces.get(&run).cloned())
    }

    fn forget(&mut self, run: RunId) -> PortResult<()> {
        let mut log = self.log.lock().unwrap();
        log.workspaces.remove(&run);
        log.forgotten.push(run);
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct RegistrationScript {
    /// Runs whose result families were retired, with the workspace asked.
    pub retired: Vec<(RunId, WorkspaceId)>,
    /// How many families the next retirement answers with.
    pub retires: u32,
    /// Handed back on the next ask, then cleared.
    pub next: Vec<ResultReference>,
    pub asked: Vec<(RunId, RunAttemptId, Option<AgentTurnId>)>,
}

pub(crate) struct ScriptedRegistrations {
    script: Arc<Mutex<RegistrationScript>>,
}

impl ArtifactRegistrations for ScriptedRegistrations {
    fn registered(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        turn: Option<AgentTurnId>,
    ) -> Vec<ResultReference> {
        let mut script = self.script.lock().unwrap();
        script.asked.push((run, attempt, turn));
        std::mem::take(&mut script.next)
    }

    fn retire_results(&mut self, run: RunId, workspace: WorkspaceId) -> PortResult<u32> {
        let mut script = self.script.lock().unwrap();
        script.retired.push((run, workspace));
        Ok(script.retires)
    }
}

pub(crate) struct SteppedClock(Arc<Mutex<i64>>);

impl Clock for SteppedClock {
    fn now_ms(&mut self) -> i64 {
        let mut now = self.0.lock().unwrap();
        *now += 1_000;
        *now
    }
}

pub(crate) struct SharedScheduleClock(pub Arc<AtomicI64>);

impl koru_delegate_sqlite::Clock for SharedScheduleClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::Acquire)
    }
}

impl ScheduleClock for SharedScheduleClock {
    fn now_ms(&mut self, _evaluation_now_ms: i64) -> i64 {
        self.0.load(Ordering::Acquire)
    }
}

pub(crate) struct EvaluationNowClock;

impl ScheduleClock for EvaluationNowClock {
    fn now_ms(&mut self, evaluation_now_ms: i64) -> i64 {
        evaluation_now_ms
    }
}

pub(crate) struct CountingSleep(Arc<Mutex<Vec<Duration>>>);

impl Sleep for CountingSleep {
    fn sleep(&mut self, duration: Duration) {
        self.0.lock().unwrap().push(duration);
    }
}

// ------------------------------------------------------------- the harness

pub(crate) struct Harness {
    pub orchestrator: Orchestrator,
    pub schedule_store: RecordingStore,
    pub schedule_clock: Arc<AtomicI64>,
    pub ledger: Arc<Mutex<LedgerScript>>,
    pub documents: Arc<Mutex<DocumentScript>>,
    pub sessions: Arc<Mutex<SessionScript>>,
    pub capture: Arc<Mutex<CaptureScript>>,
    pub bus: Arc<Mutex<BusScript>>,
    pub registrations: Arc<Mutex<RegistrationScript>>,
    pub authority: Arc<Mutex<AuthorityLog>>,
    pub sleeps: Arc<Mutex<Vec<Duration>>>,
    /// How many times the preamble has been resolved.
    pub preambles: Arc<Mutex<u32>>,
    pub dir: TempDir,
}

impl Harness {
    pub fn new(label: &str) -> Self {
        Self::with_dir(TempDir::new(label))
    }

    /// Publication tests never start a provider or resolve a working folder.
    pub fn without_folders() -> Self {
        Self::with_dir(TempDir {
            root: PathBuf::from("unused-publication-fixture"),
            cleanup: false,
        })
    }

    fn with_dir(dir: TempDir) -> Self {
        let ledger = Arc::new(Mutex::new(LedgerScript::default()));
        let documents = Arc::new(Mutex::new(DocumentScript::default()));
        let sessions = Arc::new(Mutex::new(SessionScript::default()));
        let capture = Arc::new(Mutex::new(CaptureScript::default()));
        let bus = Arc::new(Mutex::new(BusScript::default()));
        let registrations = Arc::new(Mutex::new(RegistrationScript::default()));
        let authority = Arc::new(Mutex::new(AuthorityLog::default()));
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let preambles = Arc::new(Mutex::new(0));
        let mut projects = HashMap::new();
        let project_dir = dir.root.join("project");
        if dir.cleanup {
            std::fs::create_dir_all(&project_dir).unwrap();
        }
        projects.insert(project(1), project_dir);
        let schedule_clock = Arc::new(AtomicI64::new(1_700_000_000_000));
        let schedule_store = RecordingStore {
            inner: Arc::new(Mutex::new(
                RunsDb::open_with_clock(
                    Path::new(":memory:"),
                    Box::new(SharedScheduleClock(Arc::clone(&schedule_clock))),
                )
                .unwrap(),
            )),
            script: Arc::clone(&ledger),
        };
        let ports = Ports {
            store: Box::new(schedule_store.clone()),
            documents: Box::new(MemoryDocuments {
                script: Arc::clone(&documents),
            }),
            sessions: Box::new(ScriptedSessions {
                script: Arc::clone(&sessions),
            }),
            capture: Box::new(PinningCapture {
                script: Arc::clone(&capture),
            }),
            events: Box::new(RecordingBus {
                script: Arc::clone(&bus),
            }),
            preamble: Box::new(FixedPreamble {
                resolves: Arc::clone(&preambles),
            }),
            roots: Box::new(FakeRoots {
                projects,
                temporary: dir.root.join("temporary"),
            }),
            mcp: Box::new(StubMcp {
                log: Arc::clone(&authority),
            }),
            registrations: Box::new(ScriptedRegistrations {
                script: Arc::clone(&registrations),
            }),
            clock: Box::new(SteppedClock(Arc::new(Mutex::new(1_700_000_000_000)))),
            sleep: Box::new(CountingSleep(Arc::clone(&sleeps))),
        };
        let orchestrator = Orchestrator::new(
            ports,
            Options {
                reask_attempts: 3,
                ..Options::default()
            },
        );
        Self {
            orchestrator,
            schedule_store,
            schedule_clock,
            ledger,
            documents,
            sessions,
            capture,
            bus,
            registrations,
            authority,
            sleeps,
            preambles,
            dir,
        }
    }

    pub fn scope(&self) -> WorkspaceScope {
        WorkspaceScope::ProjectRoot {
            project: project(1),
        }
    }

    /// Create and start a run in the project scope; the provider is up and
    /// the first turn submitted.
    pub fn running(&mut self, n: u8) -> RunId {
        let run = run_id(n);
        self.orchestrator
            .create(request(run, self.scope()), workspace())
            .unwrap();
        let started = self.orchestrator.start(run, StartOrigin::Manual).unwrap();
        assert!(started.events.is_some(), "{started:?}");
        run
    }

    pub fn status(&mut self, run: RunId) -> koru_delegate_core::RunStatus {
        self.orchestrator.header(run).unwrap().status
    }

    pub fn steps(&mut self, run: RunId) -> Vec<koru_delegate_core::StepKind> {
        self.orchestrator
            .view(run, None, 0, usize::MAX)
            .unwrap()
            .timeline
            .items
            .into_iter()
            .filter_map(|item| match item {
                WindowItem::Step(entry) => Some(entry.kind),
                _ => None,
            })
            .collect()
    }

    pub fn usage_facts(&mut self, run: RunId) -> Vec<koru_delegate_core::UsageFact> {
        self.orchestrator
            .view(run, None, 0, usize::MAX)
            .unwrap()
            .timeline
            .items
            .into_iter()
            .filter_map(|item| match item {
                WindowItem::Usage(fact) => Some(fact),
                _ => None,
            })
            .collect()
    }

    pub fn facts(&self) -> Vec<RunFact> {
        self.bus
            .lock()
            .unwrap()
            .published
            .iter()
            .map(|(_, fact)| fact.clone())
            .collect()
    }

    pub fn calls(&self) -> Vec<Call> {
        self.sessions.lock().unwrap().calls.clone()
    }

    /// Every submission's text, oldest first.
    pub fn submissions(&self) -> Vec<String> {
        self.sessions
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter_map(|call| match call {
                Call::Submit { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn ops(&self) -> Vec<&'static str> {
        self.ledger.lock().unwrap().ops.clone()
    }

    pub fn attempt(&mut self, run: RunId) -> AttemptRow {
        self.orchestrator
            .ports_mut()
            .store
            .current_attempt(run)
            .unwrap()
            .unwrap()
    }

    pub fn agent(&mut self, run: RunId, event: AgentEvent) -> HandleOutcome {
        self.orchestrator
            .handle(
                run,
                SessionUpdate::Agent(SequencedAgentEvent {
                    session_id: session(1),
                    sequence: StreamSequence::new(1).unwrap(),
                    event,
                }),
            )
            .unwrap()
    }

    pub fn session_event(&mut self, run: RunId, event: SessionEvent) -> HandleOutcome {
        self.orchestrator
            .handle(
                run,
                SessionUpdate::Session(SequencedSessionEvent {
                    session_id: session(1),
                    sequence: StreamSequence::new(1).unwrap(),
                    event,
                }),
            )
            .unwrap()
    }
}

/// A second orchestrator over the same fakes — a worker restarted with the
/// same ledger, documents and host — with the lookup answering `lookup`.
pub(crate) fn restarted(h: Harness, lookup: (HostInstanceId, CreationLookup)) -> Harness {
    let Harness {
        orchestrator,
        schedule_store,
        schedule_clock,
        ledger,
        documents,
        sessions,
        capture,
        bus,
        registrations,
        authority,
        sleeps,
        preambles,
        dir,
    } = h;
    let ports = orchestrator.into_ports();
    {
        let mut script = sessions.lock().unwrap();
        script.calls.clear();
        script.lookup_sequence.clear();
        script.lookups.clear();
        let key = ports
            .store
            .live_attempts()
            .unwrap()
            .into_iter()
            .find_map(|row| row.provider_session.map(|key| key.idempotency));
        if let Some(key) = key {
            script.lookups.insert(key, lookup);
        }
    }
    Harness {
        orchestrator: Orchestrator::new(
            ports,
            Options {
                reask_attempts: 3,
                ..Options::default()
            },
        ),
        schedule_store,
        schedule_clock,
        ledger,
        documents,
        sessions,
        capture,
        bus,
        registrations,
        authority,
        sleeps,
        preambles,
        dir,
    }
}

pub(crate) fn summary(managed: ManagedState, active_turn: Option<AgentTurnId>) -> SessionSummary {
    SessionSummary {
        id: session(1),
        parent_session_id: None,
        profile: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
        display_name: String::new(),
        working_directory: None,
        mode: SessionMode::Managed {
            provider: "codex".parse().unwrap(),
        },
        process: ProcessState::Running,
        controller: ControllerState::Detached,
        managed: Some(managed),
        active_turn_id: active_turn,
        attention: Default::default(),
        pending_attachment_count: 0,
        capabilities: Default::default(),
        project_id: None,
        handover: Default::default(),
        model: None,
        origin: Default::default(),
        created_at_ms: 0,
    }
}

pub(crate) fn artifact(n: u8) -> ResultReference {
    ResultReference {
        artifact: koru_delegate_core::ArtifactVersionId::from_uuid(v4(0x90 + n)),
        held_by: DeviceId::from_uuid(v4(0xe1)),
        title: koru_delegate_core::ArtifactTitle::new("result").unwrap(),
        size_bytes: 42,
        registered_at_ms: 0,
    }
}
