// SPDX-License-Identifier: MPL-2.0

use std::sync::mpsc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::*;
use koru_delegate_core::ModelName;
use koru_delegate_document::{ScheduleClaimPlan, ScheduleDocument};
use koru_delegate_schedule::{
    ScheduleFiring, ScheduleFiringOutcome, ScheduleLifecycle, ScheduleName, SchedulePauseReason,
    ScheduleState,
};
use koru_delegate_scope::ScopeIdentity;
use koru_delegate_sqlite::SlotOutcome;

mod routine;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScheduleEventCall {
    ExistingCreated,
    PrepareCreated(EventId),
    PrepareDefinition(EventId, ScheduleDefinitionEvent),
    PrepareOccurrence(EventId, ScheduleFiringOutcome, i64),
    Reconcile(Option<EventId>, i64),
    Ready(EventId),
    Abandon(EventId),
}

#[derive(Default)]
struct ScheduleEventScript {
    calls: Vec<ScheduleEventCall>,
    definitions: Vec<koru_delegate_schedule::ScheduleDefinition>,
    fail_definition_prepare: bool,
    fail_occurrence_reconcile: bool,
}

struct RecordingScheduleEvents {
    script: Arc<Mutex<ScheduleEventScript>>,
}

impl RecordingScheduleEvents {
    fn prepared(&self, call: impl FnOnce(EventId) -> ScheduleEventCall) -> PreparedScheduleEvent {
        let event = EventId::mint();
        self.script.lock().unwrap().calls.push(call(event));
        PreparedScheduleEvent { event }
    }
}

impl ScheduleEvents for RecordingScheduleEvents {
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
        self.script
            .lock()
            .unwrap()
            .calls
            .push(ScheduleEventCall::ExistingCreated);
        Ok(())
    }

    fn prepare_created(
        &mut self,
        _schedule: ScheduleEventRef,
        _header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<PreparedScheduleEvent> {
        Ok(self.prepared(ScheduleEventCall::PrepareCreated))
    }

    fn prepare_definition(
        &mut self,
        _schedule: ScheduleEventRef,
        definition: &koru_delegate_schedule::ScheduleDefinition,
        kind: ScheduleDefinitionEvent,
    ) -> PortResult<PreparedScheduleEvent> {
        let mut script = self.script.lock().unwrap();
        if script.fail_definition_prepare {
            script.fail_definition_prepare = false;
            return Err(PortError::new(
                Port::Events,
                "injected_prepare",
                "schedule event preparation failed",
            ));
        }
        let event = EventId::mint();
        script.definitions.push(definition.clone());
        script
            .calls
            .push(ScheduleEventCall::PrepareDefinition(event, kind));
        Ok(PreparedScheduleEvent { event })
    }

    fn prepare_occurrence(
        &mut self,
        fact: ScheduleOccurrenceEvent,
    ) -> PortResult<Option<PreparedScheduleEvent>> {
        Ok(Some(self.prepared(|event| {
            ScheduleEventCall::PrepareOccurrence(event, fact.outcome, fact.claimed_at_ms)
        })))
    }

    fn reconcile_occurrence(
        &mut self,
        _schedule: ScheduleEventRef,
        prepared: Option<PreparedScheduleEvent>,
        claim: &koru_delegate_sqlite::SlotClaim,
    ) -> PortResult<Option<PreparedScheduleEvent>> {
        let mut script = self.script.lock().unwrap();
        script.calls.push(ScheduleEventCall::Reconcile(
            prepared.map(|prepared| prepared.event),
            claim.claimed_at_ms,
        ));
        if script.fail_occurrence_reconcile {
            script.fail_occurrence_reconcile = false;
            return Err(PortError::new(
                Port::Events,
                "injected_reconcile",
                "schedule event reconciliation failed",
            ));
        }
        Ok(prepared)
    }

    fn ready(&mut self, prepared: PreparedScheduleEvent) -> PortResult<()> {
        self.script
            .lock()
            .unwrap()
            .calls
            .push(ScheduleEventCall::Ready(prepared.event));
        Ok(())
    }

    fn abandon(&mut self, prepared: PreparedScheduleEvent) -> PortResult<()> {
        self.script
            .lock()
            .unwrap()
            .calls
            .push(ScheduleEventCall::Abandon(prepared.event));
        Ok(())
    }
}

fn recording_schedule_events() -> (Box<dyn ScheduleEvents>, Arc<Mutex<ScheduleEventScript>>) {
    let script = Arc::new(Mutex::new(ScheduleEventScript::default()));
    (
        Box::new(RecordingScheduleEvents {
            script: Arc::clone(&script),
        }),
        script,
    )
}

struct TestScheduleRoots {
    project: std::path::PathBuf,
    temporary: std::path::PathBuf,
}

impl ScopeRoots for TestScheduleRoots {
    fn project_root(&self, project: koru_delegate_core::ProjectId) -> RootLookup {
        if project == super::project(1) {
            RootLookup::Directory(self.project.clone())
        } else {
            RootLookup::UnknownProject
        }
    }

    fn temporary_directory(&self, run: RunId) -> Option<std::path::PathBuf> {
        let directory = self.temporary.join(run.to_storage()).join("workspace");
        std::fs::create_dir_all(&directory).ok()?;
        Some(directory)
    }
}

type PendingScheduleEdit = (Arc<Mutex<Vec<u8>>>, DeviceId, i64);

struct TestSchedulePreparation {
    sessions: Arc<Mutex<SessionScript>>,
    capture: Arc<Mutex<CaptureScript>>,
    roots: TestScheduleRoots,
    edit_during_pin: Option<PendingScheduleEdit>,
    clock_during_pin: Option<(Arc<std::sync::atomic::AtomicI64>, i64)>,
}

impl SchedulePreparation for TestSchedulePreparation {
    fn profile_delegatable(&mut self, _profile: &SessionProfileKey) -> PortResult<bool> {
        Ok(self.sessions.lock().unwrap().profile_delegatable)
    }

    fn resolve_scope(
        &mut self,
        run: RunId,
        scope: &WorkspaceScope,
    ) -> Result<koru_delegate_scope::ResolvedScope, koru_delegate_scope::ScopeRefusal> {
        koru_delegate_scope::ResolvedScope::resolve(scope, run, &self.roots)
    }

    fn resolve_and_pin(
        &mut self,
        run: RunId,
        pointers: &[koru_delegate_schedule::ScheduleContextPointer],
        workspace: WorkspaceId,
    ) -> Result<Vec<RunContextItem>, CaptureFailure> {
        let pins = PinningCapture {
            script: Arc::clone(&self.capture),
        }
        .resolve_and_pin(run, pointers, workspace)?;
        if let Some((snapshot, owner, at_ms)) = self.edit_during_pin.take() {
            let document = ScheduleDocument::from_snapshot(&snapshot.lock().unwrap())
                .expect("the test schedule snapshot must open");
            let mut definition = document.header().unwrap().definition;
            definition.revision += 1;
            definition.effective_at_ms = at_ms;
            definition.lifecycle = koru_delegate_schedule::ScheduleLifecycle::Paused {
                reason: koru_delegate_schedule::SchedulePauseReason::Manual,
            };
            document.update_definition(owner, &definition).unwrap();
            *snapshot.lock().unwrap() = document.export_snapshot().unwrap();
        }
        if let Some((clock, at_ms)) = self.clock_during_pin.take() {
            clock.store(at_ms, Ordering::Release);
        }
        Ok(pins)
    }

    fn discard_candidate(&mut self, run: RunId, scope: &WorkspaceScope) -> PortResult<()> {
        if scope.is_koru_owned() {
            let directory = self
                .roots
                .temporary
                .join(run.to_storage())
                .join("workspace");
            let removal = koru_delegate_scope::remove_working_folder(scope, Some(&directory));
            if removal.is_refusal() {
                return Err(PortError::new(
                    Port::Scope,
                    "candidate_cleanup_refused",
                    "candidate scope could not be removed",
                ));
            }
        }
        Ok(())
    }
}

fn coordinator(
    schedules: MemorySchedules,
    harness: &Harness,
    owner: DeviceId,
) -> ScheduleCoordinator {
    ScheduleCoordinator::new(
        Box::new(schedules),
        Box::new(harness.schedule_store.clone()),
        Box::new(TestSchedulePreparation {
            sessions: Arc::clone(&harness.sessions),
            capture: Arc::clone(&harness.capture),
            roots: TestScheduleRoots {
                project: harness.dir.root.join("project"),
                temporary: harness.dir.root.join("temporary"),
            },
            edit_during_pin: None,
            clock_during_pin: None,
        }),
        Box::new(EvaluationNowClock),
        owner,
    )
}

#[derive(Clone)]
struct MemorySchedules {
    workspace: WorkspaceId,
    document: DocumentId,
    snapshot: Arc<Mutex<Vec<u8>>>,
}

impl MemorySchedules {
    fn one(header: &koru_delegate_schedule::ScheduleHeader) -> Self {
        Self {
            workspace: workspace(),
            document: DocumentId::from_uuid(v4(0xda)),
            snapshot: Arc::new(Mutex::new(
                ScheduleDocument::create(header)
                    .unwrap()
                    .export_snapshot()
                    .unwrap(),
            )),
        }
    }

    fn schedule(&self) -> ScheduleDocument {
        ScheduleDocument::from_snapshot(&self.snapshot.lock().unwrap()).unwrap()
    }
}

struct MemoryScheduleHandle {
    workspace: WorkspaceId,
    document: ScheduleDocument,
    snapshot: Arc<Mutex<Vec<u8>>>,
}

impl ScheduleDocumentHandle for MemoryScheduleHandle {
    fn expired_routine_authority(
        &self,
        device: DeviceId,
        plan: koru_delegate_document::RoutineExpiryPlan,
    ) -> PortResult<Option<koru_delegate_document::RoutineExpiryAuthority>> {
        self.document
            .expired_routine_authority(self.workspace, device, plan)
            .map_err(Into::into)
    }
    fn document(&self) -> &ScheduleDocument {
        &self.document
    }

    fn claim_authority(
        &self,
        device: DeviceId,
        plan: ScheduleClaimPlan,
    ) -> PortResult<Option<koru_delegate_document::ScheduleClaimAuthority>> {
        self.document
            .claim_authority(self.workspace, device, plan)
            .map_err(Into::into)
    }

    fn commit(
        &mut self,
        write: &mut dyn FnMut(
            &ScheduleDocument,
        ) -> Result<(), koru_delegate_document::ScheduleDocumentError>,
    ) -> PortResult<()> {
        write(&self.document)?;
        *self.snapshot.lock().unwrap() = self.document.export_snapshot()?;
        Ok(())
    }

    fn absorb(&mut self) -> PortResult<bool> {
        let before = self.document.version();
        self.document.import(&self.snapshot.lock().unwrap())?;
        Ok(self.document.version() != before)
    }
}

struct NoScheduleWatch;

impl ScheduleChangeWatch for NoScheduleWatch {
    fn close(self: Box<Self>) {}
}

impl ScheduleDocuments for MemorySchedules {
    fn create(
        &mut self,
        workspace: WorkspaceId,
        header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<DocumentId> {
        self.workspace = workspace;
        *self.snapshot.lock().unwrap() = ScheduleDocument::create(header)?.export_snapshot()?;
        Ok(self.document)
    }

    fn open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        if workspace != self.workspace || document != self.document {
            return Err(PortError::new(Port::Schedules, "not_found", "schedule"));
        }
        Ok(Box::new(MemoryScheduleHandle {
            workspace,
            document: self.schedule(),
            snapshot: Arc::clone(&self.snapshot),
        }))
    }

    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        let header = self.schedule().header()?;
        Ok(ScheduleCatalog {
            workspaces: vec![self.workspace],
            schedules: vec![ListedSchedule {
                workspace: self.workspace,
                document: self.document,
                header,
                trashed_at_ms: None,
            }],
            failed_workspaces: Vec::new(),
            unavailable_documents: Vec::new(),
            failures: 0,
        })
    }

    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        self.discover_active()
    }

    fn watch_changes(
        &mut self,
        _targets: &ScheduleWatchTargets,
        _wake: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        Ok(Box::new(NoScheduleWatch))
    }
}

struct FaultThenHealthySchedules {
    bad: ListedSchedule,
    healthy: MemorySchedules,
}

struct RowFailureSchedules {
    bad: ListedSchedule,
    unavailable: DocumentId,
    healthy: MemorySchedules,
}

impl ScheduleDocuments for RowFailureSchedules {
    fn create(
        &mut self,
        workspace: WorkspaceId,
        header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<DocumentId> {
        self.healthy.create(workspace, header)
    }

    fn open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        if workspace == self.bad.workspace && document == self.bad.document {
            return Err(PortError::new(
                Port::Schedules,
                "bad_schedule",
                "schedule is unavailable",
            ));
        }
        self.healthy.open(workspace, document)
    }

    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        let mut catalog = self.healthy.discover_active()?;
        catalog.workspaces.push(self.bad.workspace);
        catalog.schedules.push(self.bad.clone());
        catalog
            .unavailable_documents
            .push((self.bad.workspace, self.unavailable));
        catalog.failures = 2;
        Ok(catalog)
    }

    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        self.discover_active()
    }

    fn watch_changes(
        &mut self,
        targets: &ScheduleWatchTargets,
        wake: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        self.healthy.watch_changes(targets, wake)
    }
}

