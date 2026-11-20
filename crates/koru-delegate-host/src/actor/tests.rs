// SPDX-License-Identifier: MPL-2.0

use std::{collections::BTreeSet, path::PathBuf};

use koru_delegate_api::wire::CreateRunRequest;
use koru_delegate_core::{
    ActorRef, AgentTurnId, DocumentId, HostInstanceId, IdempotencyKey, Objective, ProfileId,
    ResultReference, RunContextItem, RunHeader, RunId, SessionId, SessionProfileKey, WorkspaceId,
    WorkspaceScope,
};
use koru_delegate_runtime::{
    ArtifactRegistrations, CaptureFailure, CapturedContext, Clock, ContextCapture, CreatedSession,
    MaterializeFailure, MaterializedContext, McpEndpoints, McpLaunch, Options, PortResult,
    Preamble, PublishFailure, RunDocumentHandle, RunDocuments, RunEvents, SessionEvents,
    SessionLaunch, Sessions, Sleep, SourceRefusal,
};
use koru_delegate_sqlite::RunsDb;
use koru_session_protocol::{
    AgentEventObservation, CancelReason, CreationLookup, ProviderResumeToken, SessionSummary,
    StreamSequence,
};

use super::*;

#[test]
fn stale_pump_messages_cannot_replace_current_attempt_observation() {
    let (mut actor, _, _) = actor();
    let run = RunId::mint();
    let epoch = uuid::Uuid::new_v4();
    actor.pumps.insert(
        run,
        Pump {
            stop: Arc::new(AtomicBool::new(false)),
            epoch,
        },
    );
    let stale = uuid::Uuid::new_v4();
    actor.handle(Command::Update {
        run,
        epoch: stale,
        update: SessionUpdate::Closed { error: None },
    });
    actor.handle(Command::PumpEnded { run, epoch: stale });
    assert_eq!(actor.pumps[&run].epoch, epoch);
    actor.handle(Command::PumpEnded { run, epoch });
    assert!(!actor.pumps.contains_key(&run));
}

struct NeverDocuments;
impl RunDocuments for NeverDocuments {
    fn create(&mut self, _: WorkspaceId, _: &RunHeader) -> PortResult<DocumentId> {
        unreachable!()
    }
    fn open(&mut self, _: WorkspaceId, _: DocumentId) -> PortResult<Box<dyn RunDocumentHandle>> {
        unreachable!()
    }
    fn list(&mut self, _: WorkspaceId) -> PortResult<Vec<koru_delegate_runtime::ListedRun>> {
        unreachable!()
    }
}

struct NeverSessions;
impl Sessions for NeverSessions {
    fn supports_delegation(&self) -> bool {
        unreachable!()
    }
    fn profile_delegatable(&mut self, _: &SessionProfileKey) -> PortResult<bool> {
        unreachable!()
    }
    fn host_instance(&mut self) -> PortResult<HostInstanceId> {
        unreachable!()
    }
    fn create_delegated(&mut self, _: SessionLaunch) -> PortResult<CreatedSession> {
        unreachable!()
    }
    fn lookup(&mut self, _: IdempotencyKey) -> PortResult<(HostInstanceId, CreationLookup)> {
        unreachable!()
    }
    fn retire(&mut self, _: IdempotencyKey) -> PortResult<(HostInstanceId, CreationLookup)> {
        unreachable!()
    }
    fn summary(&mut self, _: SessionId) -> PortResult<Option<SessionSummary>> {
        unreachable!()
    }
    fn attach(&mut self, _: SessionId) -> PortResult<()> {
        unreachable!()
    }
    fn release_control(&mut self, _: SessionId) -> PortResult<()> {
        unreachable!()
    }
    fn submit(&mut self, _: SessionId, _: String) -> PortResult<AgentTurnId> {
        unreachable!()
    }
    fn cancel_turn(&mut self, _: SessionId, _: AgentTurnId, _: CancelReason) -> PortResult<()> {
        unreachable!()
    }
    fn terminate(&mut self, _: SessionId) -> PortResult<()> {
        unreachable!()
    }
    fn resume_token(&mut self, _: SessionId) -> PortResult<Option<ProviderResumeToken>> {
        unreachable!()
    }
    fn observe(&mut self, _: SessionId) -> PortResult<AgentEventObservation> {
        unreachable!()
    }
    fn subscribe(
        &mut self,
        _: SessionId,
        _: Option<StreamSequence>,
    ) -> PortResult<Box<dyn SessionEvents>> {
        unreachable!()
    }
}

