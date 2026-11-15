// SPDX-License-Identifier: MPL-2.0
//! Pure mutation boundaries with real in-memory route storage and live Loro.
use super::*;
use koru_delegate_core::*;
use koru_delegate_sqlite::{
    BotLedgerError, RoutineHeldCursor, RoutineOccurrenceFact, RoutinePage, RoutineQueuedCursor,
    RoutineRecoveryEntry, RoutineSlotCursor, SlotClaim,
};
use std::time::Instant;

struct Routes {
    db: RunsDb,
    unavailable: bool,
}
impl RoutineLedger for Routes {
    fn routine_route(
        &self,
        id: ScheduleId,
    ) -> Result<Option<RoutineRouteIdentity>, BotLedgerError> {
        if self.unavailable {
            return Err(BotLedgerError::Invalid);
        }
        self.db.routine_route(id).map_err(Into::into)
    }
    fn routine_firing(&self, s: &RoutineSlot) -> Result<Option<SlotClaim>, BotLedgerError> {
        self.db.routine_firing(s)
    }
    fn held_routine_page(
        &self,
        a: Option<RoutineHeldCursor>,
    ) -> Result<RoutinePage<RoutineHeldCursor, RoutineRecoveryEntry>, BotLedgerError> {
        self.db.held_routine_page(a)
    }
    fn queued_routine_page(
        &self,
        a: Option<RoutineQueuedCursor>,
    ) -> Result<RoutinePage<RoutineQueuedCursor, Option<RoutineRecoveryEntry>>, BotLedgerError>
    {
        self.db.queued_routine_page(a)
    }
    fn routine_slots_page(
        &self,
        r: &RoutineRouteIdentity,
        a: Option<RoutineSlotCursor>,
    ) -> Result<RoutinePage<RoutineSlotCursor, RoutineOccurrenceFact>, BotLedgerError> {
        self.db.routine_slots_page(r, a)
    }
    fn record_expired_routine(
        &mut self,
        a: koru_delegate_document::RoutineExpiryAuthority,
    ) -> Result<(RoutineClaimObservation, Option<RoutineSkippedClaim>), BotLedgerError> {
        RoutineLedger::record_expired_routine(&mut self.db, a)
    }
    fn claim_bot_routine(
        &mut self,
        a: koru_delegate_document::ScheduleClaimAuthority,
        b: &BotAdmissionBarrier,
        p: &FreshBotPolicyProof,
    ) -> Result<(RoutineClaimObservation, Option<RoutineSkippedClaim>), BotLedgerError> {
        RoutineLedger::claim_bot_routine(&mut self.db, a, b, p)
    }
    fn read_routine_claim(
        &self,
        s: &RoutineSlot,
    ) -> Result<RoutineClaimObservation, BotLedgerError> {
        self.db.read_routine_claim(s)
    }
    fn acknowledge_routine_link(
        &mut self,
        a: &RoutineLinkAcknowledgement,
    ) -> Result<RoutineClaimRecord, BotLedgerError> {
        self.db.acknowledge_routine_link(a)
    }
}
struct NoBridge;
impl RoutineBridge for NoBridge {
    fn stopped(&self) -> bool {
        false
    }
    fn prepare(
        &mut self,
        _: &RoutineSlot,
        _: WorkspaceId,
        _: &koru_delegate_schedule::ScheduleDefinition,
        _: Option<HandoffId>,
        _: Instant,
    ) -> RoutineBridgeResult<RoutinePreparedReply> {
        panic!("mutation must not prepare execution")
    }
    fn read_preparation(
        &mut self,
        _: &RoutineSlot,
        _: RoutineDigest32,
        _: Instant,
    ) -> RoutineBridgeResult<RoutinePreparationObservation> {
        panic!("mutation must not recover execution")
    }
    fn synchronize_preclaim(
        &mut self,
        _: &RoutinePrepared,
        _: Instant,
    ) -> RoutineBridgeResult<(BotAdmissionBarrier, FreshBotPolicyProof)> {
        panic!("mutation must not install policy")
    }
    fn finalize(
        &mut self,
        _: &RoutineClaimIdentity,
        _: Instant,
    ) -> RoutineBridgeResult<RoutineLinkOutcome> {
        panic!("mutation must not finalize execution")
    }
    fn read_link(
        &mut self,
        _: &RoutineClaimIdentity,
        _: Instant,
    ) -> RoutineBridgeResult<Option<RoutineLinkOutcome>> {
        panic!("mutation must not read execution")
    }
}
struct NoGate;
impl SchedulePhaseGate for NoGate {
    fn enter_until(&self, _: Instant) -> PortResult<Box<dyn SchedulePhaseGuard>> {
        panic!("mutation must not enter execution gate")
    }
}
pub(in crate::tests::schedules) fn with_routes(
    coordinator: ScheduleCoordinator,
    header: Option<&koru_delegate_schedule::ScheduleHeader>,
    unavailable: bool,
) -> ScheduleCoordinator {
    let mut db = RunsDb::open_with_clock(
        std::path::Path::new(":memory:"),
        Box::new(SharedScheduleClock(Arc::new(
            std::sync::atomic::AtomicI64::new(0),
        ))),
    )
    .unwrap();
    if let Some(h) = header {
        let a = h.definition.routine.as_ref().unwrap();
        db.register_routine_route(&RoutineRouteIdentity {
            owner: a.owner,
            bot: a.bot,
            workspace: a.home,
            schedule: h.schedule,
            creation_request: a.creation_request,
            created_at_ms: h.created_at_ms,
            seed_hash: RoutineDigest32::from_bytes([7; 32]),
        })
        .unwrap();
    }
    coordinator.with_routines(
        Box::new(Routes { db, unavailable }),
        Box::new(NoBridge),
        Box::new(NoGate),
    )
}

