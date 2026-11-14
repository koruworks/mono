// SPDX-License-Identifier: MPL-2.0

//! Actual coordinator, live Loro and SQLite memory; no Store or provider.
use super::*;
use crate::*;
use std::time::{Duration, Instant};

mod cases;

#[derive(Default)]
struct Probe {
    handles: usize,
    gate: bool,
    lease: Option<RunId>,
    calls: Vec<&'static str>,
    link_runs: Vec<RunId>,
    missing: bool,
    unknown_prepare: bool,
    unavailable: bool,
    refuse_preclaim: bool,
    lose_ack: bool,
    busy: bool,
    fail_absorb: bool,
    stop_absorb: bool,
    fail_release: bool,
    late_open: bool,
    bad_prepared: bool,
    opens: usize,
    stopped: bool,
}
type SharedProbe = Arc<Mutex<Probe>>;
fn fail() -> PortError {
    PortError::new(Port::Schedules, "injected", "composition fixture")
}

#[derive(Clone)]
struct Documents {
    workspace: WorkspaceId,
    document: DocumentId,
    snapshot: Arc<Mutex<Vec<u8>>>,
    probe: SharedProbe,
    alias: bool,
    others: Vec<(DocumentId, Vec<u8>)>,
}
impl Documents {
    fn read(&self) -> ScheduleDocument {
        ScheduleDocument::from_snapshot(&self.snapshot.lock().unwrap()).unwrap()
    }
}
struct Handle {
    docs: Documents,
    live: ScheduleDocument,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.docs.probe.lock().unwrap().handles -= 1;
    }
}
impl ScheduleDocumentHandle for Handle {
    fn document(&self) -> &ScheduleDocument {
        &self.live
    }
    fn claim_authority(
        &self,
        device: DeviceId,
        plan: ScheduleClaimPlan,
    ) -> PortResult<Option<ScheduleClaimAuthority>> {
        assert!(self.docs.probe.lock().unwrap().gate);
        self.live
            .claim_authority(self.docs.workspace, device, plan)
            .map_err(Into::into)
    }
    fn expired_routine_authority(
        &self,
        device: DeviceId,
        plan: koru_delegate_document::RoutineExpiryPlan,
    ) -> PortResult<Option<koru_delegate_document::RoutineExpiryAuthority>> {
        assert!(self.docs.probe.lock().unwrap().gate);
        self.live
            .expired_routine_authority(self.docs.workspace, device, plan)
            .map_err(Into::into)
    }
    fn absorb(&mut self) -> PortResult<bool> {
        let mut probe = self.docs.probe.lock().unwrap();
        if probe.lease.is_some() && probe.fail_absorb {
            return Err(fail());
        }
        if probe.lease.is_some() && probe.stop_absorb {
            probe.stopped = true;
        }
        drop(probe);
        let before = self.live.version();
        self.live.import(&self.docs.snapshot.lock().unwrap())?;
        Ok(before != self.live.version())
    }
    fn commit(
        &mut self,
        write: &mut dyn FnMut(
            &ScheduleDocument,
        ) -> Result<(), koru_delegate_document::ScheduleDocumentError>,
    ) -> PortResult<()> {
        write(&self.live)?;
        *self.docs.snapshot.lock().unwrap() = self.live.export_snapshot()?;
        Ok(())
    }
}
struct Watch;
impl ScheduleChangeWatch for Watch {
    fn close(self: Box<Self>) {}
}
impl ScheduleDocuments for Documents {
    fn create(&mut self, _: WorkspaceId, _: &ScheduleHeader) -> PortResult<DocumentId> {
        panic!("no creation")
    }
    fn open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        assert_eq!(workspace, self.workspace);
        if document != self.document {
            let snapshot = self
                .others
                .iter()
                .find(|(id, _)| *id == document)
                .expect("known physical document")
                .1
                .clone();
            let mut other = self.clone();
            other.document = document;
            other.snapshot = Arc::new(Mutex::new(snapshot));
            other.others.clear();
            return other.open(workspace, document);
        }
        let mut probe = self.probe.lock().unwrap();
        probe.opens += 1;
        if probe.late_open && probe.calls.contains(&"preclaim") {
            probe.stopped = true;
        }
        probe.handles += 1;
        drop(probe);
        Ok(Box::new(Handle {
            docs: self.clone(),
            live: self.read(),
        }))
    }
    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        let listed = ListedSchedule {
            workspace: self.workspace,
            document: self.document,
            header: self.read().header()?,
            trashed_at_ms: None,
        };
        let mut schedules = vec![listed.clone()];
        for (document, snapshot) in &self.others {
            schedules.push(ListedSchedule {
                workspace: self.workspace,
                document: *document,
                header: ScheduleDocument::from_snapshot(snapshot)
                    .unwrap()
                    .header()?,
                trashed_at_ms: None,
            });
        }
        if self.alias {
            let mut alias = listed;
            alias.document = DocumentId::from_uuid(v4(1));
            schedules.insert(0, alias);
        }
        Ok(ScheduleCatalog {
            schedules,
            workspaces: vec![self.workspace],
            failed_workspaces: vec![],
            unavailable_documents: vec![],
            failures: 0,
        })
    }
    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        self.discover_active()
    }
    fn watch_changes(
        &mut self,
        _: &ScheduleWatchTargets,
        _: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        Ok(Box::new(Watch))
    }
}

