// SPDX-License-Identifier: MPL-2.0

use super::*;
pub(super) mod mutations;

struct NoOrdinaryPreparation;

impl SchedulePreparation for NoOrdinaryPreparation {
    fn profile_delegatable(&mut self, _: &SessionProfileKey) -> PortResult<bool> {
        panic!("a Routine must not prepare an ordinary provider")
    }
    fn resolve_scope(
        &mut self,
        _: RunId,
        _: &WorkspaceScope,
    ) -> Result<koru_delegate_scope::ResolvedScope, koru_delegate_scope::ScopeRefusal> {
        panic!("a Routine must not prepare an ordinary scope")
    }
    fn resolve_and_pin(
        &mut self,
        _: RunId,
        _: &[koru_delegate_schedule::ScheduleContextPointer],
        _: WorkspaceId,
    ) -> Result<Vec<RunContextItem>, CaptureFailure> {
        panic!("a Routine must not capture ordinary task context")
    }
}

fn associated_schedules(owner: DeviceId, active: bool) -> MemorySchedules {
    use koru_delegate_core::{BotId, BotOwner, BotRequestId};
    let mut header = active_schedule(owner);
    header.definition.routine = Some(
        koru_delegate_schedule::RoutineAssociation::new(
            BotOwner {
                user_scope: serde_json::from_value(serde_json::json!(v4(0xe1).to_string()))
                    .unwrap(),
                device: owner,
            },
            BotId::from_uuid(v4(0xe2)),
            workspace(),
            BotRequestId::from_uuid(v4(0xe3)),
        )
        .unwrap(),
    );
    header.definition.lifecycle = ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::Manual,
    };
    let mut schedules = MemorySchedules::one(&header);
    schedules.document = DocumentId::from_uuid(header.schedule.as_uuid());
    if active {
        // An externally authored Active associated Schedule may be imported.
        // The ordinary coordinator must refuse it even though it is readable.
        let doc = schedules.schedule();
        doc.set_lifecycle(owner, 1, ScheduleLifecycle::Active, 2)
            .unwrap();
        *schedules.snapshot.lock().unwrap() = doc.export_snapshot().unwrap();
    }
    schedules
}

fn associated_coordinator(
    schedules: MemorySchedules,
    owner: DeviceId,
) -> (ScheduleCoordinator, RecordingStore) {
    let ledger = RecordingStore {
        inner: Arc::new(Mutex::new(
            RunsDb::open_with_clock(
                std::path::Path::new(":memory:"),
                Box::new(SharedScheduleClock(Arc::new(
                    std::sync::atomic::AtomicI64::new(0),
                ))),
            )
            .unwrap(),
        )),
        script: Arc::new(Mutex::new(LedgerScript::default())),
    };
    let coordinator = ScheduleCoordinator::new(
        Box::new(schedules),
        Box::new(ledger.clone()),
        Box::new(NoOrdinaryPreparation),
        Box::new(EvaluationNowClock),
        owner,
    );
    (coordinator, ledger)
}

#[test]
fn routine_resume_refuses_before_events_or_document_mutation() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let schedules = associated_schedules(owner, false);
    let inspect = schedules.clone();
    let header = inspect.schedule().header().unwrap();
    let before = inspect.schedule().version();
    let (events, script) = recording_schedule_events();
    let (coordinator, _) = associated_coordinator(schedules, owner);
    let coordinator = mutations::with_routes(coordinator, Some(&header), false);
    let mut coordinator = coordinator.with_events(events);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    let error = coordinator
        .resume_schedule(workspace(), header.schedule, 1, 2, &zone)
        .unwrap_err();
    assert_eq!(error.code, "bot_routine_unavailable");
    assert_eq!(inspect.schedule().version(), before);
    assert!(script.lock().unwrap().calls.is_empty());
}

#[test]
fn routine_edit_refuses_before_events_or_document_mutation() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let schedules = associated_schedules(owner, false);
    let inspect = schedules.clone();
    let header = inspect.schedule().header().unwrap();
    let name = inspect.schedule().name().unwrap();
    let before = inspect.schedule().version();
    let (events, script) = recording_schedule_events();
    let (coordinator, _) = associated_coordinator(schedules, owner);
    let coordinator = mutations::with_routes(coordinator, Some(&header), false);
    let mut coordinator = coordinator.with_events(events);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    let error = coordinator
        .edit_schedule(
            workspace(),
            header.schedule,
            1,
            &name,
            &name,
            header.definition.template,
            header.definition.recurrence,
            2,
            &zone,
        )
        .unwrap_err();
    assert_eq!(error.code, "bot_routine_unavailable");
    assert_eq!(inspect.schedule().version(), before);
    assert!(script.lock().unwrap().calls.is_empty());
}

#[test]
fn routine_pause_event_preserves_the_persisted_association() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let schedules = associated_schedules(owner, true);
    let inspect = schedules.clone();
    let header = inspect.schedule().header().unwrap();
    let (events, script) = recording_schedule_events();
    let (coordinator, _) = associated_coordinator(schedules, owner);
    let coordinator = mutations::with_routes(coordinator, Some(&header), false);
    let mut coordinator = coordinator.with_events(events);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    let paused = coordinator
        .pause_schedule(workspace(), header.schedule, 2, 3, &zone)
        .unwrap();
    assert_eq!(paused.header.definition.routine, header.definition.routine);
    assert_eq!(
        paused.header.definition,
        inspect.schedule().header().unwrap().definition
    );
    assert_eq!(
        script.lock().unwrap().definitions,
        vec![paused.header.definition]
    );
}

#[test]
fn routine_evaluation_refuses_before_capture_effects_claims_or_events() {
    let owner = DeviceId::from_uuid(v4(0xd3));
    let schedules = associated_schedules(owner, true);
    let inspect = schedules.clone();
    let header = inspect.schedule().header().unwrap();
    let before = inspect.schedule().version();
    let (events, script) = recording_schedule_events();
    let (coordinator, ledger) = associated_coordinator(schedules, owner);
    let mut coordinator = coordinator.with_events(events);
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    let mut effects = TrackingEffects::default();
    let result = coordinator
        .evaluate(&mut effects, 3_600_001, &zone)
        .unwrap();
    assert_eq!(result.failures, 1);
    assert_eq!(result.evaluated, 0);
    assert!(effects.begun.is_empty());
    assert!(effects.finished.is_empty());
    assert!(effects.reconciled.is_empty());
    assert!(ledger.claims_of(header.schedule).unwrap().is_empty());
    assert!(script.lock().unwrap().calls.is_empty());
    assert_eq!(inspect.schedule().version(), before);
}