struct DeletedWorkspaceSchedules {
    retained: MemorySchedules,
    target: WorkspaceId,
    restored: Arc<AtomicBool>,
    creates: Arc<AtomicUsize>,
}

impl ScheduleDocuments for DeletedWorkspaceSchedules {
    fn create(
        &mut self,
        _workspace: WorkspaceId,
        _header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<DocumentId> {
        self.creates.fetch_add(1, Ordering::AcqRel);
        Err(PortError::new(
            Port::Schedules,
            "unexpected_create",
            "create",
        ))
    }

    fn open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        self.retained.open(workspace, document)
    }

    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        let mut catalog = self.retained.discover_active()?;
        catalog.workspaces.push(self.target);
        if !self.restored.load(Ordering::Acquire) {
            catalog
                .workspaces
                .retain(|candidate| *candidate != self.retained.workspace);
            catalog.schedules.clear();
        }
        Ok(catalog)
    }

    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        let mut catalog = self.retained.discover_active()?;
        catalog.workspaces.push(self.target);
        Ok(catalog)
    }

    fn watch_changes(
        &mut self,
        targets: &ScheduleWatchTargets,
        wake: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        self.retained.watch_changes(targets, wake)
    }
}

struct FailSecondAbsorbSchedules {
    inner: MemorySchedules,
    absorbs: Arc<AtomicUsize>,
}

struct FailFirstCommitSchedules {
    inner: MemorySchedules,
    commits: Arc<AtomicUsize>,
}

struct FailFirstCommitHandle {
    inner: MemoryScheduleHandle,
    commits: Arc<AtomicUsize>,
}

impl ScheduleDocumentHandle for FailFirstCommitHandle {
    fn expired_routine_authority(
        &self,
        device: DeviceId,
        plan: koru_delegate_document::RoutineExpiryPlan,
    ) -> PortResult<Option<koru_delegate_document::RoutineExpiryAuthority>> {
        self.inner.expired_routine_authority(device, plan)
    }
    fn document(&self) -> &ScheduleDocument {
        self.inner.document()
    }

    fn claim_authority(
        &self,
        device: DeviceId,
        plan: ScheduleClaimPlan,
    ) -> PortResult<Option<koru_delegate_document::ScheduleClaimAuthority>> {
        self.inner.claim_authority(device, plan)
    }

    fn commit(
        &mut self,
        write: &mut dyn FnMut(
            &ScheduleDocument,
        ) -> Result<(), koru_delegate_document::ScheduleDocumentError>,
    ) -> PortResult<()> {
        if self.commits.fetch_add(1, Ordering::AcqRel) == 0 {
            return Err(PortError::new(
                Port::Schedules,
                "commit_failed",
                "schedule commit unavailable",
            ));
        }
        self.inner.commit(write)
    }

    fn absorb(&mut self) -> PortResult<bool> {
        self.inner.absorb()
    }
}

impl ScheduleDocuments for FailFirstCommitSchedules {
    fn create(
        &mut self,
        workspace: WorkspaceId,
        header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<DocumentId> {
        self.inner.create(workspace, header)
    }

    fn open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        if workspace != self.inner.workspace || document != self.inner.document {
            return Err(PortError::new(Port::Schedules, "not_found", "schedule"));
        }
        Ok(Box::new(FailFirstCommitHandle {
            inner: MemoryScheduleHandle {
                workspace,
                document: self.inner.schedule(),
                snapshot: Arc::clone(&self.inner.snapshot),
            },
            commits: Arc::clone(&self.commits),
        }))
    }

    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        self.inner.discover_active()
    }

    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        self.inner.discover_identities()
    }

    fn watch_changes(
        &mut self,
        targets: &ScheduleWatchTargets,
        wake: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        self.inner.watch_changes(targets, wake)
    }
}

struct FailSecondAbsorbHandle {
    inner: MemoryScheduleHandle,
    absorbs: Arc<AtomicUsize>,
}

impl ScheduleDocumentHandle for FailSecondAbsorbHandle {
    fn expired_routine_authority(
        &self,
        device: DeviceId,
        plan: koru_delegate_document::RoutineExpiryPlan,
    ) -> PortResult<Option<koru_delegate_document::RoutineExpiryAuthority>> {
        self.inner.expired_routine_authority(device, plan)
    }
    fn document(&self) -> &ScheduleDocument {
        self.inner.document()
    }

    fn claim_authority(
        &self,
        device: DeviceId,
        plan: ScheduleClaimPlan,
    ) -> PortResult<Option<koru_delegate_document::ScheduleClaimAuthority>> {
        self.inner.claim_authority(device, plan)
    }

    fn commit(
        &mut self,
        write: &mut dyn FnMut(
            &ScheduleDocument,
        ) -> Result<(), koru_delegate_document::ScheduleDocumentError>,
    ) -> PortResult<()> {
        self.inner.commit(write)
    }

    fn absorb(&mut self) -> PortResult<bool> {
        if self.absorbs.fetch_add(1, Ordering::AcqRel) == 1 {
            return Err(PortError::new(
                Port::Schedules,
                "absorb_failed",
                "schedule updates unavailable",
            ));
        }
        self.inner.absorb()
    }
}

impl ScheduleDocuments for FailSecondAbsorbSchedules {
    fn create(
        &mut self,
        workspace: WorkspaceId,
        header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<DocumentId> {
        self.inner.create(workspace, header)
    }

    fn open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        if workspace != self.inner.workspace || document != self.inner.document {
            return Err(PortError::new(Port::Schedules, "not_found", "schedule"));
        }
        Ok(Box::new(FailSecondAbsorbHandle {
            inner: MemoryScheduleHandle {
                workspace,
                document: self.inner.schedule(),
                snapshot: Arc::clone(&self.inner.snapshot),
            },
            absorbs: Arc::clone(&self.absorbs),
        }))
    }

    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        self.inner.discover_active()
    }

    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        self.inner.discover_identities()
    }

    fn watch_changes(
        &mut self,
        targets: &ScheduleWatchTargets,
        wake: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        self.inner.watch_changes(targets, wake)
    }
}

struct EmptySchedules;

struct FailingCreateSchedules;

impl ScheduleDocuments for FailingCreateSchedules {
    fn create(
        &mut self,
        _workspace: WorkspaceId,
        _header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<DocumentId> {
        Err(PortError::new(
            Port::Schedules,
            "create_failed",
            "schedule create unavailable",
        ))
    }

    fn open(
        &mut self,
        _workspace: WorkspaceId,
        _document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        Err(PortError::new(Port::Schedules, "unused", "unused"))
    }

    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        Ok(ScheduleCatalog {
            workspaces: vec![workspace()],
            schedules: Vec::new(),
            failed_workspaces: Vec::new(),
            unavailable_documents: Vec::new(),
            failures: 0,
        })
    }

    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        self.discover_active()
    }

    fn watch_changes(
        &mut self,
        _targets: &ScheduleWatchTargets,
        _wake: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        Ok(Box::new(NoScheduleWatch))
    }
}

struct FailedDiscoverySchedules {
    creates: Arc<AtomicUsize>,
}

impl ScheduleDocuments for FailedDiscoverySchedules {
    fn create(
        &mut self,
        _workspace: WorkspaceId,
        _header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<DocumentId> {
        self.creates.fetch_add(1, Ordering::AcqRel);
        Err(PortError::new(
            Port::Schedules,
            "unexpected_create",
            "create",
        ))
    }

    fn open(
        &mut self,
        _workspace: WorkspaceId,
        _document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        Err(PortError::new(Port::Schedules, "unused", "unused"))
    }

    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        Err(PortError::new(
            Port::Schedules,
            "catalog_unavailable",
            "schedule catalog unavailable",
        ))
    }

    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        self.discover_active()
    }

    fn watch_changes(
        &mut self,
        _targets: &ScheduleWatchTargets,
        _wake: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        Ok(Box::new(NoScheduleWatch))
    }
}

impl ScheduleDocuments for EmptySchedules {
    fn create(
        &mut self,
        _workspace: WorkspaceId,
        _header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<DocumentId> {
        Err(PortError::new(Port::Schedules, "unused", "unused"))
    }

    fn open(
        &mut self,
        _workspace: WorkspaceId,
        _document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        Err(PortError::new(Port::Schedules, "unused", "unused"))
    }

    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        Ok(ScheduleCatalog {
            workspaces: Vec::new(),
            schedules: Vec::new(),
            failed_workspaces: Vec::new(),
            unavailable_documents: Vec::new(),
            failures: 0,
        })
    }

    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        self.discover_active()
    }

    fn watch_changes(
        &mut self,
        _targets: &ScheduleWatchTargets,
        _wake: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        Ok(Box::new(NoScheduleWatch))
    }
}

#[derive(Default)]
struct TrackingEffects {
    begun: Vec<RunId>,
    aborted: Vec<RunId>,
    finished: Vec<RunId>,
    reconciled: Vec<RunId>,
    fail_reconcile: Option<RunId>,
    admission_live: bool,
    busy: Option<RunId>,
}

impl ScheduleEffects for TrackingEffects {
    fn begin(
        &mut self,
        run: RunId,
        _scope: ScopeIdentity,
        _authored_scope: WorkspaceScope,
    ) -> PortResult<ScheduleAdmission> {
        self.begun.push(run);
        if let Some(holder) = self.busy {
            return Ok(ScheduleAdmission::Busy(holder));
        }
        self.admission_live = true;
        Ok(ScheduleAdmission::Granted)
    }

    fn finish(
        &mut self,
        run: RunId,
        _claim: koru_delegate_sqlite::SlotClaim,
        _scope: koru_delegate_scope::ResolvedScope,
        _authored_scope: WorkspaceScope,
    ) -> PortResult<Option<Started>> {
        if !self.admission_live {
            return Err(PortError::new(
                Port::Schedules,
                "admission_stale",
                "the schedule admission lease is not live",
            ));
        }
        self.admission_live = false;
        self.finished.push(run);
        Ok(None)
    }

    fn abort(&mut self, run: RunId) {
        self.admission_live = false;
        self.aborted.push(run);
    }

    fn reconcile(
        &mut self,
        claim: koru_delegate_sqlite::SlotClaim,
        _authored_scope: WorkspaceScope,
    ) -> PortResult<Option<Started>> {
        self.reconciled.push(claim.run);
        if self.fail_reconcile == Some(claim.run) {
            return Err(PortError::new(
                Port::Sessions,
                "reconcile_failed",
                "reconciliation unavailable",
            ));
        }
        Ok(None)
    }
}

impl ScheduleDocuments for FaultThenHealthySchedules {
    fn create(
        &mut self,
        workspace: WorkspaceId,
        header: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<DocumentId> {
        self.healthy.create(workspace, header)
    }

    fn open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        if workspace == self.bad.workspace && document == self.bad.document {
            return Err(PortError::new(
                Port::Schedules,
                "bad_schedule",
                "schedule is unavailable",
            ));
        }
        self.healthy.open(workspace, document)
    }

    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        let mut catalog = self.healthy.discover_active()?;
        // The manifest identified a schedule, but its snapshot could not be
        // read. Production returns this partial catalog so the healthy row
        // still evaluates and the unavailable document stays watched.
        catalog
            .unavailable_documents
            .push((self.bad.workspace, self.bad.document));
        catalog.failures = 1;
        Ok(catalog)
    }

    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        self.discover_active()
    }

    fn watch_changes(
        &mut self,
        targets: &ScheduleWatchTargets,
        wake: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        self.healthy.watch_changes(targets, wake)
    }
}

fn active_schedule(owner: DeviceId) -> koru_delegate_schedule::ScheduleHeader {
    let request = request(run_id(0xd2), WorkspaceScope::Temporary);
    koru_delegate_schedule::ScheduleHeader {
        schedule: koru_delegate_core::ScheduleId::from_uuid(v4(0xd1)),
        created_at_ms: 1,
        initial_owner: owner,
        definition: koru_delegate_schedule::ScheduleDefinition {
            routine: None,
            revision: 1,
            effective_at_ms: 1,
            template: koru_delegate_schedule::DelegationTemplate {
                requested_by: request.requested_by,
                objective: request.objective,
                agent: request.agent,
                requested_model: request.requested_model,
                scope: request.scope,
                grants: request.grants,
                context: Vec::new(),
            },
            recurrence: koru_recurrence::RecurrenceRule::floating(
                koru_recurrence::CronExpression::parse("0 * * * *").unwrap(),
            ),
            lifecycle: koru_delegate_schedule::ScheduleLifecycle::Active,
        },
    }
}

#[test]
fn schedule_wake_coalesces_and_unblocks_an_idle_receiver() {
    let before = crate::counters::schedule_test_snapshot();
    let (sender, receiver) = mpsc::channel();
    let wake = ScheduleWake::new(move || {
        let _ = sender.send(());
    });
    wake.signal();
    wake.signal();
    receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(receiver.try_recv().is_err());
    let after_coalesced = crate::counters::schedule_test_snapshot();
    assert_eq!(after_coalesced.wakes - before.wakes, 1);
    assert!(wake.take());
    assert!(!wake.take());

    wake.signal();
    receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    let after_rearmed = crate::counters::schedule_test_snapshot();
    assert_eq!(after_rearmed.wakes - after_coalesced.wakes, 1);
}