struct Preparation;
impl SchedulePreparation for Preparation {
    fn profile_delegatable(&mut self, _: &SessionProfileKey) -> PortResult<bool> {
        panic!("no ordinary profile")
    }
    fn resolve_and_pin(
        &mut self,
        _: RunId,
        _: &[ScheduleContextPointer],
        _: WorkspaceId,
    ) -> Result<Vec<RunContextItem>, CaptureFailure> {
        panic!("no ordinary capture")
    }
    fn resolve_scope(
        &mut self,
        _: RunId,
        scope: &WorkspaceScope,
    ) -> Result<koru_delegate_scope::ResolvedScope, koru_delegate_scope::ScopeRefusal> {
        let WorkspaceScope::BotFolder { folder } = scope else {
            panic!("exact Bot folder")
        };
        let directory = PathBuf::from(folder.as_str());
        Ok(koru_delegate_scope::ResolvedScope {
            identity: koru_delegate_scope::ScopeIdentity::fixture_canonical(&directory),
            directory,
        })
    }
}
struct Gate(SharedProbe);
struct Guard(SharedProbe);
impl SchedulePhaseGuard for Guard {}
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.lock().unwrap().gate = false;
    }
}
impl SchedulePhaseGate for Gate {
    fn enter_until(&self, deadline: Instant) -> PortResult<Box<dyn SchedulePhaseGuard>> {
        let mut p = self.0.lock().unwrap();
        assert!(!p.gate);
        if p.stopped || Instant::now() >= deadline {
            return Err(fail());
        }
        p.gate = true;
        Ok(Box::new(Guard(self.0.clone())))
    }
}
struct Effects(SharedProbe);
impl ScheduleEffects for Effects {
    fn begin_routine(
        &mut self,
        prepared: &RoutinePrepared,
        _: &BotAdmissionBarrier,
        _: &FreshBotPolicyProof,
        _: koru_delegate_scope::ScopeIdentity,
        _: WorkspaceScope,
        _: Instant,
    ) -> PortResult<ScheduleAdmission> {
        let mut p = self.0.lock().unwrap();
        assert!(p.gate);
        assert_eq!(p.handles, 1);
        assert!(p.lease.is_none());
        p.calls.push("begin");
        if p.busy {
            return Ok(ScheduleAdmission::Busy(RunId::mint()));
        }
        p.lease = Some(prepared.ids.run);
        Ok(ScheduleAdmission::Granted)
    }
    fn release_routine(&mut self, run: RunId, _: Instant) -> PortResult<()> {
        let mut p = self.0.lock().unwrap();
        assert!(!p.gate);
        assert_eq!(p.handles, 0);
        assert_eq!(p.lease, Some(run));
        p.calls.push("release");
        if p.fail_release {
            return Err(fail());
        }
        p.lease = None;
        Ok(())
    }
    fn begin(
        &mut self,
        _: RunId,
        _: koru_delegate_scope::ScopeIdentity,
        _: WorkspaceScope,
    ) -> PortResult<ScheduleAdmission> {
        panic!("no ordinary begin")
    }
    fn finish(
        &mut self,
        _: RunId,
        _: SlotClaim,
        _: koru_delegate_scope::ResolvedScope,
        _: WorkspaceScope,
    ) -> PortResult<Option<Started>> {
        panic!("no launch")
    }
    fn abort(&mut self, _: RunId) {
        panic!("no ordinary abort")
    }
    fn reconcile(&mut self, _: SlotClaim, _: WorkspaceScope) -> PortResult<Option<Started>> {
        panic!("no ordinary reconcile")
    }
}