fn change(document: &ScheduleDocument, kind: u8) {
    let root = document.doc().get_map("schedule");
    let mut header = document.header().unwrap();
    match kind {
        0 => {
            header.definition.routine = None;
            root.insert("schema", 1).unwrap();
        }
        1 => {
            root.insert("id", ScheduleId::mint().to_storage()).unwrap();
        }
        2 => {
            header.definition.routine.as_mut().unwrap().bot = BotId::mint();
        }
        _ => {
            root.insert("created_at_ms", header.created_at_ms - 1)
                .unwrap();
        }
    }
    root.insert(
        "definition",
        serde_json::to_string(&header.definition).unwrap(),
    )
    .unwrap();
    document.doc().commit();
}
fn mutate(schedules: &MemorySchedules, kind: u8) {
    let doc = schedules.schedule();
    change(&doc, kind);
    *schedules.snapshot.lock().unwrap() = doc.export_snapshot().unwrap();
}

/// Exercise every existing mutation through the real coordinator entry point.
fn invoke(
    coordinator: &mut ScheduleCoordinator,
    inspect: &MemorySchedules,
    action: u8,
) -> PortResult<ScheduleListing> {
    let header = inspect.schedule().header()?;
    let name = inspect.schedule().name()?;
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    match action {
        0 => coordinator.rename_schedule(
            workspace(),
            header.schedule,
            &name,
            &ScheduleName::new("Changed").unwrap(),
            10,
            &zone,
        ),
        1 => coordinator.edit_schedule(
            workspace(),
            header.schedule,
            header.definition.revision,
            &name,
            &name,
            header.definition.template,
            header.definition.recurrence,
            10,
            &zone,
        ),
        2 => coordinator.resume_schedule(
            workspace(),
            header.schedule,
            header.definition.revision,
            10,
            &zone,
        ),
        3 => coordinator.pause_schedule(
            workspace(),
            header.schedule,
            header.definition.revision,
            10,
            &zone,
        ),
        _ => coordinator.retire_schedule(workspace(), header.schedule, 10, &zone),
    }
}

#[test]
fn known_route_stripped_association_or_header_cannot_enter_any_ordinary_mutation() {
    for action in 0..5 {
        for kind in 0..3 {
            let owner = DeviceId::from_uuid(v4(0xd3));
            let schedules = associated_schedules(owner, action != 2);
            let inspect = schedules.clone();
            let original = inspect.schedule().header().unwrap();
            let (coordinator, _) = associated_coordinator(schedules, owner);
            let (events, script) = recording_schedule_events();
            let mut coordinator =
                with_routes(coordinator, Some(&original), false).with_events(events);
            mutate(&inspect, kind);
            let before = inspect.schedule().version();
            assert!(
                invoke(&mut coordinator, &inspect, action).is_err(),
                "action {action}, kind {kind}"
            );
            assert_eq!(inspect.schedule().version(), before);
            assert!(script.lock().unwrap().calls.is_empty());
        }
    }
}