#[test]
fn watch_targets_are_exact_canonical_catalog_rooms() {
    let first = workspace();
    let second = WorkspaceId::from_uuid(v4(0xa1));
    let document = DocumentId::from_uuid(v4(0xd0));
    let owner = DeviceId::from_uuid(v4(0xd3));
    let catalog = ScheduleCatalog {
        workspaces: vec![second, first, first],
        schedules: vec![ListedSchedule {
            workspace: first,
            document,
            header: active_schedule(owner),
            trashed_at_ms: None,
        }],
        failed_workspaces: Vec::new(),
        unavailable_documents: vec![(second, DocumentId::from_uuid(v4(0xd2)))],
        failures: 1,
    };
    let targets = ScheduleWatchTargets::from_catalog(&catalog);
    assert_eq!(targets.workspaces, vec![first, second]);
    assert_eq!(
        targets.documents,
        vec![(first, document), (second, DocumentId::from_uuid(v4(0xd2)))]
    );
}

fn run_plan(occurrence_ms: i64, run: RunId) -> ScheduleClaimPlan {
    ScheduleClaimPlan::Run {
        occurrence_ms,
        run,
        pinned_context: Vec::new(),
    }
}

#[test]
fn edit_pause_and_retirement_after_claim_preserve_the_original_launch() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(owner);
    let document = ScheduleDocument::create(&header).unwrap();
    let mut db = RunsDb::open(std::path::Path::new(":memory:")).unwrap();
    let first_run = run_id(0xd4);
    let authority = document
        .claim_authority(workspace(), owner, run_plan(60_000, first_run))
        .unwrap()
        .unwrap();
    let claimed = ScheduleLedger::claim_run(&mut db, authority, 999).unwrap();
    assert_eq!(claimed.run, first_run);
    assert_eq!(claimed.revision, 1);

    let mut edited = header.definition.clone();
    edited.revision = 2;
    edited.effective_at_ms += 1;
    edited.template.requested_model = Some(ModelName::new("gpt-5.4").unwrap());
    document.update_definition(owner, &edited).unwrap();
    let retry_run = run_id(0xd5);
    let retry = document
        .claim_authority(workspace(), owner, run_plan(60_000, retry_run))
        .unwrap()
        .unwrap();
    let same = ScheduleLedger::claim_run(&mut db, retry, 999).unwrap();
    assert_eq!(same.run, first_run);
    assert_eq!(same.revision, 1);
    assert!(db.intent(retry_run).unwrap().is_none());
    let pending = db
        .pending_intents()
        .unwrap()
        .into_iter()
        .find(|pending| pending.run == first_run)
        .unwrap();
    assert_eq!(pending.payload.requested_model, None);
    assert_eq!(
        pending.payload.objective,
        header.definition.template.objective
    );

    assert_eq!(
        ScheduleLedger::settle(
            &mut db,
            header.schedule,
            60_000,
            SlotOutcome::SkippedRetired,
        ),
        Err(LedgerError::InvalidSettlementOutcome)
    );
    assert_eq!(
        db.slot(header.schedule, 60_000).unwrap().unwrap().state,
        koru_delegate_sqlite::SlotState::Claimed
    );
    ScheduleLedger::settle(&mut db, header.schedule, 60_000, SlotOutcome::Fired).unwrap();

    let paused_header = active_schedule(owner);
    let paused = ScheduleDocument::create(&paused_header).unwrap();
    let authority = paused
        .claim_authority(workspace(), owner, run_plan(120_000, run_id(0xd6)))
        .unwrap()
        .unwrap();
    ScheduleLedger::claim_run(&mut db, authority, 999).unwrap();
    let mut pause = paused_header.definition.clone();
    pause.revision = 2;
    pause.effective_at_ms += 1;
    pause.lifecycle = koru_delegate_schedule::ScheduleLifecycle::Paused {
        reason: koru_delegate_schedule::SchedulePauseReason::Manual,
    };
    paused.update_definition(owner, &pause).unwrap();
    assert!(
        paused
            .claim_authority(workspace(), owner, run_plan(180_000, run_id(0xd7)))
            .unwrap()
            .is_none()
    );
    ScheduleLedger::settle(&mut db, paused_header.schedule, 120_000, SlotOutcome::Fired).unwrap();

    let retired_header = active_schedule(owner);
    let retired = ScheduleDocument::create(&retired_header).unwrap();
    let authority = retired
        .claim_authority(workspace(), owner, run_plan(240_000, run_id(0xd8)))
        .unwrap()
        .unwrap();
    ScheduleLedger::claim_run(&mut db, authority, 999).unwrap();
    retired.retire(owner, 300_000).unwrap();
    assert!(
        retired
            .claim_authority(workspace(), owner, run_plan(300_000, run_id(0xd9)))
            .unwrap()
            .is_none()
    );
    ScheduleLedger::settle(
        &mut db,
        retired_header.schedule,
        240_000,
        SlotOutcome::Fired,
    )
    .unwrap();
}

/// UC9's storage boundary: a schedule keeps a live pointer, each occurrence
/// resolves it again, and the first run's immutable pin is not revised when
/// the source changes before the second firing.
#[test]
fn schedule_context_is_resolved_per_firing_and_failure_claims_nothing() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let mut header = active_schedule(owner);
    header.definition.template.requested_model = Some(ModelName::new("gpt-5.4").unwrap());
    let source_document = DocumentId::from_uuid(v4(0xc0));
    header.definition.template.context =
        vec![koru_delegate_schedule::ScheduleContextPointer::Document {
            workspace: workspace(),
            document: source_document,
        }];
    let document = ScheduleDocument::create(&header).unwrap();
    let mut db = RunsDb::open(std::path::Path::new(":memory:")).unwrap();
    let mut harness = Harness::new("schedule-context-per-firing");

    let first_run = run_id(0xe1);
    let first_pins = harness
        .orchestrator
        .ports_mut()
        .capture
        .resolve_and_pin(first_run, &header.definition.template.context, workspace())
        .unwrap();
    let first_version = match &first_pins[0] {
        RunContextItem::Document { version, .. } => *version,
        _ => panic!("the document pointer must become a document pin"),
    };
    let authority = document
        .claim_authority(
            workspace(),
            owner,
            ScheduleClaimPlan::Run {
                occurrence_ms: 60_000,
                run: first_run,
                pinned_context: first_pins,
            },
        )
        .unwrap()
        .unwrap();
    ScheduleLedger::claim_run(&mut db, authority, 999).unwrap();

    // The fake returns a new captured version on each resolution, standing
    // for the source edit between today's and tomorrow's occurrence.
    let second_run = run_id(0xe2);
    let second_pins = harness
        .orchestrator
        .ports_mut()
        .capture
        .resolve_and_pin(second_run, &header.definition.template.context, workspace())
        .unwrap();
    let second_version = match &second_pins[0] {
        RunContextItem::Document { version, .. } => *version,
        _ => panic!("the document pointer must become a document pin"),
    };
    assert_ne!(first_version, second_version);
    let authority = document
        .claim_authority(
            workspace(),
            owner,
            ScheduleClaimPlan::Run {
                occurrence_ms: 120_000,
                run: second_run,
                pinned_context: second_pins,
            },
        )
        .unwrap()
        .unwrap();
    ScheduleLedger::claim_run(&mut db, authority, 999).unwrap();

    let pending = db.pending_intents().unwrap();
    let first = pending
        .iter()
        .find(|intent| intent.run == first_run)
        .unwrap()
        .payload
        .clone();
    let second = pending
        .iter()
        .find(|intent| intent.run == second_run)
        .unwrap()
        .payload
        .clone();
    assert!(
        matches!(first.context[0], RunContextItem::Document { version, .. } if version == first_version)
    );
    assert!(
        matches!(second.context[0], RunContextItem::Document { version, .. } if version == second_version)
    );
    assert_eq!(
        first.requested_model,
        header.definition.template.requested_model
    );
    assert_eq!(
        second.requested_model,
        header.definition.template.requested_model
    );

    let failed_run = run_id(0xe3);
    harness.capture.lock().unwrap().fail_resolve = Some(CaptureFailure {
        item: 0,
        stage: CaptureStage::Resolve,
    });
    assert_eq!(
        harness.orchestrator.ports_mut().capture.resolve_and_pin(
            failed_run,
            &header.definition.template.context,
            workspace(),
        ),
        Err(CaptureFailure {
            item: 0,
            stage: CaptureStage::Resolve,
        })
    );
    assert!(db.intent(failed_run).unwrap().is_none());
    assert!(db.slot(header.schedule, 180_000).unwrap().is_none());
}

fn hourly_schedule(
    owner: DeviceId,
    effective_at_ms: i64,
    scope: WorkspaceScope,
) -> koru_delegate_schedule::ScheduleHeader {
    let mut header = active_schedule(owner);
    header.created_at_ms = effective_at_ms;
    header.definition.effective_at_ms = effective_at_ms;
    header.definition.template.scope = scope;
    header
}

/// UC8 and D34 through the effects coordinator: one occurrence runs, the
/// next is atomically skipped with the holding RunId, and once that attempt
/// ends the following occurrence runs. Repeating the same evaluation is
/// inert and starts no duplicate provider.
#[test]
fn coordinator_runs_skips_busy_then_runs_without_duplicate_starts() {
    let counters_before = crate::counters::schedule_test_snapshot();
    let mut harness = Harness::new("schedule-run-skip-run");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let mut header = hourly_schedule(owner, base, harness.scope());
    header.definition.template.requested_model = Some(ModelName::new("gpt-5.4").unwrap());
    header.definition.template.context =
        vec![koru_delegate_schedule::ScheduleContextPointer::Document {
            workspace: workspace(),
            document: DocumentId::from_uuid(v4(0xef)),
        }];
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let mut coordinator = coordinator(schedules, &harness, owner);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let first = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    let counters_after_first = crate::counters::schedule_test_snapshot();
    assert_eq!(first.started.len(), 1);
    assert_eq!(
        counters_after_first.evaluations - counters_before.evaluations,
        1
    );
    assert_eq!(
        counters_after_first.documents_evaluated - counters_before.documents_evaluated,
        1
    );
    assert_eq!(counters_after_first.fired - counters_before.fired, 1);
    let first_run = first.started[0].run;
    let first_header = harness.orchestrator.header(first_run).unwrap();

    let duplicate = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    assert!(duplicate.started.is_empty());
    let counters_after_duplicate = crate::counters::schedule_test_snapshot();
    assert_eq!(
        counters_after_duplicate.fired, counters_after_first.fired,
        "re-reading settled history is not a newly applied outcome"
    );

    let busy = coordinator
        .evaluate(&mut harness.orchestrator, base + 3_600_001, &zone)
        .unwrap();
    assert!(busy.started.is_empty());
    let counters_after_busy = crate::counters::schedule_test_snapshot();
    assert_eq!(
        counters_after_busy.skipped_busy - counters_after_duplicate.skipped_busy,
        1
    );
    let firings = inspect.schedule().firings().unwrap();
    assert_eq!(firings.len(), 2);
    assert_eq!(
        firings[0].outcome,
        koru_delegate_schedule::ScheduleFiringOutcome::Fired
    );
    assert_eq!(
        firings[1].outcome,
        koru_delegate_schedule::ScheduleFiringOutcome::SkippedBusy
    );
    assert_eq!(firings[1].busy_holder, Some(first_run));

    harness.agent(
        first_run,
        koru_session_protocol::AgentEvent::TurnCompleted { turn_id: turn(1) },
    );
    harness.session_event(
        first_run,
        koru_session_protocol::SessionEvent::Exited { code: None },
    );
    let third = coordinator
        .evaluate(&mut harness.orchestrator, base + 7_200_001, &zone)
        .unwrap();
    assert_eq!(
        third.started.len(),
        1,
        "claims={} firings={:?} calls={:?}",
        harness.orchestrator.claims().len(),
        inspect.schedule().firings().unwrap(),
        harness.calls()
    );
    assert_ne!(third.started[0].run, first_run);
    let third_header = harness.orchestrator.header(third.started[0].run).unwrap();
    assert_eq!(
        first_header.requested_model,
        header.definition.template.requested_model
    );
    assert_eq!(
        third_header.requested_model,
        header.definition.template.requested_model
    );
    assert_ne!(first_header.context, third_header.context);
    assert_eq!(
        harness.orchestrator.header(first_run).unwrap().context,
        first_header.context,
        "a later occurrence never revises the earlier run's pin"
    );
    assert_eq!(
        harness
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::Create { .. }))
            .count(),
        2
    );
    assert_eq!(
        inspect
            .schedule()
            .firings()
            .unwrap()
            .iter()
            .map(|firing| firing.outcome)
            .collect::<Vec<_>>(),
        vec![
            koru_delegate_schedule::ScheduleFiringOutcome::Fired,
            koru_delegate_schedule::ScheduleFiringOutcome::SkippedBusy,
            koru_delegate_schedule::ScheduleFiringOutcome::Fired,
        ]
    );
}