#[derive(Clone)]
struct Ledger(RecordingStore);
impl RoutineLedger for Ledger {
    fn routine_route(
        &self,
        id: ScheduleId,
    ) -> Result<Option<RoutineRouteIdentity>, BotLedgerError> {
        self.0
            .inner
            .lock()
            .unwrap()
            .routine_route(id)
            .map_err(Into::into)
    }
    fn held_routine_page(
        &self,
        after: Option<RoutineHeldCursor>,
    ) -> Result<RoutinePage<RoutineHeldCursor, RoutineRecoveryEntry>, BotLedgerError> {
        self.0.inner.lock().unwrap().held_routine_page(after)
    }
    fn queued_routine_page(
        &self,
        after: Option<RoutineQueuedCursor>,
    ) -> Result<RoutinePage<RoutineQueuedCursor, Option<RoutineRecoveryEntry>>, BotLedgerError>
    {
        self.0.inner.lock().unwrap().queued_routine_page(after)
    }
    fn routine_slots_page(
        &self,
        route: &RoutineRouteIdentity,
        after: Option<RoutineSlotCursor>,
    ) -> Result<RoutinePage<RoutineSlotCursor, RoutineOccurrenceFact>, BotLedgerError> {
        self.0
            .inner
            .lock()
            .unwrap()
            .routine_slots_page(route, after)
    }
    fn record_expired_routine(
        &mut self,
        authority: koru_delegate_document::RoutineExpiryAuthority,
    ) -> Result<(RoutineClaimObservation, Option<RoutineSkippedClaim>), BotLedgerError> {
        RoutineLedger::record_expired_routine(&mut *self.0.inner.lock().unwrap(), authority)
    }
    fn claim_bot_routine(
        &mut self,
        authority: ScheduleClaimAuthority,
        admission: &BotAdmissionBarrier,
        proof: &FreshBotPolicyProof,
    ) -> Result<(RoutineClaimObservation, Option<RoutineSkippedClaim>), BotLedgerError> {
        RoutineLedger::claim_bot_routine(
            &mut *self.0.inner.lock().unwrap(),
            authority,
            admission,
            proof,
        )
    }
    fn read_routine_claim(
        &self,
        slot: &RoutineSlot,
    ) -> Result<RoutineClaimObservation, BotLedgerError> {
        self.0.inner.lock().unwrap().read_routine_claim(slot)
    }
    fn acknowledge_routine_link(
        &mut self,
        _: &RoutineLinkAcknowledgement,
    ) -> Result<RoutineClaimRecord, BotLedgerError> {
        panic!("scheduler never acknowledges")
    }
}
struct Bridge {
    probe: SharedProbe,
    prepared: RoutinePrepared,
    proof: FreshBotPolicyProof,
    ledger: RecordingStore,
}
impl Bridge {
    fn call(&self, name: &'static str) -> RoutineBridgeResult<()> {
        let mut p = self.probe.lock().unwrap();
        assert_eq!(p.handles, 0);
        assert!(!p.gate);
        assert!(p.lease.is_none());
        assert!(
            self.ledger.inner.try_lock().is_ok(),
            "HTTP outside SQLite mutex"
        );
        p.calls.push(name);
        if p.unavailable {
            Err(RoutineBridgeFailure::NotSent(fail()))
        } else {
            Ok(())
        }
    }
}
impl RoutineBridge for Bridge {
    fn stopped(&self) -> bool {
        self.probe.lock().unwrap().stopped
    }
    fn read_preparation(
        &mut self,
        slot: &RoutineSlot,
        semantic: RoutineDigest32,
        _: Instant,
    ) -> RoutineBridgeResult<RoutinePreparationObservation> {
        self.call("read_preparation")?;
        assert_eq!(*slot, self.prepared.slot);
        assert_eq!(semantic, self.prepared.semantic_hash);
        if self.probe.lock().unwrap().missing {
            Ok(RoutinePreparationObservation::Missing)
        } else {
            let mut prepared = self.prepared.clone();
            if self.probe.lock().unwrap().bad_prepared {
                prepared.definition.digest = RoutineDigest32::from_bytes([0; 32]);
            }
            Ok(RoutinePreparationObservation::Prepared(prepared))
        }
    }
    fn prepare(
        &mut self,
        _: &RoutineSlot,
        _: WorkspaceId,
        _: &ScheduleDefinition,
        _: Option<HandoffId>,
        _: Instant,
    ) -> RoutineBridgeResult<RoutinePreparedReply> {
        self.call("prepare")?;
        let mut p = self.probe.lock().unwrap();
        p.missing = false;
        if p.unknown_prepare {
            p.unknown_prepare = false;
            return Err(RoutineBridgeFailure::Unknown(fail()));
        }
        Ok(RoutinePreparedReply::Prepared(self.prepared.clone()))
    }
    fn synchronize_preclaim(
        &mut self,
        _: &RoutinePrepared,
        _: Instant,
    ) -> RoutineBridgeResult<(BotAdmissionBarrier, FreshBotPolicyProof)> {
        self.call("preclaim")?;
        if self.probe.lock().unwrap().refuse_preclaim {
            return Err(RoutineBridgeFailure::Refused(fail()));
        }
        Ok((admission(&self.prepared), self.proof.clone()))
    }
    fn read_link(
        &mut self,
        claim: &RoutineClaimIdentity,
        _: Instant,
    ) -> RoutineBridgeResult<Option<RoutineLinkOutcome>> {
        self.probe.lock().unwrap().link_runs.push(claim.ids.run);
        self.call("read_link")?;
        Ok(Some(RoutineLinkOutcome::Linked {}))
    }
    fn finalize(
        &mut self,
        claim: &RoutineClaimIdentity,
        _: Instant,
    ) -> RoutineBridgeResult<RoutineLinkOutcome> {
        self.call("finalize")?;
        if self.probe.lock().unwrap().lose_ack {
            return Ok(RoutineLinkOutcome::Linked {});
        }
        self.ledger
            .inner
            .lock()
            .unwrap()
            .acknowledge_routine_link(&RoutineLinkAcknowledgement {
                claim: claim.clone(),
                outcome: RoutineLinkOutcome::Linked {},
            })
            .unwrap();
        Ok(RoutineLinkOutcome::Linked {})
    }
}