#[test]
fn known_route_alias_and_orphan_header_refuse_before_events_or_writes() {
    for action in 0..5 {
        for orphan in [false, true] {
            let owner = DeviceId::from_uuid(v4(0xd3));
            let mut schedules = associated_schedules(owner, action != 2);
            let header = schedules.schedule().header().unwrap();
            if !orphan {
                schedules.document = DocumentId::from_uuid(v4(0xee));
            }
            let inspect = schedules.clone();
            let before = inspect.schedule().version();
            let (coordinator, _) = associated_coordinator(schedules, owner);
            let (events, script) = recording_schedule_events();
            let mut coordinator = with_routes(
                coordinator,
                if orphan { None } else { Some(&header) },
                false,
            )
            .with_events(events);
            assert!(invoke(&mut coordinator, &inspect, action).is_err());
            assert_eq!(inspect.schedule().version(), before);
            assert!(script.lock().unwrap().calls.is_empty());
        }
    }
}

#[test]
fn missing_engine_or_unavailable_route_ledger_never_implies_ordinary_absence() {
    for action in 0..5 {
        for unavailable in [false, true] {
            let owner = DeviceId::from_uuid(v4(0xd3));
            let mut header = active_schedule(owner);
            if action == 2 {
                header.definition.lifecycle = ScheduleLifecycle::Paused {
                    reason: SchedulePauseReason::Manual,
                };
            }
            let schedules = MemorySchedules::one(&header);
            let inspect = schedules.clone();
            let before = inspect.schedule().version();
            let (coordinator, _) = associated_coordinator(schedules, owner);
            let (events, script) = recording_schedule_events();
            let mut coordinator = if unavailable {
                with_routes(coordinator, None, true)
            } else {
                coordinator
            }
            .with_events(events);
            assert!(invoke(&mut coordinator, &inspect, action).is_err());
            assert_eq!(inspect.schedule().version(), before);
            assert!(script.lock().unwrap().calls.is_empty());
        }
    }
}

struct Substitute {
    inner: MemorySchedules,
    stage: u8,
    partial: u8,
}
struct SubstituteHandle {
    inner: Box<dyn ScheduleDocumentHandle>,
    stage: u8,
}
impl ScheduleDocumentHandle for SubstituteHandle {
    fn document(&self) -> &ScheduleDocument {
        self.inner.document()
    }
    fn expired_routine_authority(
        &self,
        _: DeviceId,
        _: koru_delegate_document::RoutineExpiryPlan,
    ) -> PortResult<Option<koru_delegate_document::RoutineExpiryAuthority>> {
        panic!("no expiry execution")
    }
    fn absorb(&mut self) -> PortResult<bool> {
        let changed = self.inner.absorb()?;
        if self.stage == 1 {
            change(self.inner.document(), 3);
        }
        Ok(changed)
    }
    fn commit(
        &mut self,
        write: &mut dyn FnMut(
            &ScheduleDocument,
        ) -> Result<(), koru_delegate_document::ScheduleDocumentError>,
    ) -> PortResult<()> {
        if self.stage == 2 {
            change(self.inner.document(), 3);
        }
        let before = self.inner.document().version();
        let result = self.inner.commit(write);
        if result.is_err() {
            assert_eq!(
                self.inner.document().version(),
                before,
                "refusal cannot add an operation after the injected substitution"
            );
        }
        result
    }
    fn claim_authority(
        &self,
        _: DeviceId,
        _: ScheduleClaimPlan,
    ) -> PortResult<Option<koru_delegate_document::ScheduleClaimAuthority>> {
        panic!("no execution")
    }
}
impl ScheduleDocuments for Substitute {
    fn create(
        &mut self,
        _: WorkspaceId,
        _: &koru_delegate_schedule::ScheduleHeader,
    ) -> PortResult<DocumentId> {
        panic!("no creation")
    }
    fn open(
        &mut self,
        w: WorkspaceId,
        d: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        let inner = self.inner.open(w, d)?;
        Ok(Box::new(SubstituteHandle {
            inner,
            stage: self.stage,
        }))
    }
    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        let mut catalog = self.inner.discover_active()?;
        if self.partial == 2 {
            catalog.failed_workspaces.push(workspace());
        }
        Ok(catalog)
    }
    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        let mut catalog = self.inner.discover_identities()?;
        if self.partial == 1 {
            catalog
                .unavailable_documents
                .push((workspace(), DocumentId::from_uuid(v4(0xef))));
        }
        Ok(catalog)
    }
    fn watch_changes(
        &mut self,
        _: &ScheduleWatchTargets,
        _: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        panic!("no watch")
    }
}