/// The zero-catch-up user path: run at the first slot, wake after the next
/// slot is irretrievably late, record no run for it, then run at the following
/// due slot.
#[test]
fn coordinator_runs_skips_a_slept_through_slot_then_runs() {
    let mut harness = Harness::new("schedule-run-missed-run");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let header = hourly_schedule(owner, base, harness.scope());
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let mut coordinator = coordinator(schedules, &harness, owner);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let first = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    assert_eq!(first.started.len(), 1);
    let first_run = first.started[0].run;
    harness.agent(
        first_run,
        koru_session_protocol::AgentEvent::TurnCompleted { turn_id: turn(1) },
    );
    harness.session_event(
        first_run,
        koru_session_protocol::SessionEvent::Exited { code: None },
    );

    let after_sleep_at = base + 3_600_000 + 20 * 60_000;
    harness
        .schedule_clock
        .store(after_sleep_at, Ordering::Release);
    let counters_before_miss = crate::counters::schedule_test_snapshot();
    let after_sleep = coordinator
        .evaluate(&mut harness.orchestrator, after_sleep_at, &zone)
        .unwrap();
    let counters_after_miss = crate::counters::schedule_test_snapshot();
    assert!(after_sleep.started.is_empty());
    assert_eq!(
        counters_after_miss.skipped_missed - counters_before_miss.skipped_missed,
        1
    );
    assert_eq!(
        harness
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::Create { .. }))
            .count(),
        1,
        "a missed slot never creates a provider session"
    );

    let third = coordinator
        .evaluate(&mut harness.orchestrator, base + 7_200_001, &zone)
        .unwrap();
    assert_eq!(third.started.len(), 1);
    assert_eq!(
        inspect
            .schedule()
            .firings()
            .unwrap()
            .iter()
            .map(|firing| (firing.occurrence_ms, firing.outcome))
            .collect::<Vec<_>>(),
        vec![
            (base, koru_delegate_schedule::ScheduleFiringOutcome::Fired,),
            (
                base + 3_600_000,
                koru_delegate_schedule::ScheduleFiringOutcome::SkippedMissed,
            ),
            (
                base + 7_200_000,
                koru_delegate_schedule::ScheduleFiringOutcome::Fired,
            ),
        ]
    );
}

#[test]
fn fired_event_keeps_one_prepared_id_and_the_claims_exact_time() {
    let mut harness = Harness::new("schedule-fired-event-order");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let header = hourly_schedule(owner, base, harness.scope());
    let schedules = MemorySchedules::one(&header);
    let (events, script) = recording_schedule_events();
    let mut coordinator = coordinator(schedules, &harness, owner).with_events(events);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let report = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();

    assert_eq!(report.started.len(), 1);
    let calls = script.lock().unwrap().calls.clone();
    assert_eq!(calls[0], ScheduleEventCall::ExistingCreated);
    let prepared = match calls[1] {
        ScheduleEventCall::PrepareOccurrence(event, ScheduleFiringOutcome::Fired, at_ms) => {
            assert_eq!(at_ms, base + 1);
            event
        }
        other => panic!("unexpected event call: {other:?}"),
    };
    assert_eq!(
        calls[2],
        ScheduleEventCall::Reconcile(Some(prepared), base + 1)
    );
    assert_eq!(calls[3], ScheduleEventCall::Ready(prepared));
    let claim = harness
        .schedule_store
        .claims_of(header.schedule)
        .unwrap()
        .remove(0);
    assert_eq!(claim.claimed_at_ms, base + 1);
}

#[test]
fn candidate_claim_caches_its_admission_before_event_reconciliation_can_fail() {
    let harness = Harness::new("schedule-event-reconcile-candidate-lease");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let schedules = MemorySchedules::one(&hourly_schedule(owner, base, harness.scope()));
    let (events, script) = recording_schedule_events();
    script.lock().unwrap().fail_occurrence_reconcile = true;
    let mut coordinator = coordinator(schedules, &harness, owner).with_events(events);
    let mut effects = TrackingEffects::default();
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let report = coordinator.evaluate(&mut effects, base + 1, &zone).unwrap();

    assert_eq!(report.failures, 1);
    let claim = harness.schedule_store.unsettled().unwrap().remove(0);
    assert_eq!(effects.begun, vec![claim.run]);
    assert!(effects.aborted.is_empty());
    assert!(effects.admission_live);
    assert!(
        coordinator.has_claimed_scope(claim.run),
        "the live lease is owned by restart state before the event error returns"
    );
}

#[test]
fn prior_claim_winner_aborts_the_candidate_before_event_reconciliation_can_fail() {
    let mut harness = Harness::new("schedule-event-reconcile-prior-winner-lease");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let header = hourly_schedule(owner, base, harness.scope());
    let schedules = MemorySchedules::one(&header);
    let workspace_id = schedules.workspace;
    let document = schedules.document;
    let mut handle = schedules.clone().open(workspace_id, document).unwrap();
    let prior_run = run_id(0xb7);
    let authority = ScheduleDocument::create(&header)
        .unwrap()
        .claim_authority(workspace(), owner, run_plan(base, prior_run))
        .unwrap()
        .unwrap();
    harness.schedule_store.claim_run(authority, base).unwrap();
    let (events, script) = recording_schedule_events();
    script.lock().unwrap().fail_occurrence_reconcile = true;
    let mut coordinator = coordinator(schedules, &harness, owner).with_events(events);
    let mut effects = TrackingEffects::default();
    let mut report = ScheduleEvaluation::default();

    let result = coordinator.fire_due_for_test(
        &mut effects,
        handle.as_mut(),
        workspace_id,
        document,
        base,
        base + 1,
        &mut report,
    );

    assert!(matches!(
        result,
        Err(PortError { ref code, .. }) if code == "injected_reconcile"
    ));
    assert_eq!(effects.begun.len(), 1);
    assert_eq!(effects.aborted, effects.begun);
    assert!(!effects.admission_live);
    assert!(!coordinator.has_claimed_scope(effects.begun[0]));
    assert_eq!(
        harness.schedule_store.unsettled().unwrap()[0].run,
        prior_run
    );
}

#[test]
fn manual_and_scheduled_scope_admission_are_deterministic_in_both_orders() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let mut manual_first = Harness::new("manual-before-schedule");
    let manual_holder = manual_first.running(0xa1);
    let header = hourly_schedule(owner, base, manual_first.scope());
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let mut manual_first_coordinator = coordinator(schedules, &manual_first, owner);
    let report = manual_first_coordinator
        .evaluate(&mut manual_first.orchestrator, base + 1, &zone)
        .unwrap();
    assert!(report.started.is_empty());
    let firing = inspect.schedule().firings().unwrap().remove(0);
    assert_eq!(
        firing.outcome,
        koru_delegate_schedule::ScheduleFiringOutcome::SkippedBusy
    );
    assert_eq!(firing.busy_holder, Some(manual_holder));

    let mut schedule_first = Harness::new("schedule-before-manual");
    let header = hourly_schedule(owner, base, schedule_first.scope());
    let mut schedule_first_coordinator =
        coordinator(MemorySchedules::one(&header), &schedule_first, owner);
    assert_eq!(
        schedule_first_coordinator
            .evaluate(&mut schedule_first.orchestrator, base + 1, &zone)
            .unwrap()
            .started
            .len(),
        1
    );
    let manual = run_id(0xa2);
    schedule_first
        .orchestrator
        .create(request(manual, schedule_first.scope()), workspace())
        .unwrap();
    schedule_first
        .orchestrator
        .start(manual, StartOrigin::Manual)
        .unwrap();
    assert!(
        schedule_first
            .steps(manual)
            .contains(&koru_delegate_core::StepKind::ScopeBusy { holders: 1 }),
        "a later explicit manual run keeps D34's visible warning-and-proceed rule"
    );
}

#[test]
fn two_roots_and_a_handoff_never_start_the_same_occurrence_twice() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let target = DeviceId::from_uuid(v4(0xd4));
    let base = 1_767_225_600_000;
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    let mut owner_harness = Harness::new("schedule-owner-root");
    let mut target_harness = Harness::new("schedule-target-root");
    let header = hourly_schedule(owner, base, owner_harness.scope());
    let schedules = MemorySchedules::one(&header);
    let handoff = koru_delegate_core::HandoffId::from_uuid(v4(0xdf));
    let pending = schedules.schedule();
    pending
        .request_handoff(handoff, target, target, base - 2)
        .unwrap();
    *schedules.snapshot.lock().unwrap() = pending.export_snapshot().unwrap();

    let mut owner_coordinator = coordinator(schedules.clone(), &owner_harness, owner);
    let mut target_coordinator = coordinator(schedules.clone(), &target_harness, target);
    assert!(
        target_coordinator
            .evaluate(&mut target_harness.orchestrator, base + 1, &zone)
            .unwrap()
            .started
            .is_empty(),
        "an unconfirmed target has no authority"
    );
    assert_eq!(
        owner_coordinator
            .evaluate(&mut owner_harness.orchestrator, base + 1, &zone)
            .unwrap()
            .started
            .len(),
        1,
        "the old owner remains active through the handoff gap"
    );
    let owner_calls = owner_harness.calls();
    let target_calls = target_harness.calls();
    assert_eq!(
        owner_calls
            .iter()
            .chain(target_calls.iter())
            .filter(|call| matches!(call, Call::Create { .. }))
            .count(),
        1
    );

    let confirmed = schedules.schedule();
    confirmed.confirm_handoff(handoff, owner, base + 2).unwrap();
    *schedules.snapshot.lock().unwrap() = confirmed.export_snapshot().unwrap();
    assert!(
        owner_coordinator
            .evaluate(&mut owner_harness.orchestrator, base + 3_600_001, &zone,)
            .unwrap()
            .started
            .is_empty(),
        "the confirmed old owner cannot claim a later occurrence"
    );
    assert_eq!(
        target_coordinator
            .evaluate(&mut target_harness.orchestrator, base + 3_600_001, &zone,)
            .unwrap()
            .started
            .len(),
        1,
        "the confirmed new owner takes the next occurrence"
    );
    assert_eq!(
        schedules
            .schedule()
            .firings()
            .unwrap()
            .iter()
            .map(|firing| firing.occurrence_ms)
            .collect::<Vec<_>>(),
        vec![base, base + 3_600_000]
    );
}

/// UC9's integrated failure half: a live pointer that cannot resolve pauses
/// the schedule with an item/stage reason before any claim, Run document, or
/// provider call exists.
#[test]
fn coordinator_context_failure_pauses_visibly_before_claim_or_provider() {
    let mut harness = Harness::new("schedule-context-failure");
    harness.capture.lock().unwrap().fail_resolve = Some(CaptureFailure {
        item: 0,
        stage: CaptureStage::Retention,
    });
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let mut header = hourly_schedule(owner, base, WorkspaceScope::Temporary);
    header.definition.template.context =
        vec![koru_delegate_schedule::ScheduleContextPointer::Document {
            workspace: workspace(),
            document: DocumentId::from_uuid(v4(0xee)),
        }];
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let mut coordinator = coordinator(schedules, &harness, owner);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let report = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    assert!(report.started.is_empty());
    assert!(harness.calls().is_empty());
    assert!(harness.documents.lock().unwrap().snapshots.is_empty());
    assert!(
        harness
            .orchestrator
            .ports()
            .store
            .claims_of(header.schedule)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        inspect.schedule().header().unwrap().definition.lifecycle,
        koru_delegate_schedule::ScheduleLifecycle::Paused {
            reason: koru_delegate_schedule::SchedulePauseReason::ContextUnavailable {
                item: 0,
                stage: koru_delegate_schedule::ScheduleContextFailureStage::Retention,
            }
        }
    ));
    assert!(
        std::fs::read_dir(harness.dir.root.join("temporary"))
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "context refusal leaves no candidate run directory"
    );
}

#[test]
fn deleted_custom_profile_pauses_without_claim_or_provider_start() {
    let mut harness = Harness::new("schedule-deleted-profile");
    harness.sessions.lock().unwrap().profile_delegatable = false;
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let mut header = hourly_schedule(owner, base, harness.scope());
    header.definition.template.agent =
        SessionProfileKey::Custom(koru_session_protocol::SessionProfileId::mint());
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let mut coordinator = coordinator(schedules, &harness, owner);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let report = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    assert!(report.started.is_empty());
    assert!(harness.calls().is_empty());
    assert!(
        harness
            .schedule_store
            .claims_of(header.schedule)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        inspect.schedule().header().unwrap().definition.lifecycle,
        koru_delegate_schedule::ScheduleLifecycle::Paused {
            reason: koru_delegate_schedule::SchedulePauseReason::ProviderNotDelegatable,
        }
    ));
}