struct Composition {
    h: Harness,
    docs: Documents,
    prepared: RoutinePrepared,
    coordinator: ScheduleCoordinator,
    effects: Effects,
    probe: SharedProbe,
}
impl Composition {
    fn new() -> Self {
        Self::configured(|_, _| {})
    }
    fn configured(configure: impl FnOnce(&mut Documents, &RecordingStore)) -> Self {
        let (h, doc, mut prepared, proof) = fixture();
        let mut header = doc.header().unwrap();
        header.definition.revision = 1;
        header.definition.effective_at_ms = 0;
        header.definition.lifecycle = ScheduleLifecycle::Paused {
            reason: SchedulePauseReason::Manual,
        };
        header.definition.template.scope = prepared.submission.request.scope.clone();
        header.definition.template.grants = [prepared.submission.workspace].into_iter().collect();
        let doc = ScheduleDocument::create(&header).unwrap();
        doc.set_lifecycle(prepared.slot.owner.device, 1, ScheduleLifecycle::Active, 1)
            .unwrap();
        prepared.definition.digest = routine_definition_digest(
            &doc.header().unwrap().definition,
            prepared.submission.workspace,
            None,
        )
        .unwrap();
        prepared
            .submission
            .provenance
            .routine
            .as_mut()
            .unwrap()
            .definition = prepared.definition;
        h.schedule_clock.store(61_000, Ordering::SeqCst);
        prepared.slot.occurrence_ms = 60_000;
        prepared
            .submission
            .provenance
            .routine
            .as_mut()
            .unwrap()
            .occurrence_ms = 60_000;
        if let RunOrigin::Scheduled { occurrence_ms, .. } = &mut prepared.submission.request.origin
        {
            *occurrence_ms = 60_000;
        }
        prepared.semantic_hash = routine_semantic_digest(
            &prepared.slot,
            &doc.header().unwrap().definition,
            prepared.submission.workspace,
            None,
        )
        .unwrap();
        let probe = Arc::new(Mutex::new(Probe::default()));
        let mut docs = Documents {
            workspace: prepared.submission.workspace,
            document: DocumentId::from_uuid(prepared.slot.schedule.as_uuid()),
            snapshot: Arc::new(Mutex::new(doc.export_snapshot().unwrap())),
            probe: probe.clone(),
            alias: false,
            others: vec![],
        };
        configure(&mut docs, &h.schedule_store);
        let coordinator = ScheduleCoordinator::new(
            Box::new(docs.clone()),
            Box::new(h.schedule_store.clone()),
            Box::new(Preparation),
            Box::new(SharedScheduleClock(h.schedule_clock.clone())),
            prepared.slot.owner.device,
        )
        .with_routines(
            Box::new(Ledger(h.schedule_store.clone())),
            Box::new(Bridge {
                probe: probe.clone(),
                prepared: prepared.clone(),
                proof,
                ledger: h.schedule_store.clone(),
            }),
            Box::new(Gate(probe.clone())),
        );
        Self {
            h,
            docs,
            prepared,
            coordinator,
            effects: Effects(probe.clone()),
            probe,
        }
    }
    fn select(&mut self) -> RoutineWork {
        let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
        self.probe.lock().unwrap().gate = true;
        let report = self
            .coordinator
            .evaluate(
                &mut self.effects,
                self.h.schedule_clock.load(Ordering::SeqCst),
                &zone,
            )
            .unwrap();
        self.probe.lock().unwrap().gate = false;
        report.routine_work.expect("selected Routine")
    }
    fn run(&mut self) -> PortResult<ScheduleEvaluation> {
        let work = self.select();
        self.coordinator.evaluate_routine_work(
            work,
            &mut self.effects,
            Instant::now() + Duration::from_secs(5),
        )
    }
    fn observation(&self) -> RoutineClaimObservation {
        self.h
            .schedule_store
            .inner
            .lock()
            .unwrap()
            .read_routine_claim(&self.prepared.slot)
            .unwrap()
    }
    fn no_launch(&self) {
        let mut db = self.h.schedule_store.inner.lock().unwrap();
        assert!(db.pending_intents().unwrap().is_empty());
        assert!(db.queued_bots(0, 16).unwrap().is_empty());
        assert!(db.reserve_bot(self.prepared.ids.run).is_err());
        assert!(self.h.sessions.lock().unwrap().calls.is_empty());
    }
}