#[test]
fn partial_identity_catalog_refuses_all_mutations_without_event_preparation() {
    for action in 0..5 {
        for partial in [1, 2] {
            let owner = DeviceId::from_uuid(v4(0xd3));
            let schedules = MemorySchedules::one(&active_schedule(owner));
            let inspect = schedules.clone();
            let before = inspect.schedule().version();
            let (events, script) = recording_schedule_events();
            let mut coordinator = listing_coordinator(
                Substitute {
                    inner: schedules,
                    stage: 0,
                    partial,
                },
                owner,
            )
            .with_events(events);
            assert!(invoke(&mut coordinator, &inspect, action).is_err());
            assert_eq!(inspect.schedule().version(), before);
            assert!(script.lock().unwrap().calls.is_empty());
        }
    }
}

#[test]
fn absorbed_or_consumed_handle_substitution_refuses_all_mutations() {
    for action in 0..5 {
        for stage in [1, 2] {
            let owner = DeviceId::from_uuid(v4(0xd3));
            let mut header = active_schedule(owner);
            if action == 2 {
                header.definition.lifecycle = ScheduleLifecycle::Paused {
                    reason: SchedulePauseReason::Manual,
                };
            }
            let schedules = MemorySchedules::one(&header);
            let inspect = schedules.clone();
            let before = inspect.schedule().version();
            let (events, script) = recording_schedule_events();
            let mut coordinator = listing_coordinator(
                Substitute {
                    inner: schedules,
                    stage,
                    partial: 0,
                },
                owner,
            )
            .with_events(events);
            assert!(
                invoke(&mut coordinator, &inspect, action).is_err(),
                "action {action}, stage {stage}"
            );
            assert_eq!(inspect.schedule().version(), before);
            let calls = &script.lock().unwrap().calls;
            if stage == 1 || action == 0 || action == 4 {
                assert!(calls.is_empty());
            } else {
                assert_eq!(calls.len(), 1);
                assert!(matches!(calls[0], ScheduleEventCall::PrepareDefinition(..)));
            }
            assert!(
                !calls
                    .iter()
                    .any(|c| matches!(c, ScheduleEventCall::Ready(_)))
            );
        }
    }
}

#[test]
fn valid_known_routine_rename_pause_and_irreversible_retire_keep_identity() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let schedules = associated_schedules(owner, true);
    let inspect = schedules.clone();
    let header = inspect.schedule().header().unwrap();
    let (coordinator, _) = associated_coordinator(schedules, owner);
    let (events, script) = recording_schedule_events();
    let mut coordinator = with_routes(coordinator, Some(&header), false).with_events(events);
    let renamed = invoke(&mut coordinator, &inspect, 0).unwrap();
    assert_eq!(renamed.header, header);
    let paused = invoke(&mut coordinator, &inspect, 3).unwrap();
    assert_eq!(paused.header.definition.routine, header.definition.routine);
    assert!(matches!(
        paused.header.definition.lifecycle,
        ScheduleLifecycle::Paused { .. }
    ));
    let retired = invoke(&mut coordinator, &inspect, 4).unwrap();
    assert_eq!(retired.state, ScheduleState::Retired);
    assert_eq!(retired.header.definition.routine, header.definition.routine);
    assert_eq!(
        inspect.schedule().name().unwrap(),
        ScheduleName::new("Changed").unwrap()
    );
    assert_eq!(script.lock().unwrap().calls.len(), 2);
}