#[test]
fn failure_with_backlog_commits_the_pause_before_any_missed_cursor() {
    let mut harness = Harness::new("schedule-backlog-pause-first");
    harness.sessions.lock().unwrap().profile_delegatable = false;
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let now = base + 31 * 24 * 60 * 60_000;
    let header = hourly_schedule(owner, base, harness.scope());
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let mut coordinator = coordinator(schedules, &harness, owner);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    coordinator
        .evaluate(&mut harness.orchestrator, now + 1, &zone)
        .unwrap();
    assert!(matches!(
        inspect.schedule().header().unwrap().definition.lifecycle,
        koru_delegate_schedule::ScheduleLifecycle::Paused {
            reason: koru_delegate_schedule::SchedulePauseReason::ProviderNotDelegatable,
        }
    ));
    assert!(inspect.schedule().firings().unwrap().is_empty());
    assert!(
        harness
            .schedule_store
            .claims_of(header.schedule)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn pause_event_is_prepared_before_store_and_readied_only_after_store_ack() {
    let mut harness = Harness::new("schedule-pause-event-order");
    harness.sessions.lock().unwrap().profile_delegatable = false;
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let header = hourly_schedule(owner, base, WorkspaceScope::Temporary);
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let commits = Arc::new(AtomicUsize::new(0));
    let (events, script) = recording_schedule_events();
    let mut coordinator = ScheduleCoordinator::new(
        Box::new(FailFirstCommitSchedules {
            inner: schedules,
            commits,
        }),
        Box::new(harness.schedule_store.clone()),
        Box::new(TestSchedulePreparation {
            sessions: Arc::clone(&harness.sessions),
            capture: Arc::clone(&harness.capture),
            roots: TestScheduleRoots {
                project: harness.dir.root.join("project"),
                temporary: harness.dir.root.join("temporary"),
            },
            edit_during_pin: None,
            clock_during_pin: None,
        }),
        Box::new(EvaluationNowClock),
        owner,
    )
    .with_events(events);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let report = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();

    assert_eq!(report.failures, 1);
    let calls = &script.lock().unwrap().calls;
    assert_eq!(calls[0], ScheduleEventCall::ExistingCreated);
    let prepared = match calls[1] {
        ScheduleEventCall::PrepareDefinition(
            event,
            ScheduleDefinitionEvent::Paused(SchedulePauseReason::ProviderNotDelegatable),
        ) => event,
        other => panic!("unexpected event call: {other:?}"),
    };
    assert!(!calls.contains(&ScheduleEventCall::Ready(prepared)));
    assert!(matches!(
        inspect.schedule().header().unwrap().definition.lifecycle,
        ScheduleLifecycle::Active
    ));
}

#[test]
fn a_failed_event_prepare_cannot_mutate_the_schedule() {
    let mut harness = Harness::new("schedule-pause-event-prepare-failure");
    harness.sessions.lock().unwrap().profile_delegatable = false;
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let header = hourly_schedule(owner, base, WorkspaceScope::Temporary);
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let (events, script) = recording_schedule_events();
    script.lock().unwrap().fail_definition_prepare = true;
    let mut coordinator = coordinator(schedules, &harness, owner).with_events(events);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let report = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();

    assert_eq!(report.failures, 1);
    assert!(matches!(
        inspect.schedule().header().unwrap().definition.lifecycle,
        ScheduleLifecycle::Active
    ));
    assert!(
        script
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|call| !matches!(call, ScheduleEventCall::Ready(_)))
    );
}

/// A crash after atomic claim+intent but before Run creation is completed
/// forward from the durable payload. Retirement after the claim does not
/// revoke it, and the resulting Run retains scheduled provenance.
#[test]
fn coordinator_recovers_a_claimed_intent_after_retirement_without_restarting_twice() {
    let mut harness = Harness::new("schedule-claimed-recovery");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let mut header = hourly_schedule(owner, base, harness.scope());
    header.definition.template.requested_model = Some(ModelName::new("gpt-5.4").unwrap());
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let run = run_id(0xec);
    let authority = inspect
        .schedule()
        .claim_authority(
            workspace(),
            owner,
            ScheduleClaimPlan::Run {
                occurrence_ms: base,
                run,
                pinned_context: Vec::new(),
            },
        )
        .unwrap()
        .unwrap();
    harness
        .orchestrator
        .ports_mut()
        .store
        .claim_run(authority, 999)
        .unwrap();
    let retired = inspect.schedule();
    retired.retire(owner, base + 1).unwrap();
    *inspect.snapshot.lock().unwrap() = retired.export_snapshot().unwrap();

    let mut coordinator = coordinator(schedules, &harness, owner);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    let report = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    assert_eq!(report.started.len(), 1);
    assert_eq!(report.started[0].run, run);
    let run_header = harness.orchestrator.header(run).unwrap();
    assert_eq!(
        run_header.requested_model,
        header.definition.template.requested_model
    );
    assert_eq!(
        run_header.origin,
        koru_delegate_core::RunOrigin::Scheduled {
            schedule: header.schedule,
            occurrence_ms: base,
            revision: 1,
        }
    );
    assert_eq!(inspect.schedule().firings().unwrap().len(), 1);

    let duplicate = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    assert!(duplicate.started.is_empty());
    assert_eq!(
        harness
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::Create { .. }))
            .count(),
        1
    );
}

#[test]
fn scheduled_capture_failure_stays_pending_and_reconciles_once() {
    let mut harness = Harness::new("schedule-capture-retry");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let header = hourly_schedule(owner, base, harness.scope());
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    harness.capture.lock().unwrap().fail_capture = Some(CaptureFailure {
        item: 0,
        stage: CaptureStage::Commit,
    });
    let mut coordinator = coordinator(schedules, &harness, owner);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let first = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    assert!(first.started.is_empty());
    assert_eq!(first.failures, 1);
    let claim = harness.schedule_store.unsettled().unwrap().remove(0);
    assert_eq!(
        harness
            .orchestrator
            .ports()
            .store
            .intent(claim.run)
            .unwrap()
            .unwrap()
            .state,
        koru_delegate_sqlite::IntentState::Pending
    );
    assert!(harness.calls().is_empty());

    let restart = harness.orchestrator.restart().unwrap();
    assert_eq!(restart.pending_scheduled_scopes.len(), 1);
    assert_eq!(restart.pending_scheduled_scopes[0].run, claim.run);
    assert!(restart.pending_scheduled_scopes[0].identity.is_some());

    harness.capture.lock().unwrap().fail_capture = None;
    let recovered = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    assert_eq!(recovered.started.len(), 1);
    assert_eq!(recovered.started[0].run, claim.run);
    assert!(harness.schedule_store.unsettled().unwrap().is_empty());
    assert_eq!(inspect.schedule().firings().unwrap().len(), 1);
    assert_eq!(
        harness
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::Create { .. }))
            .count(),
        1
    );
}

#[test]
fn due_and_representative_miss_survive_a_failure_after_the_atomic_claim() {
    let mut harness = Harness::new("schedule-due-missed-crash");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let now = base + 31 * 24 * 60 * 60_000;
    harness.schedule_clock.store(now, Ordering::Release);
    harness.capture.lock().unwrap().fail_capture = Some(CaptureFailure {
        item: 0,
        stage: CaptureStage::Commit,
    });
    let header = hourly_schedule(owner, base, harness.scope());
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let mut coordinator = coordinator(schedules, &harness, owner);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let interrupted = coordinator
        .evaluate(&mut harness.orchestrator, now + 1, &zone)
        .unwrap();
    assert!(interrupted.started.is_empty());
    let claims = harness.schedule_store.claims_of(header.schedule).unwrap();
    assert_eq!(claims.len(), 2, "both facts share the durable boundary");
    assert!(claims.iter().any(|claim| claim.occurrence_ms == base
        && matches!(
            claim.state,
            koru_delegate_sqlite::SlotState::Settled(SlotOutcome::SkippedMissed)
        )));
    assert!(claims.iter().any(|claim| claim.occurrence_ms == now
        && matches!(claim.state, koru_delegate_sqlite::SlotState::Claimed)));

    harness.capture.lock().unwrap().fail_capture = None;
    let recovered = coordinator
        .evaluate(&mut harness.orchestrator, now + 1, &zone)
        .unwrap();
    assert_eq!(recovered.started.len(), 1);
    assert!(harness.schedule_store.unsettled().unwrap().is_empty());
    assert_eq!(
        inspect
            .schedule()
            .firings()
            .unwrap()
            .iter()
            .map(|firing| (firing.occurrence_ms, firing.outcome))
            .collect::<Vec<_>>(),
        vec![
            (
                base,
                koru_delegate_schedule::ScheduleFiringOutcome::SkippedMissed
            ),
            (now, koru_delegate_schedule::ScheduleFiringOutcome::Fired),
        ]
    );
    assert_eq!(
        harness
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::Create { .. }))
            .count(),
        1
    );
}

#[test]
fn edit_arriving_during_context_preparation_invalidates_the_whole_candidate() {
    let mut harness = Harness::new("schedule-edit-during-preparation");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let mut header = hourly_schedule(owner, base, WorkspaceScope::Temporary);
    header.definition.template.context =
        vec![koru_delegate_schedule::ScheduleContextPointer::Document {
            workspace: workspace(),
            document: DocumentId::from_uuid(v4(0xed)),
        }];
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let preparation = TestSchedulePreparation {
        sessions: Arc::clone(&harness.sessions),
        capture: Arc::clone(&harness.capture),
        roots: TestScheduleRoots {
            project: harness.dir.root.join("project"),
            temporary: harness.dir.root.join("temporary"),
        },
        edit_during_pin: Some((Arc::clone(&inspect.snapshot), owner, base + 1)),
        clock_during_pin: None,
    };
    let mut coordinator = ScheduleCoordinator::new(
        Box::new(schedules),
        Box::new(harness.schedule_store.clone()),
        Box::new(preparation),
        Box::new(EvaluationNowClock),
        owner,
    );
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let report = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    assert!(report.started.is_empty());
    assert_eq!(report.next_deadline_ms, Some(base + 1));
    assert!(harness.calls().is_empty());
    assert!(
        harness
            .schedule_store
            .claims_of(header.schedule)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        inspect.schedule().header().unwrap().definition.lifecycle,
        koru_delegate_schedule::ScheduleLifecycle::Paused {
            reason: koru_delegate_schedule::SchedulePauseReason::Manual,
        }
    ));
    assert!(
        std::fs::read_dir(harness.dir.root.join("temporary"))
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "an edit race leaves no candidate run directory"
    );
}

fn coordinator_with_clock_change(
    schedules: MemorySchedules,
    harness: &Harness,
    owner: DeviceId,
    final_at_ms: i64,
) -> ScheduleCoordinator {
    ScheduleCoordinator::new(
        Box::new(schedules),
        Box::new(harness.schedule_store.clone()),
        Box::new(TestSchedulePreparation {
            sessions: Arc::clone(&harness.sessions),
            capture: Arc::clone(&harness.capture),
            roots: TestScheduleRoots {
                project: harness.dir.root.join("project"),
                temporary: harness.dir.root.join("temporary"),
            },
            edit_during_pin: None,
            clock_during_pin: Some((Arc::clone(&harness.schedule_clock), final_at_ms)),
        }),
        Box::new(SharedScheduleClock(Arc::clone(&harness.schedule_clock))),
        owner,
    )
}

#[test]
fn preparation_that_crosses_the_due_window_records_a_miss_without_launching() {
    let mut harness = Harness::new("schedule-preparation-crosses-window");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    harness.schedule_clock.store(base + 1, Ordering::Release);
    let mut header = hourly_schedule(owner, base, WorkspaceScope::Temporary);
    header.definition.template.context = vec![];
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let mut coordinator = coordinator_with_clock_change(schedules, &harness, owner, base + 60_000);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let report = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    assert!(report.started.is_empty());
    assert!(harness.calls().is_empty());
    assert_eq!(
        inspect.schedule().firings().unwrap()[0].outcome,
        koru_delegate_schedule::ScheduleFiringOutcome::SkippedMissed
    );
    assert!(
        std::fs::read_dir(harness.dir.root.join("temporary"))
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "the unclaimed temporary candidate was removed"
    );
}

#[test]
fn clock_rollback_during_preparation_reschedules_instead_of_launching_early() {
    let mut harness = Harness::new("schedule-preparation-clock-rollback");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    harness.schedule_clock.store(base + 1, Ordering::Release);
    let header = hourly_schedule(owner, base, WorkspaceScope::Temporary);
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let mut coordinator = coordinator_with_clock_change(schedules, &harness, owner, base - 1);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let report = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    assert!(report.started.is_empty());
    assert_eq!(report.next_deadline_ms, Some(base));
    assert!(inspect.schedule().firings().unwrap().is_empty());
    assert!(
        harness
            .schedule_store
            .claims_of(header.schedule)
            .unwrap()
            .is_empty()
    );
    assert!(harness.calls().is_empty());
}