struct NeverCapture;
impl ContextCapture for NeverCapture {
    fn resolve_and_pin(
        &mut self,
        _: RunId,
        _: &[koru_delegate_schedule::ScheduleContextPointer],
        _: WorkspaceId,
    ) -> Result<Vec<RunContextItem>, CaptureFailure> {
        unreachable!()
    }
    fn admit(&mut self, _: RunId, _: &[RunContextItem]) -> Result<(), SourceRefusal> {
        unreachable!()
    }
    fn capture(
        &mut self,
        _: RunId,
        _: &[RunContextItem],
        _: WorkspaceId,
    ) -> Result<CapturedContext, CaptureFailure> {
        unreachable!()
    }
    fn materialize(
        &mut self,
        _: RunId,
        _: &CapturedContext,
        _: &std::path::Path,
    ) -> Result<MaterializedContext, MaterializeFailure> {
        unreachable!()
    }
    fn cleanup(&mut self, _: RunId, _: &std::path::Path) {
        unreachable!()
    }
}

struct NeverEvents;
impl RunEvents for NeverEvents {
    fn publish(
        &mut self,
        _: koru_delegate_core::EventId,
        _: &koru_delegate_runtime::RunFact,
    ) -> Result<(), PublishFailure> {
        unreachable!()
    }
}
struct NeverPreamble;
impl Preamble for NeverPreamble {
    fn resolve(&mut self) -> koru_delegate_preamble::Resolved {
        unreachable!()
    }
}
struct NeverRoots;
impl koru_delegate_scope::ScopeRoots for NeverRoots {
    fn project_root(&self, _: koru_delegate_core::ProjectId) -> koru_delegate_runtime::RootLookup {
        koru_delegate_runtime::RootLookup::UnknownProject
    }
    fn temporary_directory(&self, _: RunId) -> Option<PathBuf> {
        None
    }
}
struct NeverMcp;
impl McpEndpoints for NeverMcp {
    fn launch(&mut self, _: McpLaunch<'_>) -> PortResult<koru_session_protocol::DelegationLaunch> {
        unreachable!()
    }
}
struct NeverRegistrations;
impl ArtifactRegistrations for NeverRegistrations {
    fn registered(
        &mut self,
        _: RunId,
        _: koru_delegate_core::RunAttemptId,
        _: Option<AgentTurnId>,
    ) -> Vec<ResultReference> {
        unreachable!()
    }
}
struct FixedClock;
impl Clock for FixedClock {
    fn now_ms(&mut self) -> i64 {
        0
    }
}
struct NoSleep;
impl Sleep for NoSleep {
    fn sleep(&mut self, _: Duration) {}
}

fn actor() -> (Actor, mpsc::Sender<Command>, mpsc::Receiver<Command>) {
    let ports = koru_delegate_runtime::Ports {
        store: Box::new(RunsDb::open(std::path::Path::new(":memory:")).unwrap()),
        documents: Box::new(NeverDocuments),
        sessions: Box::new(NeverSessions),
        capture: Box::new(NeverCapture),
        events: Box::new(NeverEvents),
        preamble: Box::new(NeverPreamble),
        roots: Box::new(NeverRoots),
        mcp: Box::new(NeverMcp),
        registrations: Box::new(NeverRegistrations),
        clock: Box::new(FixedClock),
        sleep: Box::new(NoSleep),
    };
    let orchestrator = Orchestrator::new(ports, Options::default());
    let (commands, inbox) = mpsc::channel();
    (
        Actor::new(
            orchestrator,
            commands.clone(),
            uuid::Uuid::nil().to_string(),
        ),
        commands,
        inbox,
    )
}

fn create(run: RunId) -> CreateRunRequest {
    CreateRunRequest {
        run,
        workspace: WorkspaceId::new(),
        requested_by: ActorRef::Person {
            profile: ProfileId::new(),
        },
        objective: Objective::new("deferred fixture").unwrap(),
        agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
        requested_model: None,
        scope: WorkspaceScope::Temporary,
        context: Vec::new(),
        grants: BTreeSet::new(),
    }
}

#[test]
fn a_live_schedule_admission_defers_create_and_resume_in_arrival_order() {
    let (mut actor, _commands, _inbox) = actor();
    actor.schedule_admission = Some(RunId::mint());
    let (create_reply, _create_answer) = mpsc::sync_channel(1);
    let (resume_reply, _resume_answer) = mpsc::sync_channel(1);
    actor.handle(Command::Create {
        request: create(RunId::mint()),
        reply: create_reply,
    });
    actor.handle(Command::Resume {
        run: RunId::mint(),
        confirm: true,
        reply: resume_reply,
    });
    assert_eq!(actor.deferred_starts.len(), 2);
    assert!(matches!(actor.deferred_starts[0], Command::Create { .. }));
    assert!(matches!(actor.deferred_starts[1], Command::Resume { .. }));
}