#[test]
fn busy_is_recorded_only_after_the_final_clock_still_says_due() {
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    for (label, final_at, expected_deadline, expected_outcome) in [
        (
            "busy-clock-rollback",
            1_767_225_600_000 - 1,
            Some(1_767_225_600_000),
            None,
        ),
        (
            "busy-clock-advance",
            1_767_225_600_000 + 60_000,
            Some(1_767_225_600_000 + 3_600_000),
            Some(koru_delegate_schedule::ScheduleFiringOutcome::SkippedMissed),
        ),
    ] {
        let harness = Harness::new(label);
        let owner = DeviceId::from_uuid(v4(0xd3));
        let base = 1_767_225_600_000;
        harness.schedule_clock.store(base + 1, Ordering::Release);
        let header = hourly_schedule(owner, base, WorkspaceScope::Temporary);
        let schedules = MemorySchedules::one(&header);
        let inspect = schedules.clone();
        let mut coordinator = coordinator_with_clock_change(schedules, &harness, owner, final_at);
        let mut effects = TrackingEffects {
            busy: Some(run_id(0xaf)),
            ..TrackingEffects::default()
        };

        let report = coordinator.evaluate(&mut effects, base + 1, &zone).unwrap();
        assert!(report.started.is_empty(), "{label}");
        assert_eq!(report.next_deadline_ms, expected_deadline, "{label}");
        let firings = inspect.schedule().firings().unwrap();
        assert_eq!(
            firings.first().map(|firing| firing.outcome),
            expected_outcome,
            "{label}"
        );
        assert!(
            !firings.iter().any(|firing| matches!(
                firing.outcome,
                koru_delegate_schedule::ScheduleFiringOutcome::SkippedBusy
            )),
            "{label}: stale preflight must not determine the skip reason"
        );
    }
}

#[test]
fn claimed_scope_identity_is_reverified_before_context_materialization() {
    let mut harness = Harness::new("schedule-claimed-scope-swap");
    let run = run_id(0xe8);
    harness
        .orchestrator
        .create(request(run, harness.scope()), workspace())
        .unwrap();
    let unexpected = harness.dir.root.join("unexpected");
    std::fs::create_dir_all(&unexpected).unwrap();
    let expected_identity = ScopeIdentity::of_directory(&unexpected).unwrap();
    let admitted_directory = harness.dir.root.join("project");

    let result = harness.orchestrator.start_claimed(
        run,
        koru_delegate_scope::ResolvedScope {
            identity: expected_identity,
            directory: admitted_directory,
        },
        StartOrigin::ClaimedSchedule {
            schedule: koru_delegate_scope::ScheduleRef::from_uuid(v4(0xd1)),
        },
    );
    assert!(matches!(
        result,
        Err(RunError::Port(PortError { ref code, .. })) if code == "claimed_scope_changed"
    ));
    assert!(harness.capture.lock().unwrap().materialized.is_empty());
    assert!(
        harness
            .orchestrator
            .ports()
            .store
            .current_attempt(run)
            .unwrap()
            .is_none(),
        "identity mismatch remains retryable before an attempt opens"
    );
}

#[test]
fn restart_isolates_an_unavailable_claimed_scope_and_starts_the_next_run() {
    let mut harness = Harness::new("schedule-restart-scope-isolation");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let unavailable_run = run_id(0x21);
    let healthy_run = run_id(0x22);
    for (ordinal, scope, run) in [
        (0_u8, WorkspaceScope::None, unavailable_run),
        (1_u8, harness.scope(), healthy_run),
    ] {
        let mut header = hourly_schedule(owner, base + i64::from(ordinal), scope);
        header.schedule = koru_delegate_core::ScheduleId::from_uuid(v4(0x31 + ordinal));
        let document = ScheduleDocument::create(&header).unwrap();
        let authority = document
            .claim_authority(workspace(), owner, run_plan(base + i64::from(ordinal), run))
            .unwrap()
            .unwrap();
        harness.schedule_store.claim_run(authority, 999).unwrap();
        harness.orchestrator.complete_claimed_intent(run).unwrap();
    }

    let report = harness.orchestrator.restart().unwrap();
    assert_eq!(report.started.len(), 1);
    assert_eq!(report.started[0].run, healthy_run);
    assert_eq!(
        harness.status(unavailable_run),
        koru_delegate_core::RunStatus::Created
    );
    assert!(
        harness
            .orchestrator
            .ports()
            .store
            .current_attempt(unavailable_run)
            .unwrap()
            .is_none()
    );
}

#[test]
fn restart_orders_claimed_schedules_before_overlapping_manual_created_runs() {
    for (label, scheduled_run, manual_run, schedule_first) in [
        ("schedule-first", run_id(0x10), run_id(0x90), true),
        ("manual-first", run_id(0x90), run_id(0x10), false),
    ] {
        let mut harness = Harness::new(label);
        let owner = DeviceId::from_uuid(v4(0xd3));
        let base = 1_767_225_600_000;
        let scheduled_scope = WorkspaceScope::ProjectFolder {
            project: project(1),
            folder: koru_delegate_core::RelativeFolder::new("missing-child").unwrap(),
        };
        let header = hourly_schedule(owner, base, scheduled_scope);
        let document = ScheduleDocument::create(&header).unwrap();

        let create_scheduled = |harness: &mut Harness| {
            let authority = document
                .claim_authority(workspace(), owner, run_plan(base, scheduled_run))
                .unwrap()
                .unwrap();
            harness.schedule_store.claim_run(authority, 999).unwrap();
            harness
                .orchestrator
                .complete_claimed_intent(scheduled_run)
                .unwrap();
        };
        let create_manual = |harness: &mut Harness| {
            harness
                .orchestrator
                .create(request(manual_run, harness.scope()), workspace())
                .unwrap();
        };
        if schedule_first {
            create_scheduled(&mut harness);
            create_manual(&mut harness);
        } else {
            create_manual(&mut harness);
            create_scheduled(&mut harness);
        }

        let report = harness.orchestrator.restart().unwrap();
        assert!(report.started.is_empty(), "{label}");
        assert_eq!(report.pending_scheduled_scopes.len(), 1, "{label}");
        assert_eq!(report.pending_scheduled_scopes[0].run, scheduled_run);
        assert_eq!(report.deferred_manual_starts, vec![manual_run], "{label}");
        assert_eq!(
            harness.status(scheduled_run),
            koru_delegate_core::RunStatus::Created
        );
        assert_eq!(
            harness.status(manual_run),
            koru_delegate_core::RunStatus::Created
        );
        assert!(
            harness
                .orchestrator
                .ports()
                .store
                .current_attempt(manual_run)
                .unwrap()
                .is_none(),
            "{label}: the manual Created run must not pass the unresolved claimed child"
        );

        std::fs::create_dir_all(harness.dir.root.join("project").join("missing-child")).unwrap();
        let scheduled_scope = harness
            .orchestrator
            .plan_scope(scheduled_run, &header.definition.template.scope)
            .unwrap();
        harness
            .orchestrator
            .start_claimed(
                scheduled_run,
                scheduled_scope,
                StartOrigin::ClaimedSchedule {
                    schedule: koru_delegate_scope::ScheduleRef::from_uuid(
                        header.schedule.as_uuid(),
                    ),
                },
            )
            .unwrap();
        let manual_scope = harness
            .orchestrator
            .plan_scope(manual_run, &harness.scope())
            .unwrap();
        harness
            .orchestrator
            .start_resolved(manual_run, manual_scope, StartOrigin::Manual)
            .unwrap();
        assert!(matches!(
            harness.status(scheduled_run),
            koru_delegate_core::RunStatus::Running { .. }
        ));
        assert!(matches!(
            harness.status(manual_run),
            koru_delegate_core::RunStatus::Running { .. }
        ));
    }
}

#[test]
fn restart_reports_a_resolved_claim_that_still_needs_launch() {
    let mut harness = Harness::new("schedule-restart-scope-reservation");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let run = run_id(0x23);
    let header = hourly_schedule(owner, base, harness.scope());
    let document = ScheduleDocument::create(&header).unwrap();
    let authority = document
        .claim_authority(workspace(), owner, run_plan(base, run))
        .unwrap()
        .unwrap();
    harness.schedule_store.claim_run(authority, 999).unwrap();
    harness.orchestrator.complete_claimed_intent(run).unwrap();
    harness.sessions.lock().unwrap().supports_delegation = false;

    let report = harness.orchestrator.restart().unwrap();
    assert!(report.started.is_empty());
    assert_eq!(report.pending_scheduled_scopes.len(), 1);
    assert_eq!(report.pending_scheduled_scopes[0].run, run);
    assert_eq!(harness.status(run), koru_delegate_core::RunStatus::Created);
    assert!(
        harness
            .orchestrator
            .ports()
            .store
            .current_attempt(run)
            .unwrap()
            .is_none(),
        "a transient restart refusal remains retryable before an attempt"
    );
}

#[test]
fn one_unreadable_schedule_snapshot_does_not_starve_a_healthy_due_schedule() {
    let mut harness = Harness::new("schedule-failure-isolation");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let header = hourly_schedule(owner, base, harness.scope());
    let healthy = MemorySchedules::one(&header);
    let bad = ListedSchedule {
        workspace: workspace(),
        document: DocumentId::from_uuid(v4(0xbb)),
        header: {
            let mut header = header.clone();
            header.schedule = koru_delegate_core::ScheduleId::from_uuid(v4(0xbc));
            header
        },
        trashed_at_ms: None,
    };
    let schedules = FaultThenHealthySchedules { bad, healthy };
    let mut coordinator = ScheduleCoordinator::new(
        Box::new(schedules),
        Box::new(harness.schedule_store.clone()),
        Box::new(TestSchedulePreparation {
            sessions: Arc::clone(&harness.sessions),
            capture: Arc::clone(&harness.capture),
            roots: TestScheduleRoots {
                project: harness.dir.root.join("project"),
                temporary: harness.dir.root.join("temporary"),
            },
            edit_during_pin: None,
            clock_during_pin: None,
        }),
        Box::new(EvaluationNowClock),
        owner,
    );
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let counters_before = crate::counters::schedule_test_snapshot();
    let report = coordinator
        .evaluate(&mut harness.orchestrator, base + 1, &zone)
        .unwrap();
    let counters_after = crate::counters::schedule_test_snapshot();
    assert_eq!(report.failures, 1);
    assert_eq!(
        counters_after.evaluation_failures - counters_before.evaluation_failures,
        1
    );
    assert_eq!(
        counters_after.documents_evaluated - counters_before.documents_evaluated,
        1,
        "the unreadable catalog row is a failure, not a document the evaluator opened"
    );
    assert_eq!(report.started.len(), 1);
    assert_eq!(report.next_deadline_ms, Some(base + 1_001));
    assert_eq!(
        harness
            .calls()
            .iter()
            .filter(|call| matches!(call, Call::Create { .. }))
            .count(),
        1
    );
}

#[test]
fn final_absorb_failure_releases_the_actor_admission() {
    let harness = Harness::new("schedule-admission-abort");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let inner = MemorySchedules::one(&hourly_schedule(owner, base, harness.scope()));
    let absorbs = Arc::new(AtomicUsize::new(0));
    let schedules = FailSecondAbsorbSchedules {
        inner,
        absorbs: Arc::clone(&absorbs),
    };
    let mut coordinator = coordinator_with_schedules(Box::new(schedules), &harness, owner);
    let mut effects = TrackingEffects::default();
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let report = coordinator.evaluate(&mut effects, base + 1, &zone).unwrap();
    assert_eq!(report.failures, 1);
    assert_eq!(absorbs.load(Ordering::Acquire), 2);
    assert_eq!(effects.begun.len(), 1);
    assert_eq!(effects.aborted, effects.begun);
    assert!(harness.schedule_store.unsettled().unwrap().is_empty());
}

#[test]
fn a_transient_claim_read_keeps_postclaim_finish_authority() {
    let harness = Harness::new("schedule-postclaim-read-retry");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let schedules = MemorySchedules::one(&hourly_schedule(owner, base, harness.scope()));
    let inspect = schedules.clone();
    let mut coordinator = coordinator_with_schedules(Box::new(schedules), &harness, owner);
    let mut effects = TrackingEffects::default();
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    harness.ledger.lock().unwrap().fail = Some("schedule:by_run");

    let first = coordinator.evaluate(&mut effects, base + 1, &zone).unwrap();
    assert_eq!(first.failures, 1);
    assert!(effects.finished.is_empty());
    assert!(effects.aborted.is_empty());
    assert!(effects.admission_live);
    assert_eq!(harness.schedule_store.unsettled().unwrap().len(), 1);

    let second = coordinator.evaluate(&mut effects, base + 2, &zone).unwrap();
    assert_eq!(second.failures, 0);
    assert_eq!(effects.finished.len(), 1);
    assert!(effects.reconciled.is_empty());
    assert!(harness.schedule_store.unsettled().unwrap().is_empty());
    assert_eq!(inspect.schedule().firings().unwrap().len(), 1);
}

#[test]
fn a_transient_settle_after_finish_reconciles_without_reusing_the_lease() {
    let harness = Harness::new("schedule-postfinish-settle-retry");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let schedules = MemorySchedules::one(&hourly_schedule(owner, base, harness.scope()));
    let inspect = schedules.clone();
    let mut coordinator = coordinator_with_schedules(Box::new(schedules), &harness, owner);
    let mut effects = TrackingEffects::default();
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    harness.ledger.lock().unwrap().fail = Some("schedule:settle");

    let first = coordinator.evaluate(&mut effects, base + 1, &zone).unwrap();
    assert_eq!(first.failures, 1);
    assert_eq!(effects.finished.len(), 1);
    assert!(!effects.admission_live);
    assert_eq!(harness.schedule_store.unsettled().unwrap().len(), 1);
    assert_eq!(inspect.schedule().firings().unwrap().len(), 1);

    let second = coordinator.evaluate(&mut effects, base + 2, &zone).unwrap();
    assert_eq!(second.failures, 0);
    assert_eq!(effects.finished.len(), 1, "Finish is not replayed");
    assert_eq!(effects.reconciled.len(), 1);
    assert!(harness.schedule_store.unsettled().unwrap().is_empty());
    assert_eq!(inspect.schedule().firings().unwrap().len(), 1);
}

#[test]
fn a_transient_firing_append_after_finish_reconciles_and_repairs_history() {
    let counters_before = crate::counters::schedule_test_snapshot();
    let harness = Harness::new("schedule-postfinish-history-retry");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let schedules = MemorySchedules::one(&hourly_schedule(owner, base, harness.scope()));
    let inspect = schedules.clone();
    let commits = Arc::new(AtomicUsize::new(0));
    let mut coordinator = coordinator_with_schedules(
        Box::new(FailFirstCommitSchedules {
            inner: schedules,
            commits: Arc::clone(&commits),
        }),
        &harness,
        owner,
    );
    let mut effects = TrackingEffects::default();
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let first = coordinator.evaluate(&mut effects, base + 1, &zone).unwrap();
    assert_eq!(first.failures, 1);
    assert_eq!(effects.finished.len(), 1);
    assert_eq!(harness.schedule_store.unsettled().unwrap().len(), 1);
    assert!(inspect.schedule().firings().unwrap().is_empty());
    let counters_after_failed_live_append = crate::counters::schedule_test_snapshot();
    assert_eq!(
        counters_after_failed_live_append.fired, counters_before.fired,
        "a failed live append has no outcome"
    );

    let second = coordinator.evaluate(&mut effects, base + 2, &zone).unwrap();
    assert_eq!(second.failures, 0);
    assert_eq!(effects.finished.len(), 1, "Finish is not replayed");
    assert_eq!(effects.reconciled.len(), 1);
    assert!(harness.schedule_store.unsettled().unwrap().is_empty());
    assert_eq!(inspect.schedule().firings().unwrap().len(), 1);
    assert_eq!(commits.load(Ordering::Acquire), 2);
    let counters_after_repair = crate::counters::schedule_test_snapshot();
    assert_eq!(
        counters_after_repair.fired, counters_after_failed_live_append.fired,
        "reconciliation repairs history without reconstructing a live outcome"
    );
}

#[test]
fn one_failed_reconciliation_does_not_starve_later_claims_or_catalog_work() {
    let harness = Harness::new("schedule-reconcile-isolation");
    let owner = DeviceId::from_uuid(v4(0xd3));
    let base = 1_767_225_600_000;
    let first_header = hourly_schedule(owner, base, harness.scope());
    let mut second_header = first_header.clone();
    second_header.schedule = koru_delegate_core::ScheduleId::from_uuid(v4(0xce));
    let first = ScheduleDocument::create(&first_header).unwrap();
    let second = ScheduleDocument::create(&second_header).unwrap();
    let first_run = run_id(0xc1);
    let second_run = run_id(0xc2);
    let mut ledger = harness.schedule_store.clone();
    for (document, run) in [(&first, first_run), (&second, second_run)] {
        let authority = document
            .claim_authority(workspace(), owner, run_plan(base, run))
            .unwrap()
            .unwrap();
        ledger.claim_run(authority, 999).unwrap();
    }
    let mut coordinator = coordinator_with_schedules(Box::new(EmptySchedules), &harness, owner);
    let mut effects = TrackingEffects {
        fail_reconcile: Some(first_run),
        ..TrackingEffects::default()
    };
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let report = coordinator.evaluate(&mut effects, base + 1, &zone).unwrap();
    assert_eq!(report.failures, 1);
    assert_eq!(report.evaluated, 0, "the empty catalog was still reached");
    assert_eq!(effects.reconciled.len(), 2);
    assert!(matches!(
        ledger.by_run(first_run).unwrap().unwrap().state,
        koru_delegate_sqlite::SlotState::Claimed
    ));
    assert!(matches!(
        ledger.by_run(second_run).unwrap().unwrap().state,
        koru_delegate_sqlite::SlotState::Settled(SlotOutcome::Fired)
    ));
}

fn coordinator_with_schedules(
    schedules: Box<dyn ScheduleDocuments>,
    harness: &Harness,
    owner: DeviceId,
) -> ScheduleCoordinator {
    ScheduleCoordinator::new(
        schedules,
        Box::new(harness.schedule_store.clone()),
        Box::new(TestSchedulePreparation {
            sessions: Arc::clone(&harness.sessions),
            capture: Arc::clone(&harness.capture),
            roots: TestScheduleRoots {
                project: harness.dir.root.join("project"),
                temporary: harness.dir.root.join("temporary"),
            },
            edit_during_pin: None,
            clock_during_pin: None,
        }),
        Box::new(EvaluationNowClock),
        owner,
    )
}

fn listing_coordinator(
    schedules: impl ScheduleDocuments + 'static,
    device: DeviceId,
) -> ScheduleCoordinator {
    let schedule_clock = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let store = RecordingStore {
        inner: Arc::new(Mutex::new(
            RunsDb::open_with_clock(
                std::path::Path::new(":memory:"),
                Box::new(SharedScheduleClock(Arc::clone(&schedule_clock))),
            )
            .unwrap(),
        )),
        script: Arc::new(Mutex::new(LedgerScript::default())),
    };
    let coordinator = ScheduleCoordinator::new(
        Box::new(schedules),
        Box::new(store),
        Box::new(TestSchedulePreparation {
            sessions: Arc::new(Mutex::new(SessionScript::default())),
            capture: Arc::new(Mutex::new(CaptureScript::default())),
            roots: TestScheduleRoots {
                project: "unused-project".into(),
                temporary: "unused-temporary".into(),
            },
            edit_during_pin: None,
            clock_during_pin: None,
        }),
        Box::new(EvaluationNowClock),
        device,
    );
    routine::mutations::with_routes(coordinator, None, false)
}

#[test]
fn schedule_create_stamps_worker_authority_and_reads_canonical_state() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let mut prior = active_schedule(device);
    prior.definition.template.requested_model =
        Some(koru_delegate_core::ModelName::new("gpt-test").unwrap());
    prior.definition.template.context =
        vec![koru_delegate_schedule::ScheduleContextPointer::Document {
            workspace: workspace(),
            document: DocumentId::from_uuid(v4(0xf0)),
        }];
    let schedules = MemorySchedules::one(&prior);
    let mut coordinator = listing_coordinator(schedules, device);
    let schedule = koru_delegate_core::ScheduleId::from_uuid(v4(0xd4));
    let at_ms = 1_767_225_600_123;
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    let template = prior.definition.template.clone();
    let recurrence = prior.definition.recurrence.clone();

    let listed = coordinator
        .create_schedule(
            workspace(),
            schedule,
            template.clone(),
            recurrence.clone(),
            at_ms,
            &zone,
        )
        .unwrap();

    assert_eq!(listed.header.schedule, schedule);
    assert_eq!(listed.header.initial_owner, device);
    assert_eq!(listed.header.created_at_ms, at_ms);
    assert_eq!(listed.header.definition.revision, 1);
    assert_eq!(listed.header.definition.effective_at_ms, at_ms);
    assert_eq!(listed.header.definition.template, template);
    assert_eq!(listed.header.definition.recurrence, recurrence);
    assert!(matches!(
        listed.header.definition.lifecycle,
        koru_delegate_schedule::ScheduleLifecycle::Active
    ));
    assert_eq!(
        listed.state,
        koru_delegate_schedule::ScheduleState::Active { owner: device }
    );
}

#[test]
fn schedule_edit_stamps_one_monotonic_revision_and_orders_its_event() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(device);
    let original_name = ScheduleName::from_objective(&header.definition.template.objective);
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let (events, script) = recording_schedule_events();
    let mut coordinator = listing_coordinator(schedules, device).with_events(events);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    let name = ScheduleName::new("Research launches").unwrap();
    let mut template = header.definition.template.clone();
    template.objective = koru_delegate_core::Objective::new("Research and report").unwrap();
    template.requested_model = Some(ModelName::new("gpt-test").unwrap());
    let recurrence = koru_recurrence::RecurrenceRule::floating(
        koru_recurrence::CronExpression::parse("*/5 * * * *").unwrap(),
    );

    let listed = coordinator
        .edit_schedule(
            workspace(),
            header.schedule,
            1,
            &original_name,
            &name,
            template.clone(),
            recurrence.clone(),
            1,
            &zone,
        )
        .unwrap();

    assert_eq!(listed.name, name);
    assert_eq!(listed.header.definition.revision, 2);
    assert_eq!(listed.header.definition.effective_at_ms, 2);
    assert_eq!(listed.header.definition.template, template);
    assert_eq!(listed.header.definition.recurrence, recurrence);
    assert_eq!(inspect.schedule().header().unwrap(), listed.header);
    let calls = script.lock().unwrap().calls.clone();
    let event = match calls.as_slice() {
        [
            ScheduleEventCall::PrepareDefinition(event, ScheduleDefinitionEvent::Updated),
            ScheduleEventCall::Ready(ready),
        ] if event == ready => *event,
        other => panic!("unexpected edit event calls: {other:?}"),
    };
    assert!(calls.contains(&ScheduleEventCall::Ready(event)));

    let retry = coordinator
        .edit_schedule(
            workspace(),
            header.schedule,
            1,
            &original_name,
            &name,
            template,
            recurrence,
            99_000,
            &zone,
        )
        .unwrap();
    assert_eq!(retry.header.definition.revision, 2);
    assert_eq!(retry.header.definition.effective_at_ms, 2);
}

#[test]
fn pause_and_resume_are_revision_safe_and_resume_every_paused_reason() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(device);
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let (events, script) = recording_schedule_events();
    let mut coordinator = listing_coordinator(schedules, device).with_events(events);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let paused = coordinator
        .pause_schedule(
            workspace(),
            header.schedule,
            1,
            header.definition.effective_at_ms + 1,
            &zone,
        )
        .unwrap();
    assert_eq!(paused.header.definition.revision, 2);
    assert!(matches!(
        paused.header.definition.lifecycle,
        ScheduleLifecycle::Paused {
            reason: SchedulePauseReason::Manual
        }
    ));
    assert_eq!(paused.next, ScheduleNext::None);

    let retried = coordinator
        .pause_schedule(workspace(), header.schedule, 1, 99_000, &zone)
        .unwrap();
    assert_eq!(retried.header.definition, paused.header.definition);

    let resumed = coordinator
        .resume_schedule(
            workspace(),
            header.schedule,
            2,
            paused.header.definition.effective_at_ms + 60_000,
            &zone,
        )
        .unwrap();
    assert_eq!(resumed.header.definition.revision, 3);
    assert!(matches!(
        resumed.header.definition.lifecycle,
        ScheduleLifecycle::Active
    ));
    assert!(resumed.header.definition.effective_at_ms > paused.header.definition.effective_at_ms);
    assert_eq!(inspect.schedule().header().unwrap(), resumed.header);

    let calls = &script.lock().unwrap().calls;
    assert!(matches!(
        calls.first(),
        Some(ScheduleEventCall::PrepareDefinition(
            _,
            ScheduleDefinitionEvent::Paused(SchedulePauseReason::Manual)
        ))
    ));
    assert!(calls.iter().any(|call| matches!(
        call,
        ScheduleEventCall::PrepareDefinition(_, ScheduleDefinitionEvent::Updated)
    )));

    for reason in [
        SchedulePauseReason::ProviderNotDelegatable,
        SchedulePauseReason::ScopeUnavailable,
        SchedulePauseReason::ContextUnavailable {
            item: 0,
            stage: koru_delegate_schedule::ScheduleContextFailureStage::Resolve,
        },
    ] {
        let mut paused_header = active_schedule(device);
        paused_header.definition.lifecycle = ScheduleLifecycle::Paused { reason };
        let schedules = MemorySchedules::one(&paused_header);
        let mut coordinator = listing_coordinator(schedules, device);
        let resumed = coordinator
            .resume_schedule(
                workspace(),
                paused_header.schedule,
                paused_header.definition.revision,
                paused_header.definition.effective_at_ms + 1,
                &zone,
            )
            .unwrap();
        assert!(matches!(
            resumed.header.definition.lifecycle,
            ScheduleLifecycle::Active
        ));
    }
}