#[test]
fn stopped_scheduler_ack_releases_a_retained_postclaim_lease() {
    let (mut actor, commands, inbox) = actor();
    let admission = RunId::mint();
    actor.schedule_admission = Some(admission);
    actor.attach_scheduler(crate::scheduler::test_control(commands));
    let (stop, stopped) = mpsc::channel();
    stop.send("test stop").unwrap();
    actor.run(inbox, stopped);
    assert_eq!(actor.schedule_admission, None);
}

#[test]
fn a_failed_postclaim_finish_can_retry_without_reopening_a_preclaim_lease() {
    let (mut actor, _commands, _inbox) = actor();
    let run = RunId::mint();
    let scope = ScopeIdentity::of_directory(std::path::Path::new(".")).unwrap();
    actor.schedule_admission = Some(run);
    assert!(actor.may_finish_schedule(run));

    actor.record_schedule_finish(run, WorkspaceScope::Temporary, scope.clone(), false);
    assert_eq!(actor.schedule_admission, None);
    assert!(
        actor.may_finish_schedule(run),
        "the durable claim, not the expired preclaim lease, authorizes retry"
    );

    actor.record_schedule_finish(run, WorkspaceScope::Temporary, scope, true);
    assert!(!actor.may_finish_schedule(run));
}

#[test]
fn claimed_reservation_answers_a_conflicting_person_without_waiting() {
    let (mut actor, _commands, _inbox) = actor();
    let run = RunId::mint();
    assert!(
        !actor.scope_is_reserved(run, &WorkspaceScope::Temporary),
        "an empty reservation set never asks roots or creates a folder"
    );
    let identity = ScopeIdentity::of_directory(std::path::Path::new(".")).unwrap();
    actor
        .claimed_schedule_retries
        .push(ScheduledScopeReservation {
            run,
            scope: WorkspaceScope::Temporary,
            identity: Some(identity),
            resolution: ScopeReservationResolution::Exact,
        });
    let (reply, answer) = mpsc::sync_channel(1);
    actor.handle(Command::Create {
        request: create(run),
        reply,
    });
    assert!(matches!(answer.recv().unwrap(), Response::Failure(_)));
    assert!(actor.deferred_starts.is_empty());
}

#[test]
fn restart_reservation_precedes_the_next_person_command() {
    let (mut actor, _commands, _inbox) = actor();
    let claimed = RunId::mint();
    let identity = ScopeIdentity::of_directory(std::path::Path::new(".")).unwrap();
    let mut restarted = vec![ScheduledScopeReservation {
        run: claimed,
        scope: WorkspaceScope::Temporary,
        identity: Some(identity),
        resolution: ScopeReservationResolution::Refresh,
    }];
    actor.reserve_restart_claims(&mut restarted);

    let (reply, answer) = mpsc::sync_channel(1);
    actor.handle(Command::Create {
        request: create(claimed),
        reply,
    });
    let Response::Failure(failure) = answer.recv().unwrap() else {
        panic!("the claimed schedule must answer before a person can take its scope")
    };
    assert_eq!(failure.code, "schedule_scope_reserved");
    assert!(failure.retryable);
    assert!(restarted.is_empty());
    assert!(actor.deferred_starts.is_empty());
}

#[test]
fn reconcile_releases_only_a_successful_restart_reservation() {
    let (mut actor, _commands, _inbox) = actor();
    let claimed = RunId::mint();

    actor.record_schedule_reconcile(claimed, WorkspaceScope::Temporary, None, false);
    assert!(
        actor.scope_is_reserved(claimed, &WorkspaceScope::Temporary),
        "a retryable reconcile failure creates the reservation even when restart did not"
    );
    actor.record_schedule_reconcile(claimed, WorkspaceScope::Temporary, None, true);
    assert!(!actor.scope_is_reserved(claimed, &WorkspaceScope::Temporary));
}