#[test]
fn retirement_is_idempotent_hidden_authority_and_preserves_firings() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(owner);
    let mut schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let firing = ScheduleFiring {
        occurrence_ms: header.definition.effective_at_ms + 60_000,
        revision: 1,
        run: RunId::from_uuid(v4(0xd9)),
        outcome: ScheduleFiringOutcome::Fired,
        busy_holder: None,
        at: header.definition.effective_at_ms + 60_001,
    };
    let mut seeded = schedules.open(workspace(), schedules.document).unwrap();
    seeded
        .commit(&mut |document| {
            document.append_firing(&firing)?;
            Ok(())
        })
        .unwrap();
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    let mut coordinator = listing_coordinator(schedules.clone(), owner);

    let retired = coordinator
        .retire_schedule(
            workspace(),
            header.schedule,
            header.definition.effective_at_ms + 70_000,
            &zone,
        )
        .unwrap();
    assert_eq!(retired.state, ScheduleState::Retired);
    assert_eq!(retired.firings, vec![firing.clone()]);
    assert_eq!(inspect.schedule().firings().unwrap(), vec![firing]);

    let retry = coordinator
        .retire_schedule(workspace(), header.schedule, 99_000, &zone)
        .unwrap();
    assert_eq!(retry.state, ScheduleState::Retired);

    let mut remote = listing_coordinator(schedules, DeviceId::from_uuid(v4(0xd4)));
    let error = remote
        .retire_schedule(workspace(), header.schedule, 100_000, &zone)
        .unwrap_err();
    assert_eq!(error.code, "schedule_not_owner");
}

#[test]
fn schedule_rename_changes_only_name_and_exact_retry_succeeds() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(device);
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let mut coordinator = listing_coordinator(schedules, device);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    let original = inspect.schedule().name().unwrap();
    let renamed = ScheduleName::new("Research weekly launches").unwrap();

    let listed = coordinator
        .rename_schedule(
            workspace(),
            header.schedule,
            &original,
            &renamed,
            header.created_at_ms + 10,
            &zone,
        )
        .unwrap();
    assert_eq!(listed.name, renamed);
    assert_eq!(listed.header, header);
    assert!(listed.firings.is_empty());

    let retried = coordinator
        .rename_schedule(
            workspace(),
            header.schedule,
            &original,
            &renamed,
            header.created_at_ms + 11,
            &zone,
        )
        .unwrap();
    assert_eq!(retried.name, renamed);
    assert_eq!(inspect.schedule().header().unwrap(), header);
}

#[test]
fn stale_and_remote_schedule_renames_are_refused() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(owner);
    let schedules = MemorySchedules::one(&header);
    let original = schedules.schedule().name().unwrap();
    let first = ScheduleName::new("First rename").unwrap();
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    let mut coordinator = listing_coordinator(schedules.clone(), owner);
    coordinator
        .rename_schedule(
            workspace(),
            header.schedule,
            &original,
            &first,
            header.created_at_ms + 10,
            &zone,
        )
        .unwrap();
    let error = coordinator
        .rename_schedule(
            workspace(),
            header.schedule,
            &original,
            &ScheduleName::new("Stale overwrite").unwrap(),
            header.created_at_ms + 11,
            &zone,
        )
        .unwrap_err();
    assert_eq!(error.code, "schedule_name_conflict");

    let mut remote = listing_coordinator(schedules, DeviceId::from_uuid(v4(0xd4)));
    let error = remote
        .rename_schedule(
            workspace(),
            header.schedule,
            &first,
            &ScheduleName::new("Remote overwrite").unwrap(),
            header.created_at_ms + 12,
            &zone,
        )
        .unwrap_err();
    assert_eq!(error.code, "schedule_not_owner");
}

#[test]
fn store_create_failure_leaves_the_event_prepared_not_ready() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(device);
    let (events, script) = recording_schedule_events();
    let mut coordinator = listing_coordinator(FailingCreateSchedules, device).with_events(events);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let error = coordinator
        .create_schedule(
            workspace(),
            header.schedule,
            header.definition.template,
            header.definition.recurrence,
            header.created_at_ms,
            &zone,
        )
        .unwrap_err();

    assert_eq!(error.port, Port::Schedules);
    let calls = &script.lock().unwrap().calls;
    let prepared = match calls.as_slice() {
        [ScheduleEventCall::PrepareCreated(event)] => *event,
        other => panic!("unexpected event calls: {other:?}"),
    };
    assert!(!calls.contains(&ScheduleEventCall::Ready(prepared)));
}

#[test]
fn a_create_identity_race_abandons_the_loser_then_seeds_the_winner_root() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let prior = active_schedule(device);
    let schedules = MemorySchedules::one(&prior);
    let (events, script) = recording_schedule_events();
    let mut coordinator = listing_coordinator(schedules, device).with_events(events);
    let schedule = koru_delegate_core::ScheduleId::from_uuid(v4(0xd4));
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let listed = coordinator
        .create_schedule(
            workspace(),
            schedule,
            prior.definition.template,
            prior.definition.recurrence,
            prior.created_at_ms + 1,
            &zone,
        )
        .unwrap();

    assert_eq!(listed.header.schedule, schedule);
    let calls = &script.lock().unwrap().calls;
    let losing = match calls.as_slice() {
        [
            ScheduleEventCall::PrepareCreated(event),
            ScheduleEventCall::Abandon(abandoned),
            ScheduleEventCall::ExistingCreated,
        ] if event == abandoned => *event,
        other => panic!("unexpected event calls: {other:?}"),
    };
    assert!(!calls.contains(&ScheduleEventCall::Ready(losing)));
}

#[test]
fn schedule_create_retry_resolves_the_durable_document_after_seed_expiry() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(device);
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let mut coordinator = listing_coordinator(schedules, device);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let listed = coordinator
        .create_schedule(
            workspace(),
            header.schedule,
            header.definition.template.clone(),
            header.definition.recurrence.clone(),
            header.created_at_ms + 8 * 24 * 60 * 60 * 1_000,
            &zone,
        )
        .unwrap();

    assert_eq!(listed.header, header);
    assert_eq!(inspect.schedule().header().unwrap(), header);
}

#[test]
fn schedule_id_is_the_retry_identity_after_authored_input_changes() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(device);
    let schedules = MemorySchedules::one(&header);
    let inspect = schedules.clone();
    let mut coordinator = listing_coordinator(schedules, device);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    let mut different = header.definition.template.clone();
    different.objective = koru_delegate_core::Objective::new("a different objective").unwrap();

    let listed = coordinator
        .create_schedule(
            workspace(),
            header.schedule,
            different,
            header.definition.recurrence.clone(),
            header.created_at_ms + 8 * 24 * 60 * 60 * 1_000,
            &zone,
        )
        .unwrap();

    assert_eq!(listed.header, header);
    assert_eq!(inspect.schedule().header().unwrap(), header);
}

#[test]
fn schedule_create_fails_closed_on_a_partial_catalog() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(device);
    let healthy = MemorySchedules::one(&header);
    let inspect = healthy.clone();
    let bad = ListedSchedule {
        workspace: workspace(),
        document: DocumentId::from_uuid(v4(0xbb)),
        header: {
            let mut other = header.clone();
            other.schedule = koru_delegate_core::ScheduleId::from_uuid(v4(0xbc));
            other
        },
        trashed_at_ms: None,
    };
    let mut coordinator = listing_coordinator(FaultThenHealthySchedules { bad, healthy }, device);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let error = coordinator
        .create_schedule(
            workspace(),
            koru_delegate_core::ScheduleId::from_uuid(v4(0xd4)),
            header.definition.template.clone(),
            header.definition.recurrence.clone(),
            1_767_225_600_123,
            &zone,
        )
        .unwrap_err();

    assert_eq!(error.code, "schedule_unavailable");
    assert_eq!(inspect.schedule().header().unwrap(), header);
}

#[test]
fn schedule_create_does_not_write_when_catalog_discovery_fails() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(device);
    let creates = Arc::new(AtomicUsize::new(0));
    let mut coordinator = listing_coordinator(
        FailedDiscoverySchedules {
            creates: Arc::clone(&creates),
        },
        device,
    );
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let error = coordinator
        .create_schedule(
            workspace(),
            header.schedule,
            header.definition.template,
            header.definition.recurrence,
            1_767_225_600_123,
            &zone,
        )
        .unwrap_err();

    assert_eq!(error.code, "catalog_unavailable");
    assert_eq!(creates.load(Ordering::Acquire), 0);
}

#[test]
fn deleted_workspace_identity_blocks_expired_reuse_and_survives_restore() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(device);
    let retained = MemorySchedules::one(&header);
    let target = WorkspaceId::from_uuid(v4(0xe1));
    let restored = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let creates = Arc::new(AtomicUsize::new(0));
    let mut coordinator = listing_coordinator(
        DeletedWorkspaceSchedules {
            retained,
            target,
            restored: Arc::clone(&restored),
            creates: Arc::clone(&creates),
        },
        device,
    );
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let error = coordinator
        .create_schedule(
            target,
            header.schedule,
            header.definition.template.clone(),
            header.definition.recurrence.clone(),
            header.created_at_ms + 8 * 24 * 60 * 60 * 1_000,
            &zone,
        )
        .unwrap_err();
    assert_eq!(error.code, "schedule_conflict");
    assert_eq!(creates.load(Ordering::Acquire), 0);

    restored.store(true, Ordering::Release);
    let listed = coordinator
        .list_schedules(workspace(), header.created_at_ms + 9, &zone)
        .unwrap();
    assert_eq!(listed.schedules.len(), 1);
    assert_eq!(listed.schedules[0].header, header);
}

#[test]
fn a_remote_floating_schedule_has_no_fabricated_exact_next_instant() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let local = DeviceId::from_uuid(v4(0xd4));
    let header = active_schedule(owner);
    let schedules = MemorySchedules::one(&header);
    let mut coordinator = listing_coordinator(schedules, local);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let listed = coordinator
        .list_schedules(workspace(), 1_767_225_600_123, &zone)
        .unwrap();
    assert!(!listed.incomplete);
    assert_eq!(listed.schedules.len(), 1);
    assert_eq!(listed.schedules[0].next, ScheduleNext::OwnerLocal);
}

#[test]
fn a_zoned_schedule_has_an_exact_next_instant_on_a_non_owner() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let local = DeviceId::from_uuid(v4(0xd4));
    let mut header = active_schedule(owner);
    header.definition.recurrence =
        koru_recurrence::RecurrenceRule::zoned("0 * * * *".parse().unwrap(), "UTC").unwrap();
    let schedules = MemorySchedules::one(&header);
    let mut coordinator = listing_coordinator(schedules, local);
    let zone = koru_time_zone::IanaZoneRules::parse("America/New_York").unwrap();

    let listed = coordinator
        .list_schedules(workspace(), 1_767_225_600_123, &zone)
        .unwrap();
    assert!(matches!(listed.schedules[0].next, ScheduleNext::Exact(_)));
}

#[test]
fn a_partial_catalog_keeps_healthy_rows_and_says_it_is_incomplete() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(device);
    let healthy = MemorySchedules::one(&header);
    let bad = ListedSchedule {
        workspace: workspace(),
        document: DocumentId::from_uuid(v4(0xbb)),
        header: {
            let mut other = header.clone();
            other.schedule = koru_delegate_core::ScheduleId::from_uuid(v4(0xbc));
            other
        },
        trashed_at_ms: None,
    };
    let mut coordinator = listing_coordinator(FaultThenHealthySchedules { bad, healthy }, device);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let listed = coordinator
        .list_schedules(workspace(), 1_767_225_600_123, &zone)
        .unwrap();
    assert!(listed.incomplete);
    assert_eq!(listed.schedules.len(), 1);
    assert_eq!(listed.schedules[0].header.schedule, header.schedule);
}

#[test]
fn list_isolates_a_failed_row_and_an_unavailable_row() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(device);
    let healthy = MemorySchedules::one(&header);
    let bad = ListedSchedule {
        workspace: workspace(),
        document: DocumentId::from_uuid(v4(0xbb)),
        header: {
            let mut other = header.clone();
            other.schedule = koru_delegate_core::ScheduleId::from_uuid(v4(0xbc));
            other
        },
        trashed_at_ms: None,
    };
    let mut coordinator = listing_coordinator(
        RowFailureSchedules {
            bad,
            unavailable: DocumentId::from_uuid(v4(0xbd)),
            healthy,
        },
        device,
    );
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let listed = coordinator
        .list_schedules(workspace(), 1_767_225_600_123, &zone)
        .unwrap();
    assert!(listed.incomplete);
    assert_eq!(listed.schedules.len(), 1);
    assert_eq!(listed.schedules[0].header.schedule, header.schedule);
}

#[test]
fn failures_outside_the_requested_workspace_do_not_taint_its_list() {
    let device = DeviceId::from_uuid(v4(0xd3));
    let header = active_schedule(device);
    let healthy = MemorySchedules::one(&header);
    let other_workspace = WorkspaceId::from_uuid(v4(0xe1));
    let bad = ListedSchedule {
        workspace: other_workspace,
        document: DocumentId::from_uuid(v4(0xbb)),
        header: {
            let mut other = header.clone();
            other.schedule = koru_delegate_core::ScheduleId::from_uuid(v4(0xbc));
            other
        },
        trashed_at_ms: None,
    };
    let mut coordinator = listing_coordinator(
        RowFailureSchedules {
            bad,
            unavailable: DocumentId::from_uuid(v4(0xbd)),
            healthy,
        },
        device,
    );
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();

    let listed = coordinator
        .list_schedules(workspace(), 1_767_225_600_123, &zone)
        .unwrap();
    assert!(!listed.incomplete);
    assert_eq!(listed.schedules.len(), 1);
}