#[test]
fn a_transient_restart_drain_failure_is_retried_then_removed() {
    let (mut actor, _commands, _inbox) = actor();
    let run = RunId::mint();
    actor.defer_created_start(run);
    let mut attempts = 0;

    actor.drain_deferred_restart_starts_with(|_, candidate| {
        assert_eq!(candidate, run);
        attempts += 1;
        failed_created_start_disposition(true, Some(&RunStatus::Created))
    });

    assert_eq!(attempts, 1);
    assert_eq!(actor.deferred_restart_starts, VecDeque::from([run]));
    assert!(
        actor.deferred_restart_retry_at.is_some(),
        "only a transient restart-drain failure arms the retry deadline"
    );
    assert!(actor.deferred_restart_retry_unhealthy);

    actor.drain_deferred_restart_starts_with(|_, candidate| {
        assert_eq!(candidate, run);
        attempts += 1;
        DeferredStartDisposition::Complete
    });

    assert_eq!(attempts, 2);
    assert!(actor.deferred_restart_starts.is_empty());
    assert_eq!(actor.deferred_restart_retry_at, None);
    assert!(!actor.deferred_restart_retry_unhealthy);

    assert_eq!(
        failed_created_start_disposition(false, Some(&RunStatus::Created)),
        DeferredStartDisposition::Complete,
        "an ordinary exact Create remains caller-driven"
    );
    assert_eq!(
        failed_created_start_disposition(true, Some(&RunStatus::Completed)),
        DeferredStartDisposition::Complete,
        "a failure that already changed durable status is not replayed"
    );
    assert_eq!(
        failed_created_start_disposition(true, None),
        DeferredStartDisposition::Retryable,
        "a transient status read cannot discard a run proven Created at drain entry"
    );
}

#[test]
fn unresolved_restart_scope_blocks_only_its_authored_project() {
    let (mut actor, _commands, _inbox) = actor();
    let claimed = RunId::mint();
    let project = koru_delegate_core::ProjectId::mint();
    actor
        .claimed_schedule_retries
        .push(ScheduledScopeReservation {
            run: claimed,
            scope: WorkspaceScope::ProjectRoot { project },
            // A restart snapshot may have resolved the old root. Refresh
            // mode must ignore it when the project can no longer resolve.
            identity: Some(ScopeIdentity::of_directory(std::path::Path::new(".")).unwrap()),
            resolution: ScopeReservationResolution::Refresh,
        });

    assert!(actor.scope_is_reserved(RunId::mint(), &WorkspaceScope::ProjectRoot { project }));
    assert!(!actor.scope_is_reserved(RunId::mint(), &WorkspaceScope::Temporary));
    assert!(!actor.scope_is_reserved(
        RunId::mint(),
        &WorkspaceScope::ProjectRoot {
            project: koru_delegate_core::ProjectId::mint(),
        }
    ));
}

#[test]
fn unresolved_claimed_child_blocks_a_later_schedule_for_the_same_project() {
    let (mut actor, _commands, _inbox) = actor();
    let claimed = RunId::mint();
    let project = koru_delegate_core::ProjectId::mint();
    actor
        .claimed_schedule_retries
        .push(ScheduledScopeReservation {
            run: claimed,
            scope: WorkspaceScope::ProjectFolder {
                project,
                folder: koru_delegate_core::RelativeFolder::new("missing-child").unwrap(),
            },
            identity: None,
            resolution: ScopeReservationResolution::Refresh,
        });
    let (reply, answer) = mpsc::sync_channel(1);
    actor.handle(Command::ScheduleBegin {
        run: RunId::mint(),
        scope: ScopeIdentity::of_directory(std::path::Path::new(".")).unwrap(),
        authored_scope: WorkspaceScope::ProjectRoot { project },
        reply,
    });

    assert_eq!(answer.recv().unwrap(), Ok(ScheduleAdmission::Busy(claimed)));
}

#[test]
fn resolved_restart_scopes_overlap_across_project_names() {
    let directory = ScopeIdentity::of_directory(std::path::Path::new(".")).unwrap();
    let reserved = ScheduledScopeReservation {
        run: RunId::mint(),
        scope: WorkspaceScope::ProjectRoot {
            project: koru_delegate_core::ProjectId::mint(),
        },
        identity: Some(directory.clone()),
        resolution: ScopeReservationResolution::Exact,
    };
    let candidate = WorkspaceScope::ProjectRoot {
        project: koru_delegate_core::ProjectId::mint(),
    };
    assert!(reservation_overlaps(
        &candidate,
        Some(&directory),
        Some(&directory),
        &reserved
    ));

    let temporary = ScheduledScopeReservation {
        run: RunId::mint(),
        scope: WorkspaceScope::Temporary,
        identity: Some(directory.clone()),
        resolution: ScopeReservationResolution::Exact,
    };
    assert!(reservation_overlaps(
        &candidate,
        Some(&directory),
        Some(&directory),
        &temporary
    ));
}
