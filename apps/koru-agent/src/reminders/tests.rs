// SPDX-License-Identifier: MPL-2.0

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use koru_native_notifications::{
    Activation, Authorization, Capability, DeliverySupport, Failure, FailureCode, NativeHandle,
    NativeNotifications, RegistrationId, Schedule, Sound,
};
use koru_store_client::{ReminderSnapshot, ReminderSnapshotRow, ReminderTarget};
use koru_store_core::ids::{
    ActionId, DocumentId, ProfileId, ReminderId, RuleRevisionId, WorkspaceId,
};
use koru_store_core::personal_attributes::EntityRef;
use koru_store_core::reminders::{
    ActionKind, DueRelation, OccurrenceAction, OccurrenceMoment, OccurrenceSource, OneTimeSchedule,
    ReminderActions, ReminderRule, ReminderSettings, RuleStatus, StoredReminder, TargetState,
    ToneId, reduce,
};
use koru_store_core::temporal::{CalendarDate, FloatingDateTime, TemporalValue, ZonedDateTime};

use super::*;

#[derive(Default)]
struct StoreState {
    profiles: Vec<ProfileId>,
    locales: HashMap<ProfileId, String>,
    snapshots: HashMap<ProfileId, ReminderSnapshot>,
    deleted_targets: Vec<(ProfileId, EntityRef)>,
    fail_snapshot: bool,
    audit: Arc<Mutex<Vec<&'static str>>>,
    operation_threads: Vec<std::thread::ThreadId>,
}

#[derive(Clone)]
struct FakeStore(Arc<Mutex<StoreState>>);

impl ReminderStore for FakeStore {
    fn profiles(&mut self) -> Result<Vec<ProfileId>, SchedulerFailure> {
        let mut state = self.0.lock().unwrap();
        state.operation_threads.push(std::thread::current().id());
        state.audit.lock().unwrap().push("profiles");
        Ok(state.profiles.clone())
    }

    fn profile_locale(&mut self, profile: ProfileId) -> Result<String, SchedulerFailure> {
        self.0
            .lock()
            .unwrap()
            .locales
            .get(&profile)
            .cloned()
            .ok_or_else(|| SchedulerFailure::new(FailureCategory::Store))
    }

    fn snapshot(&mut self, profile: ProfileId) -> Result<ReminderSnapshot, SchedulerFailure> {
        let mut state = self.0.lock().unwrap();
        state.operation_threads.push(std::thread::current().id());
        state.audit.lock().unwrap().push("snapshot");
        if state.fail_snapshot {
            state.fail_snapshot = false;
            return Err(SchedulerFailure::new(FailureCategory::Store));
        }
        state
            .snapshots
            .get(&profile)
            .cloned()
            .ok_or_else(|| SchedulerFailure::new(FailureCategory::Store))
    }

    fn handle(
        &mut self,
        profile: ProfileId,
        reminder: ReminderId,
        action: OccurrenceAction,
    ) -> Result<(), SchedulerFailure> {
        let mut state = self.0.lock().unwrap();
        state.operation_threads.push(std::thread::current().id());
        let row = state
            .snapshots
            .get_mut(&profile)
            .and_then(|snapshot| {
                snapshot
                    .reminders
                    .iter_mut()
                    .find(|row| row.stored.rule.id == reminder)
            })
            .ok_or_else(|| SchedulerFailure::new(FailureCategory::Store))?;
        row.stored.actions.push(action);
        refresh(row);
        Ok(())
    }

    fn delete_target(
        &mut self,
        profile: ProfileId,
        target: &EntityRef,
    ) -> Result<(), SchedulerFailure> {
        let mut state = self.0.lock().unwrap();
        state.operation_threads.push(std::thread::current().id());
        state.deleted_targets.push((profile, target.clone()));
        Ok(())
    }
}

#[derive(Default)]
struct NativeState {
    scheduled: Vec<Schedule>,
    withdrawn: Vec<RegistrationId>,
    fail_schedule: bool,
    fail_withdraw: bool,
    namespace_resets: usize,
    static_unsupported: bool,
    operation_threads: Vec<std::thread::ThreadId>,
    control_calls: Vec<&'static str>,
    fail_control: Option<FailureCode>,
    authorization: Option<Authorization>,
    custom_sound_unsupported: bool,
    test_notifications: Vec<(
        koru_native_notifications::Content,
        koru_native_notifications::Sound,
    )>,
    sound_failures: Vec<(Sound, FailureCode)>,
}

#[derive(Clone)]
struct FakeNative(Arc<Mutex<NativeState>>);

impl NativeNotifications for FakeNative {
    fn delivery_support(&self) -> DeliverySupport {
        let unsupported = self.0.lock().unwrap().static_unsupported;
        DeliverySupport {
            actions: !unsupported,
            stable_replacement: true,
            withdrawal: true,
            custom_sound: true,
            activation_while_stopped: !unsupported,
        }
    }

    fn capability(&mut self) -> Result<Capability, Failure> {
        let mut state = self.0.lock().unwrap();
        state.operation_threads.push(std::thread::current().id());
        state.control_calls.push("inspect");
        if let Some(code) = state.fail_control.take() {
            return Err(Failure {
                code,
                detail: "armed control failure".into(),
            });
        }
        Ok(Capability {
            authorization: state.authorization.unwrap_or(Authorization::Allowed),
            actions: true,
            stable_replacement: true,
            withdrawal: true,
            custom_sound: !state.custom_sound_unsupported,
            activation_while_stopped: true,
        })
    }

    fn schedule(&mut self, request: Schedule) -> Result<NativeHandle, Failure> {
        let mut state = self.0.lock().unwrap();
        state.operation_threads.push(std::thread::current().id());
        if let Some((_, code)) = state
            .sound_failures
            .iter()
            .find(|(sound, _)| sound == &request.sound)
        {
            return Err(Failure {
                code: *code,
                detail: "armed sound failure".into(),
            });
        }
        if state.fail_schedule {
            state.fail_schedule = false;
            return Err(Failure {
                code: FailureCode::Platform,
                detail: "armed fake failure".into(),
            });
        }
        let handle = NativeHandle(format!("handle-{}", state.scheduled.len()));
        state.scheduled.push(request);
        Ok(handle)
    }

    fn withdraw(&mut self, registration: &RegistrationId) -> Result<(), Failure> {
        let mut state = self.0.lock().unwrap();
        state.operation_threads.push(std::thread::current().id());
        if state.fail_withdraw {
            state.fail_withdraw = false;
            return Err(Failure {
                code: FailureCode::Platform,
                detail: "armed fake failure".into(),
            });
        }
        state.withdrawn.push(registration.clone());
        Ok(())
    }

    fn reset_namespace(&mut self) -> Result<(), Failure> {
        let mut state = self.0.lock().unwrap();
        state.namespace_resets += 1;
        state.scheduled.clear();
        Ok(())
    }

    fn open_settings(&mut self) -> Result<(), Failure> {
        let mut state = self.0.lock().unwrap();
        state.operation_threads.push(std::thread::current().id());
        state.control_calls.push("open_settings");
        if let Some(code) = state.fail_control.take() {
            return Err(Failure {
                code,
                detail: "armed control failure".into(),
            });
        }
        Ok(())
    }

    fn request_authorization(&mut self) -> Result<Authorization, Failure> {
        let mut state = self.0.lock().unwrap();
        state.operation_threads.push(std::thread::current().id());
        state.control_calls.push("request_authorization");
        if let Some(code) = state.fail_control.take() {
            return Err(Failure {
                code,
                detail: "armed control failure".into(),
            });
        }
        Ok(state.authorization.unwrap_or(Authorization::Allowed))
    }

    fn send_test(
        &mut self,
        content: koru_native_notifications::Content,
        sound: koru_native_notifications::Sound,
    ) -> Result<(), Failure> {
        let mut state = self.0.lock().unwrap();
        state.operation_threads.push(std::thread::current().id());
        state.control_calls.push("send_test");
        if let Some(code) = state.fail_control.take() {
            return Err(Failure {
                code,
                detail: "armed control failure".into(),
            });
        }
        state.test_notifications.push((content, sound));
        Ok(())
    }
}

#[derive(Default)]
struct LedgerMemory {
    state: LedgerState,
    fail_replace: bool,
    fail_next_load: bool,
    fail_load_after_replace: bool,
    native_reset_required: bool,
}

#[derive(Clone)]
struct FakeLedger(Arc<Mutex<LedgerMemory>>);

impl Ledger for FakeLedger {
    fn load(&mut self) -> Result<LedgerState, SchedulerFailure> {
        let mut memory = self.0.lock().unwrap();
        if memory.fail_next_load {
            memory.fail_next_load = false;
            return Err(SchedulerFailure::new(FailureCategory::Ledger));
        }
        Ok(memory.state.clone())
    }

    fn replace(&mut self, state: &LedgerState) -> Result<(), SchedulerFailure> {
        let mut memory = self.0.lock().unwrap();
        if memory.fail_replace {
            memory.fail_replace = false;
            return Err(SchedulerFailure::new(FailureCategory::Ledger));
        }
        memory.state = state.clone();
        memory.native_reset_required = false;
        if memory.fail_load_after_replace {
            memory.fail_load_after_replace = false;
            memory.fail_next_load = true;
        }
        Ok(())
    }

    fn native_reset_required(&self) -> bool {
        self.0.lock().unwrap().native_reset_required
    }
}

#[derive(Clone)]
struct FakeClock(Arc<Mutex<i64>>);

impl Clock for FakeClock {
    fn now_ms(&mut self) -> i64 {
        *self.0.lock().unwrap()
    }
}

#[derive(Default)]
struct ZoneState {
    name: String,
    fail: bool,
}

#[derive(Clone)]
struct FakeZone(Arc<Mutex<ZoneState>>);

impl ZoneProvider for FakeZone {
    fn resolve_wall(&mut self, wall: &str) -> Result<LocalResolution, SchedulerFailure> {
        let state = self.0.lock().unwrap();
        if state.fail {
            return Err(SchedulerFailure::new(FailureCategory::TimeZone));
        }
        let resolved = koru_time_zone::IanaZoneRules::parse(&state.name)
            .and_then(|rules| rules.resolve_canonical_wall(wall))
            .map_err(|_| SchedulerFailure::new(FailureCategory::TimeZone))?;
        Ok(LocalResolution {
            instant_ms: resolved.instant_ms,
            rules: ZoneRulesFingerprint {
                canonical_zone: resolved.rules.canonical_zone,
                database_version: resolved.rules.database_version.to_owned(),
            },
        })
    }
}

struct EventsState {
    head: u64,
    fail_head: bool,
    fail_read: bool,
    pages: VecDeque<EventPage>,
    audit: Arc<Mutex<Vec<&'static str>>>,
}

#[derive(Clone)]
struct FakeEvents(Arc<Mutex<EventsState>>);

impl EventCursor for FakeEvents {
    fn head(&mut self) -> Result<u64, SchedulerFailure> {
        let mut state = self.0.lock().unwrap();
        state.audit.lock().unwrap().push("head");
        if state.fail_head {
            state.fail_head = false;
            return Err(SchedulerFailure::new(FailureCategory::Events));
        }
        Ok(state.head)
    }

    fn read_after(&mut self, cursor: u64) -> Result<EventPage, SchedulerFailure> {
        let mut state = self.0.lock().unwrap();
        if state.fail_read {
            state.fail_read = false;
            return Err(SchedulerFailure::new(FailureCategory::Events));
        }
        Ok(state.pages.pop_front().unwrap_or(EventPage {
            next_cursor: cursor,
            wakes: Vec::new(),
            has_more: false,
        }))
    }
}

type TestScheduler = Scheduler<FakeStore, FakeNative, FakeLedger, FakeClock, FakeZone, FakeEvents>;
const TEST_ZONE: &str = stringify!(UTC);

struct Harness {
    scheduler: TestScheduler,
    store: Arc<Mutex<StoreState>>,
    native: Arc<Mutex<NativeState>>,
    ledger: Arc<Mutex<LedgerMemory>>,
    clock: Arc<Mutex<i64>>,
    zone: Arc<Mutex<ZoneState>>,
    events: Arc<Mutex<EventsState>>,
}

impl Harness {
    fn new(profile: ProfileId, snapshot: ReminderSnapshot, now: i64, zone: &str) -> Self {
        let audit = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(Mutex::new(StoreState {
            profiles: vec![profile],
            locales: HashMap::from([(profile, "en-US".into())]),
            snapshots: HashMap::from([(profile, snapshot)]),
            audit: Arc::clone(&audit),
            ..StoreState::default()
        }));
        let native = Arc::new(Mutex::new(NativeState::default()));
        let ledger = Arc::new(Mutex::new(LedgerMemory::default()));
        let clock = Arc::new(Mutex::new(now));
        let zone = Arc::new(Mutex::new(ZoneState {
            name: zone.into(),
            fail: false,
        }));
        let events = Arc::new(Mutex::new(EventsState {
            head: 5,
            fail_head: false,
            fail_read: false,
            pages: VecDeque::new(),
            audit,
        }));
        let scheduler = Scheduler::new(
            FakeStore(Arc::clone(&store)),
            FakeNative(Arc::clone(&native)),
            FakeLedger(Arc::clone(&ledger)),
            FakeClock(Arc::clone(&clock)),
            FakeZone(Arc::clone(&zone)),
            FakeEvents(Arc::clone(&events)),
        );
        Self {
            scheduler,
            store,
            native,
            ledger,
            clock,
            zone,
            events,
        }
    }

    fn wake_profile(&self, profile: ProfileId, next_cursor: u64) {
        self.events.lock().unwrap().pages.push_back(EventPage {
            next_cursor,
            wakes: vec![EventWake::Profile(profile)],
            has_more: false,
        });
    }
}

fn entity() -> EntityRef {
    EntityRef::new(
        "task",
        WorkspaceId::new(),
        DocumentId::new(),
        uuid::Uuid::new_v4(),
    )
    .unwrap()
}

fn floating_row(wall: &str) -> ReminderSnapshotRow {
    let rule = ReminderRule {
        id: ReminderId::new(),
        target: entity(),
        revision: RuleRevisionId::new(),
        status: RuleStatus::Enabled,
        schedule: OneTimeSchedule::Floating {
            wall: FloatingDateTime::parse(wall).unwrap(),
        },
        tone_override: None,
    };
    row(rule, None)
}

fn zoned_row(instant_ms: i64) -> ReminderSnapshotRow {
    let rule = ReminderRule {
        id: ReminderId::new(),
        target: entity(),
        revision: RuleRevisionId::new(),
        status: RuleStatus::Enabled,
        schedule: OneTimeSchedule::Zoned {
            value: ZonedDateTime::new(instant_ms, "UTC").unwrap(),
        },
        tone_override: None,
    };
    row(rule, None)
}

fn relative_row(instant_ms: i64) -> ReminderSnapshotRow {
    let rule = ReminderRule {
        id: ReminderId::new(),
        target: entity(),
        revision: RuleRevisionId::new(),
        status: RuleStatus::Enabled,
        schedule: OneTimeSchedule::RelativeToDue {
            relation: DueRelation::At,
            offset: None,
        },
        tone_override: None,
    };
    let due = TemporalValue::Date {
        date: CalendarDate::parse("2026-01-02").unwrap(),
    };
    row(
        rule,
        Some(OccurrenceSource::due_relative(
            due,
            OccurrenceMoment::Zoned {
                value: ZonedDateTime::new(instant_ms, "UTC").unwrap(),
            },
        )),
    )
}

fn row(rule: ReminderRule, occurrence_source: Option<OccurrenceSource>) -> ReminderSnapshotRow {
    let stored = StoredReminder {
        rule,
        deleted: false,
        actions: Vec::new(),
    };
    let target = ReminderTarget {
        target: stored.rule.target.clone(),
        title: "private customer title".into(),
        source_location: "Acme / Notes".into(),
        state: TargetState::Active,
        due: None,
        projected: true,
    };
    let reduced = reduce(
        &stored.rule,
        stored.deleted,
        target.state,
        occurrence_source.as_ref(),
        ReminderActions::new(stored.rule.id, &stored.actions),
    );
    ReminderSnapshotRow {
        stored,
        target,
        occurrence_source,
        reduced,
    }
}

fn snapshot(rows: Vec<ReminderSnapshotRow>) -> ReminderSnapshot {
    ReminderSnapshot {
        reminders: rows,
        settings: ReminderSettings::default(),
        updating_workspaces: Vec::new(),
    }
}

fn refresh(row: &mut ReminderSnapshotRow) {
    row.reduced = reduce(
        &row.stored.rule,
        row.stored.deleted,
        row.target.state,
        row.occurrence_source.as_ref(),
        ReminderActions::new(row.stored.rule.id, &row.stored.actions),
    );
}

fn current_occurrence(row: &ReminderSnapshotRow) -> koru_store_core::reminders::OccurrenceKey {
    match row.reduced.effective {
        koru_store_core::reminders::EffectiveState::Scheduled { occurrence, .. }
        | koru_store_core::reminders::EffectiveState::Snoozed { occurrence, .. }
        | koru_store_core::reminders::EffectiveState::Handled { occurrence } => occurrence,
        _ => panic!("row has no current occurrence"),
    }
}

#[test]
fn a_future_occurrence_registers_once_with_a_portable_digest_id() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    let report = harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(report.registered, 1);
    assert_eq!(report.cursor, Some(5));
    assert_eq!(report.next_deadline_ms, Some(10_000));
    assert_eq!(report.pending_count, 1);
    assert_eq!(report.registered_count, 1);
    assert_eq!(report.failed_count, 0);

    let native = harness.native.lock().unwrap();
    let id = native.scheduled[0].registration.as_str();
    assert_eq!(id.len(), 64);
    assert!(
        id.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    drop(native);

    harness.events.lock().unwrap().pages.push_back(EventPage {
        next_cursor: 6,
        wakes: vec![EventWake::Irrelevant],
        has_more: false,
    });
    let report = harness.scheduler.run(Trigger::Events).unwrap();
    assert_eq!(report.registered, 0);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
    assert_eq!(harness.ledger.lock().unwrap().state.event_cursor, Some(6));
}

#[test]
fn disabling_withdraws_this_roots_native_state_and_enabling_repairs_from_authority() {
    let profile = ProfileId::new();
    let now = 1_725_000_000_000;
    let mut harness = Harness::new(
        profile,
        snapshot(vec![floating_row("2024-09-01T09:00:00")]),
        now,
        TEST_ZONE,
    );
    let initial = harness.scheduler.run(Trigger::Startup).unwrap();
    assert!(initial.delivery_enabled);
    assert_eq!(initial.registered_count, 1);
    assert_eq!(initial.last_successful_sync_ms, Some(now));

    let disabled = harness.scheduler.disable_delivery().unwrap();
    assert!(!disabled.delivery_enabled);
    assert_eq!(disabled.withdrawn, 1);
    assert_eq!(disabled.registered_count, 0);
    assert_eq!(disabled.next_deadline_ms, None);
    assert!(harness.ledger.lock().unwrap().state.entries.is_empty());
    assert_eq!(harness.native.lock().unwrap().namespace_resets, 1);

    let repaired = harness
        .scheduler
        .run(Trigger::NativeCapabilityChanged)
        .unwrap();
    assert!(repaired.delivery_enabled);
    assert_eq!(repaired.registered_count, 1);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
}

#[test]
fn corrupt_ledger_recovery_clears_obsolete_native_state_before_rebuild() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();

    let mut obsolete = harness.native.lock().unwrap().scheduled[0].clone();
    obsolete.registration = RegistrationId::new("obsolete-registration").unwrap();
    harness.native.lock().unwrap().scheduled.push(obsolete);
    harness.ledger.lock().unwrap().native_reset_required = true;

    let report = harness.scheduler.run(Trigger::Startup).unwrap();
    let native = harness.native.lock().unwrap();
    assert_eq!(native.namespace_resets, 1);
    assert_eq!(report.registered, 1);
    assert_eq!(native.scheduled.len(), 1);
    assert_ne!(
        native.scheduled[0].registration.as_str(),
        "obsolete-registration"
    );
}

#[test]
fn unsupported_adapter_with_missing_ledger_never_touches_native_namespace() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.ledger.lock().unwrap().native_reset_required = true;
    harness.native.lock().unwrap().static_unsupported = true;

    let error = harness.scheduler.run(Trigger::Startup).unwrap_err();
    assert_eq!(error.category, FailureCategory::NativeUnsupported);
    let native = harness.native.lock().unwrap();
    assert_eq!(native.namespace_resets, 0);
    assert!(native.scheduled.is_empty());
    assert!(
        native.operation_threads.is_empty(),
        "static unsupported must not inspect authorization through the native service"
    );
    assert!(harness.ledger.lock().unwrap().native_reset_required);
}

#[test]
fn ready_adapter_with_missing_ledger_resets_root_namespace_then_rebuilds() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.ledger.lock().unwrap().native_reset_required = true;

    let report = harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(report.registered, 1);
    let native = harness.native.lock().unwrap();
    assert_eq!(native.namespace_resets, 1);
    assert_eq!(native.scheduled.len(), 1);
    assert!(!harness.ledger.lock().unwrap().native_reset_required);
}

#[test]
fn restart_and_reconnect_keep_an_identical_future_registration() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    let registration = harness.native.lock().unwrap().scheduled[0]
        .registration
        .clone();

    harness.scheduler.run(Trigger::Reconnect).unwrap();
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
    assert!(harness.native.lock().unwrap().withdrawn.is_empty());

    let mut restarted = Scheduler::new(
        FakeStore(Arc::clone(&harness.store)),
        FakeNative(Arc::clone(&harness.native)),
        FakeLedger(Arc::clone(&harness.ledger)),
        FakeClock(Arc::clone(&harness.clock)),
        FakeZone(Arc::clone(&harness.zone)),
        FakeEvents(Arc::clone(&harness.events)),
    );
    let report = restarted.run(Trigger::Startup).unwrap();
    assert_eq!(report.unchanged, 1);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
    assert_eq!(
        harness.ledger.lock().unwrap().state.entries[0].registration,
        registration
    );
}

#[test]
fn cold_start_repairs_a_future_entry_without_a_native_handle() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    harness.ledger.lock().unwrap().state.entries[0].native_handle = None;
    harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(harness.native.lock().unwrap().withdrawn.len(), 1);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 2);
}

#[test]
fn two_devices_keep_one_occurrence_but_resolve_a_floating_wall_locally() {
    let profile = ProfileId::new();
    let row = floating_row("2026-07-01T09:00:00");
    let occurrence = current_occurrence(&row);
    let mut east = Harness::new(
        profile,
        snapshot(vec![row.clone()]),
        1_000,
        "America/New_York",
    );
    let mut west = Harness::new(profile, snapshot(vec![row]), 1_000, "America/Los_Angeles");
    east.scheduler.run(Trigger::Startup).unwrap();
    west.scheduler.run(Trigger::Startup).unwrap();
    let east_native = east.native.lock().unwrap();
    let west_native = west.native.lock().unwrap();
    assert_eq!(
        east_native.scheduled[0].registration,
        west_native.scheduled[0].registration
    );
    assert_eq!(
        west_native.scheduled[0].deliver_at_unix_ms - east_native.scheduled[0].deliver_at_unix_ms,
        3 * 60 * 60 * 1_000
    );
    assert_eq!(
        east.ledger.lock().unwrap().state.entries[0].occurrence,
        occurrence
    );
}

#[test]
fn snooze_and_handled_actions_converge_across_two_devices() {
    let profile = ProfileId::new();
    let row = floating_row("2026-07-01T09:00:00");
    let occurrence = current_occurrence(&row);
    let mut east = Harness::new(
        profile,
        snapshot(vec![row.clone()]),
        1_000,
        "America/New_York",
    );
    let mut west = Harness::new(profile, snapshot(vec![row]), 1_000, "America/Los_Angeles");
    east.scheduler.run(Trigger::Startup).unwrap();
    west.scheduler.run(Trigger::Startup).unwrap();

    let snooze = OccurrenceAction {
        id: ActionId::new(),
        occurrence,
        kind: ActionKind::SnoozedUntil { instant_ms: 50_000 },
    };
    for store in [&east.store, &west.store] {
        let mut store = store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.stored.actions.push(snooze.clone());
        refresh(row);
    }
    east.wake_profile(profile, 6);
    west.wake_profile(profile, 6);
    east.scheduler.run(Trigger::Events).unwrap();
    west.scheduler.run(Trigger::Events).unwrap();
    let east_snooze = east.native.lock().unwrap().scheduled[1].clone();
    let west_snooze = west.native.lock().unwrap().scheduled[1].clone();
    assert_eq!(east_snooze.registration, west_snooze.registration);
    assert_eq!(east_snooze.deliver_at_unix_ms, 50_000);
    assert_eq!(west_snooze.deliver_at_unix_ms, 50_000);

    let handled = OccurrenceAction {
        id: ActionId::new(),
        occurrence,
        kind: ActionKind::Handled,
    };
    for store in [&east.store, &west.store] {
        let mut store = store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.stored.actions.push(handled.clone());
        refresh(row);
    }
    east.wake_profile(profile, 7);
    west.wake_profile(profile, 7);
    east.scheduler.run(Trigger::Events).unwrap();
    west.scheduler.run(Trigger::Events).unwrap();
    assert!(east.ledger.lock().unwrap().state.entries.is_empty());
    assert!(west.ledger.lock().unwrap().state.entries.is_empty());
}

#[test]
fn snooze_replaces_the_schedule_and_handled_activation_withdraws_it() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let reminder = row.stored.rule.id;
    let occurrence = current_occurrence(&row);
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    let registration = harness.native.lock().unwrap().scheduled[0]
        .registration
        .clone();
    {
        let mut store = harness.store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.stored.actions.push(OccurrenceAction {
            id: ActionId::new(),
            occurrence,
            kind: ActionKind::SnoozedUntil { instant_ms: 20_000 },
        });
        refresh(row);
    }
    harness.wake_profile(profile, 6);
    harness.scheduler.run(Trigger::Events).unwrap();
    assert_eq!(
        harness.native.lock().unwrap().scheduled[1].deliver_at_unix_ms,
        20_000
    );

    let result = harness
        .scheduler
        .activate(Activation::new(
            registration,
            koru_native_notifications::Action::Handled,
        ))
        .unwrap();
    assert!(matches!(result, ActivationResult::Handled(_)));
    assert_eq!(harness.native.lock().unwrap().withdrawn.len(), 2);
    let row = &harness.store.lock().unwrap().snapshots[&profile].reminders[0];
    assert_eq!(row.stored.rule.id, reminder);
    assert!(matches!(
        row.reduced.effective,
        koru_store_core::reminders::EffectiveState::Handled { .. }
    ));
}

#[test]
fn handling_one_of_four_delivered_reminders_preserves_the_other_three() {
    let profile = ProfileId::new();
    let rows = (0..4).map(|_| zoned_row(10_000)).collect::<Vec<_>>();
    let handled_reminder = rows[0].stored.rule.id;
    let mut harness = Harness::new(profile, snapshot(rows), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    *harness.clock.lock().unwrap() = 10_000;
    let due = harness.scheduler.run(Trigger::Deadline).unwrap();
    assert_eq!(due.attention.len(), 4);
    let registration = harness.native.lock().unwrap().scheduled[0]
        .registration
        .clone();

    let result = harness
        .scheduler
        .activate(Activation::new(
            registration,
            koru_native_notifications::Action::Handled,
        ))
        .unwrap();
    let ActivationResult::Handled(report) = result else {
        panic!("Handled must return the remaining attention projection")
    };
    assert_eq!(report.attention.len(), 3);
    assert!(
        report
            .attention
            .iter()
            .all(|(_, reminder, _, _)| *reminder != handled_reminder)
    );
    let store = harness.store.lock().unwrap();
    let reminders = &store.snapshots[&profile].reminders;
    assert!(matches!(
        reminders
            .iter()
            .find(|row| row.stored.rule.id == handled_reminder)
            .unwrap()
            .reduced
            .effective,
        koru_store_core::reminders::EffectiveState::Handled { .. }
    ));
    assert_eq!(
        reminders
            .iter()
            .filter(|row| !matches!(
                row.reduced.effective,
                koru_store_core::reminders::EffectiveState::Handled { .. }
            ))
            .count(),
        3
    );
}

#[test]
fn clearing_visible_history_never_redelivers_a_handled_occurrence() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let occurrence = current_occurrence(&row);
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    {
        let mut store = harness.store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.stored.actions.push(OccurrenceAction {
            id: ActionId::new(),
            occurrence,
            kind: ActionKind::Handled,
        });
        refresh(row);
        assert!(matches!(
            row.reduced.effective,
            koru_store_core::reminders::EffectiveState::Handled { .. }
        ));
        // This is the corrected Store contract: visible history is empty, but
        // the authoritative reduction retains the content-free terminal fact.
        row.stored.actions.clear();
    }
    harness.wake_profile(profile, 6);
    harness.scheduler.run(Trigger::Events).unwrap();
    assert_eq!(harness.native.lock().unwrap().withdrawn.len(), 1);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
    assert!(harness.ledger.lock().unwrap().state.entries.is_empty());

    harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
}

#[test]
fn due_clear_waits_and_a_new_due_occurrence_restores_delivery() {
    let profile = ProfileId::new();
    let row = relative_row(10_000);
    let first = current_occurrence(&row);
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    {
        let mut store = harness.store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.occurrence_source = None;
        refresh(row);
    }
    harness.wake_profile(profile, 6);
    harness.scheduler.run(Trigger::Events).unwrap();
    assert_eq!(harness.native.lock().unwrap().withdrawn.len(), 1);
    {
        let mut store = harness.store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.occurrence_source = Some(OccurrenceSource::due_relative(
            TemporalValue::Date {
                date: CalendarDate::parse("2026-01-03").unwrap(),
            },
            OccurrenceMoment::Zoned {
                value: ZonedDateTime::new(30_000, "UTC").unwrap(),
            },
        ));
        refresh(row);
        assert_ne!(current_occurrence(row), first);
    }
    harness.wake_profile(profile, 7);
    harness.scheduler.run(Trigger::Events).unwrap();
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 2);
}

#[test]
fn store_reconnect_repairs_an_offline_due_edit_without_a_later_event() {
    let profile = ProfileId::new();
    let row = relative_row(10_000);
    let first = current_occurrence(&row);
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    {
        let mut store = harness.store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.occurrence_source = Some(OccurrenceSource::due_relative(
            TemporalValue::Date {
                date: CalendarDate::parse("2026-01-04").unwrap(),
            },
            OccurrenceMoment::Zoned {
                value: ZonedDateTime::new(40_000, "UTC").unwrap(),
            },
        ));
        refresh(row);
        assert_ne!(current_occurrence(row), first);
    }

    // No cursor page is queued: StoreAvailability(running=true) maps to this
    // full repair even when the live tail has no event to deliver.
    harness.scheduler.run(Trigger::Reconnect).unwrap();
    let native = harness.native.lock().unwrap();
    assert_eq!(native.withdrawn.len(), 1);
    assert_eq!(native.scheduled.len(), 2);
    assert_eq!(native.scheduled[1].deliver_at_unix_ms, 40_000);
}

#[test]
fn crash_window_cleanup_waits_for_a_current_authoritative_missing_projection() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let target = row.target.target.clone();
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    {
        let mut store = harness.store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.target.state = TargetState::Missing;
        row.target.projected = false;
        refresh(row);
    }
    let pending = harness.scheduler.run(Trigger::Reconnect).unwrap_err();
    assert_eq!(pending.category, FailureCategory::ProjectionUpdating);
    assert_eq!(harness.ledger.lock().unwrap().state.event_cursor, Some(5));
    assert!(harness.native.lock().unwrap().withdrawn.is_empty());
    assert!(harness.store.lock().unwrap().deleted_targets.is_empty());

    // This is the P3 crash window: projection transitioned, but its journal
    // fact never committed. Reconnect has no event to consume; the full current
    // snapshot alone must discover and clean it.
    {
        let mut store = harness.store.lock().unwrap();
        store.snapshots.get_mut(&profile).unwrap().reminders[0]
            .target
            .projected = true;
    }
    harness.scheduler.run(Trigger::Reconnect).unwrap();
    assert_eq!(harness.native.lock().unwrap().withdrawn.len(), 1);
    assert_eq!(
        harness.store.lock().unwrap().deleted_targets,
        vec![(profile, target)]
    );
    assert!(harness.ledger.lock().unwrap().state.entries.is_empty());
}

#[test]
fn missing_target_cleanup_precedes_an_unrelated_native_permission_failure() {
    let profile = ProfileId::new();
    let mut missing = zoned_row(10_000);
    let missing_target = missing.target.target.clone();
    missing.target.state = TargetState::Missing;
    refresh(&mut missing);
    let active = zoned_row(20_000);
    let active_target = active.target.target.clone();
    let mut harness = Harness::new(profile, snapshot(vec![missing, active]), 1_000, TEST_ZONE);
    harness.native.lock().unwrap().authorization = Some(Authorization::Denied);

    assert_eq!(
        harness
            .scheduler
            .run(Trigger::Startup)
            .unwrap_err()
            .category,
        FailureCategory::NativePermission
    );
    assert_eq!(
        harness.store.lock().unwrap().deleted_targets,
        vec![(profile, missing_target)]
    );
    assert_ne!(
        harness.store.lock().unwrap().deleted_targets[0].1,
        active_target
    );
    assert!(harness.native.lock().unwrap().scheduled.is_empty());
}

#[test]
fn a_cold_projection_freezes_the_cursor_then_schedules_when_current() {
    let profile = ProfileId::new();
    let mut row = zoned_row(10_000);
    row.target.projected = false;
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    let failure = harness.scheduler.run(Trigger::Startup).unwrap_err();
    assert_eq!(failure.category, FailureCategory::ProjectionUpdating);
    assert_eq!(harness.ledger.lock().unwrap().state.event_cursor, None);
    assert!(harness.native.lock().unwrap().scheduled.is_empty());

    harness
        .store
        .lock()
        .unwrap()
        .snapshots
        .get_mut(&profile)
        .unwrap()
        .reminders[0]
        .target
        .projected = true;
    let report = harness.scheduler.run(Trigger::Reconnect).unwrap();
    assert_eq!(report.cursor, Some(5));
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
}

#[test]
fn a_healthy_profile_reconciles_while_another_profile_projection_is_cold() {
    let cold_profile = ProfileId::new();
    let healthy_profile = ProfileId::new();
    let mut cold = zoned_row(10_000);
    cold.target.projected = false;
    let healthy = zoned_row(20_000);
    let mut harness = Harness::new(cold_profile, snapshot(vec![cold]), 1_000, "UTC");
    {
        let mut store = harness.store.lock().unwrap();
        store.profiles.push(healthy_profile);
        store
            .snapshots
            .insert(healthy_profile, snapshot(vec![healthy]));
    }

    let failure = harness.scheduler.run(Trigger::Startup).unwrap_err();
    assert_eq!(failure.category, FailureCategory::ProjectionUpdating);
    let ledger = harness.ledger.lock().unwrap().state.clone();
    assert_eq!(ledger.event_cursor, None);
    assert_eq!(ledger.entries.len(), 1);
    assert_eq!(ledger.entries[0].profile, healthy_profile);

    harness
        .store
        .lock()
        .unwrap()
        .snapshots
        .get_mut(&cold_profile)
        .unwrap()
        .reminders[0]
        .target
        .projected = true;
    let report = harness.scheduler.run(Trigger::Reconnect).unwrap();
    assert_eq!(report.cursor, Some(5));
    assert_eq!(report.registered, 1);
    assert_eq!(report.unchanged, 1);
    assert_eq!(harness.ledger.lock().unwrap().state.entries.len(), 2);
}

#[test]
fn an_updating_workspace_freezes_a_global_wake_until_the_sweep_finishes() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let workspace = row.target.target.workspace();
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    {
        let mut store = harness.store.lock().unwrap();
        let snapshot = store.snapshots.get_mut(&profile).unwrap();
        snapshot.updating_workspaces.push(workspace);
    }
    harness.events.lock().unwrap().pages.push_back(EventPage {
        next_cursor: 6,
        wakes: vec![EventWake::AllProfiles],
        has_more: false,
    });
    let failure = harness.scheduler.run(Trigger::Events).unwrap_err();
    assert_eq!(failure.category, FailureCategory::ProjectionUpdating);
    assert_eq!(harness.ledger.lock().unwrap().state.event_cursor, Some(5));
    assert!(harness.native.lock().unwrap().withdrawn.is_empty());

    harness
        .store
        .lock()
        .unwrap()
        .snapshots
        .get_mut(&profile)
        .unwrap()
        .updating_workspaces
        .clear();
    harness.events.lock().unwrap().pages.push_back(EventPage {
        next_cursor: 6,
        wakes: vec![EventWake::AllProfiles],
        has_more: false,
    });
    let report = harness.scheduler.run(Trigger::Events).unwrap();
    assert_eq!(report.cursor, Some(6));
    assert_eq!(report.unchanged, 1);
}

#[test]
fn bin_restore_and_permanent_deletion_are_authoritative_diffs() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    {
        let mut store = harness.store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.target.state = TargetState::InBin;
        refresh(row);
    }
    harness.wake_profile(profile, 6);
    harness.scheduler.run(Trigger::Events).unwrap();
    assert_eq!(harness.native.lock().unwrap().withdrawn.len(), 1);
    {
        let mut store = harness.store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.target.state = TargetState::Active;
        refresh(row);
    }
    harness.wake_profile(profile, 7);
    harness.scheduler.run(Trigger::Events).unwrap();
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 2);
}

#[test]
fn missed_boundary_and_quiet_summary_are_once_even_across_restart() {
    let profile = ProfileId::new();
    let day = 24 * 60 * 60 * 1_000;
    let now = 10 * day;
    let recent = zoned_row(now - day);
    let old = zoned_row(now - day - 1);
    let mut harness = Harness::new(profile, snapshot(vec![recent, old]), now, "UTC");
    let report = harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(report.registered, 1, "exactly 24h is delivered immediately");
    assert_eq!(report.quiet_summaries[0].count, 1);
    assert_eq!(
        harness.native.lock().unwrap().scheduled[0].deliver_at_unix_ms,
        now
    );

    harness.wake_profile(profile, 6);
    let again = harness.scheduler.run(Trigger::Events).unwrap();
    assert!(again.quiet_summaries.is_empty());
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 2);
    harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 2);
}

#[test]
fn thirty_minutes_and_twenty_hours_deliver_while_three_days_groups_quietly() {
    let profile = ProfileId::new();
    let hour = 60 * 60 * 1_000;
    let now = 10 * 24 * hour;
    let rows = vec![
        zoned_row(now - 30 * 60 * 1_000),
        zoned_row(now - 20 * hour),
        zoned_row(now - 3 * 24 * hour),
    ];
    let mut harness = Harness::new(profile, snapshot(rows), now, "UTC");
    let report = harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(report.registered, 2);
    assert_eq!(
        report.quiet_summaries,
        vec![QuietSummary { profile, count: 1 }]
    );
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 3);
    assert!(
        harness
            .native
            .lock()
            .unwrap()
            .scheduled
            .iter()
            .all(|request| request.deliver_at_unix_ms == now)
    );
}

#[test]
fn missed_and_wait_calculations_saturate_at_integer_boundaries() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(
        profile,
        snapshot(vec![zoned_row(i64::MIN)]),
        i64::MAX,
        "UTC",
    );
    let report = harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(
        report.quiet_summaries,
        vec![QuietSummary { profile, count: 1 }]
    );

    let future = zoned_row(i64::MAX);
    let mut harness = Harness::new(profile, snapshot(vec![future]), i64::MIN, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(
        harness.scheduler.wait_duration().unwrap(),
        Some(std::time::Duration::from_millis(i64::MAX as u64))
    );
}

#[test]
fn an_awake_deadline_marks_the_native_registration_delivered_without_replay() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    *harness.clock.lock().unwrap() = 10_000;
    let report = harness.scheduler.run(Trigger::Deadline).unwrap();
    assert_eq!(report.unchanged, 1);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
    assert!(harness.native.lock().unwrap().withdrawn.is_empty());
    assert_eq!(
        harness.ledger.lock().unwrap().state.entries[0].status,
        RegistrationStatus::Delivered
    );
    assert_eq!(
        harness.ledger.lock().unwrap().state.entries[0].fired_at_ms,
        Some(10_000)
    );
    assert_eq!(report.attention[0].3, 10_000);
}

#[test]
fn startup_repairs_an_overdue_registered_occurrence_once() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    *harness.clock.lock().unwrap() = 20_000;

    let report = harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(report.registered, 1);
    assert_eq!(harness.native.lock().unwrap().withdrawn.len(), 1);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 2);
    assert_eq!(
        harness.native.lock().unwrap().scheduled[1].deliver_at_unix_ms,
        20_000
    );
    assert_eq!(
        harness.ledger.lock().unwrap().state.entries[0].status,
        RegistrationStatus::Delivered
    );
    assert_eq!(
        harness.ledger.lock().unwrap().state.entries[0].fired_at_ms,
        Some(20_000),
        "an overdue repair fires now, not at the old due instant"
    );
    assert_eq!(report.attention[0].3, 20_000);

    harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 2);
    assert_eq!(harness.native.lock().unwrap().withdrawn.len(), 1);
}

#[test]
fn startup_restores_an_overdue_snooze_to_attention_once() {
    let profile = ProfileId::new();
    let mut row = zoned_row(10_000);
    let occurrence = current_occurrence(&row);
    row.stored.actions.push(OccurrenceAction {
        id: ActionId::new(),
        occurrence,
        kind: ActionKind::SnoozedUntil { instant_ms: 30_000 },
    });
    refresh(&mut row);
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(
        harness.native.lock().unwrap().scheduled[0].deliver_at_unix_ms,
        30_000
    );

    *harness.clock.lock().unwrap() = 40_000;
    let restored = harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(restored.attention.len(), 1);
    assert_eq!(restored.attention[0].3, 40_000);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 2);
    assert_eq!(
        harness.native.lock().unwrap().scheduled[1].deliver_at_unix_ms,
        40_000,
        "a snooze that elapsed while Koru was down must be delivered on restart"
    );

    let again = harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(again.attention, restored.attention);
    assert_eq!(
        harness.native.lock().unwrap().scheduled.len(),
        2,
        "restoring attention must not deliver the snoozed occurrence twice"
    );
}

#[test]
fn resume_quietly_summarizes_an_old_registered_occurrence_once() {
    let profile = ProfileId::new();
    let day = 24 * 60 * 60 * 1_000;
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    *harness.clock.lock().unwrap() = 10_000 + day + 1;

    let report = harness.scheduler.run(Trigger::Resume).unwrap();
    assert_eq!(report.quiet_summaries[0].count, 1);
    assert_eq!((report.pending_count, report.registered_count), (0, 1));
    assert_eq!(report.attention[0].3, 10_000 + day + 1);
    assert_eq!(harness.native.lock().unwrap().withdrawn.len(), 1);
    let native = harness.native.lock().unwrap();
    assert_eq!(native.scheduled.len(), 2);
    let summary = &native.scheduled[1];
    assert_ne!(summary.registration, native.scheduled[0].registration);
    assert_eq!(summary.content.title, "Missed reminders: 1");
    assert_eq!(summary.content.body, None);
    assert_eq!(summary.sound, koru_native_notifications::Sound::Silent);
    assert_eq!(
        summary.actions,
        koru_native_notifications::ActionSet::OpenOnly
    );
    let summary_registration = summary.registration.clone();
    let profile_storage = profile.to_storage();
    assert!(!summary_registration.as_str().contains(&profile_storage));
    assert_eq!(
        summary_registration,
        super::reconcile::summary_registration_id(profile)
    );
    assert_ne!(
        summary_registration,
        super::reconcile::summary_registration_id(ProfileId::new())
    );
    drop(native);
    assert_eq!(
        harness.ledger.lock().unwrap().state.entries[0].status,
        RegistrationStatus::QuietSummarized
    );
    let activation = Activation::new(
        summary_registration.clone(),
        koru_native_notifications::Action::Open,
    );
    assert!(
        !serde_json::to_string(&activation)
            .unwrap()
            .contains(&profile_storage)
    );
    assert!(matches!(
        harness.scheduler.activate(activation).unwrap(),
        ActivationResult::OpenProfile(value) if value == profile
    ));

    let again = harness.scheduler.run(Trigger::Reconnect).unwrap();
    assert!(again.quiet_summaries.is_empty());
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 2);
    assert_eq!(harness.native.lock().unwrap().withdrawn.len(), 1);
}

#[test]
fn quiet_summary_status_waits_for_native_acceptance() {
    let profile = ProfileId::new();
    let day = 24 * 60 * 60 * 1_000;
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    *harness.clock.lock().unwrap() = 10_000 + day + 1;
    harness.native.lock().unwrap().fail_schedule = true;

    assert_eq!(
        harness.scheduler.run(Trigger::Resume).unwrap_err().category,
        FailureCategory::NativePlatform
    );
    let ledger = harness.ledger.lock().unwrap();
    assert_eq!(
        ledger.state.entries[0].status,
        RegistrationStatus::Registered
    );
    assert!(ledger.state.quiet_summaries.is_empty());
    drop(ledger);

    harness.scheduler.run(Trigger::Resume).unwrap();
    assert_eq!(
        harness.ledger.lock().unwrap().state.entries[0].status,
        RegistrationStatus::QuietSummarized
    );
}

#[test]
fn quiet_summary_localizes_the_count_for_every_supported_profile_locale() {
    let cases = [
        ("en-US", "Missed reminders: 1,234"),
        ("es-ES", "Recordatorios perdidos: 1.234"),
        ("pt-BR", "Lembretes perdidos: 1.234"),
        ("de-DE", "Verpasste Erinnerungen: 1.234"),
        ("fr-FR", "Rappels manqués : 1 234"),
        ("ar", "التذكيرات الفائتة: ١٬٢٣٤"),
        ("zh-Hans", "错过的提醒：1,234"),
        ("ja-JP", "未確認のリマインダー: 1,234"),
    ];
    for (locale, expected) in cases {
        let content = super::reconcile::quiet_summary_content(locale, 1_234);
        assert_eq!(content.title, expected, "{locale}");
        assert_eq!(content.body, None);
    }
}

#[test]
fn device_zone_travel_replaces_same_occurrence_and_target_date_uses_dst_rules() {
    let profile = ProfileId::new();
    let row = floating_row("2026-07-01T09:00:00");
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "America/New_York");
    harness.scheduler.run(Trigger::Startup).unwrap();
    let first = harness.native.lock().unwrap().scheduled[0].clone();
    // July in New York is UTC-4, even if reconciliation itself runs in winter.
    assert_eq!(first.deliver_at_unix_ms, 1_782_910_800_000);
    harness.zone.lock().unwrap().name = "America/Los_Angeles".into();
    harness.scheduler.run(Trigger::DeviceZoneChanged).unwrap();
    let native = harness.native.lock().unwrap();
    assert_eq!(native.withdrawn, vec![first.registration.clone()]);
    assert_eq!(native.scheduled[1].registration, first.registration);
    assert_eq!(
        native.scheduled[1].deliver_at_unix_ms - first.deliver_at_unix_ms,
        3 * 60 * 60 * 1_000
    );
}

#[test]
fn zone_travel_that_moves_a_wall_into_the_past_uses_missed_policy_once() {
    let profile = ProfileId::new();
    let row = floating_row("2026-07-01T09:00:00");
    // 14:00 UTC is after 09:00 in New York (13:00 UTC), but before the same
    // wall in Los Angeles (16:00 UTC).
    let now = 1_782_914_400_000;
    let mut harness = Harness::new(profile, snapshot(vec![row]), now, "America/Los_Angeles");
    harness.scheduler.run(Trigger::Startup).unwrap();
    let first = harness.native.lock().unwrap().scheduled[0].clone();
    assert!(first.deliver_at_unix_ms > now);

    harness.zone.lock().unwrap().name = "America/New_York".into();
    harness.scheduler.run(Trigger::DeviceZoneChanged).unwrap();
    let native = harness.native.lock().unwrap();
    assert_eq!(native.withdrawn, vec![first.registration.clone()]);
    assert_eq!(native.scheduled[1].registration, first.registration);
    assert_eq!(native.scheduled[1].deliver_at_unix_ms, now);
    drop(native);

    let ledger = harness.ledger.lock().unwrap().state.clone();
    assert_eq!(ledger.entries[0].status, RegistrationStatus::Delivered);
    assert_eq!(
        ledger.entries[0].fingerprint.deliver_at_ms, 1_782_910_800_000,
        "delivery now must never become the stable fingerprint"
    );
    harness.scheduler.run(Trigger::Resume).unwrap();
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 2);
}

#[test]
fn zoned_and_snoozed_instants_ignore_device_zone_changes() {
    let profile = ProfileId::new();
    let mut row = zoned_row(20_000);
    let occurrence = current_occurrence(&row);
    row.stored.actions.push(OccurrenceAction {
        id: ActionId::new(),
        occurrence,
        kind: ActionKind::SnoozedUntil { instant_ms: 30_000 },
    });
    refresh(&mut row);
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "America/New_York");
    harness.scheduler.run(Trigger::Startup).unwrap();
    harness.zone.lock().unwrap().name = "America/Los_Angeles".into();
    let report = harness.scheduler.run(Trigger::DeviceZoneChanged).unwrap();
    assert_eq!(report.unchanged, 1);
    assert!(harness.native.lock().unwrap().withdrawn.is_empty());
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
    assert!(
        harness.ledger.lock().unwrap().state.entries[0]
            .fingerprint
            .floating_rules
            .is_none()
    );
}

#[test]
fn tone_is_part_of_the_registration_fingerprint() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    {
        let mut store = harness.store.lock().unwrap();
        let snapshot = store.snapshots.get_mut(&profile).unwrap();
        snapshot.reminders[0].stored.rule.tone_override = Some(ToneId::Urgent);
        snapshot.reminders[0].stored.rule.revision = RuleRevisionId::new();
        refresh(&mut snapshot.reminders[0]);
    }
    harness.wake_profile(profile, 6);
    harness.scheduler.run(Trigger::Events).unwrap();
    let native = harness.native.lock().unwrap();
    assert_eq!(native.withdrawn.len(), 1);
    assert_eq!(native.scheduled.len(), 2);
    assert_eq!(
        native.scheduled[1].sound,
        koru_native_notifications::Sound::Bundled(koru_native_notifications::BundledTone::Urgent)
    );
}

#[test]
fn unsupported_custom_sound_isolated_from_system_reminders_and_rebuilds_on_restart() {
    let profile = ProfileId::new();
    let before = zoned_row(10_000);
    let mut unsupported = zoned_row(20_000);
    unsupported.stored.rule.tone_override = Some(ToneId::Soft);
    refresh(&mut unsupported);
    let after = zoned_row(30_000);
    let mut harness = Harness::new(
        profile,
        snapshot(vec![before, unsupported, after]),
        1_000,
        "UTC",
    );
    harness.native.lock().unwrap().custom_sound_unsupported = true;

    let report = harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(report.registered, 2);
    assert_eq!(report.registered_count, 2);
    assert_eq!(report.pending_count, 2);
    assert_eq!(report.failed_count, 1);
    assert_eq!(report.failure, Some(FailureCategory::NativeUnsupported));
    assert_eq!(report.cursor, Some(5));
    assert_eq!(report.next_deadline_ms, Some(10_000));
    assert!(
        !format!("{report:?}").contains("private customer title"),
        "scheduler diagnostics must not expose authored reminder content"
    );
    let native = harness.native.lock().unwrap();
    assert_eq!(native.scheduled.len(), 2);
    assert!(
        native
            .scheduled
            .iter()
            .all(|request| request.sound == Sound::System),
        "unsupported custom sound must not fall back to another tone"
    );
    drop(native);
    assert_eq!(harness.ledger.lock().unwrap().state.entries.len(), 2);

    let other_profile = ProfileId::new();
    {
        let mut store = harness.store.lock().unwrap();
        store.profiles.push(other_profile);
        store.locales.insert(other_profile, "en-US".into());
        store
            .snapshots
            .insert(other_profile, snapshot(vec![zoned_row(40_000)]));
    }
    harness.wake_profile(other_profile, 6);
    let retained = harness.scheduler.run(Trigger::Events).unwrap();
    assert_eq!(retained.registered, 1);
    assert_eq!(retained.failed_count, 1);
    assert_eq!(
        retained.failure,
        Some(FailureCategory::NativeUnsupported),
        "an unrelated wake must not erase an unresolved registration failure"
    );
    assert_eq!(retained.cursor, Some(6));
    assert_eq!(retained.registered_count, 3);

    let mut restarted = Scheduler::new(
        FakeStore(Arc::clone(&harness.store)),
        FakeNative(Arc::clone(&harness.native)),
        FakeLedger(Arc::clone(&harness.ledger)),
        FakeClock(Arc::clone(&harness.clock)),
        FakeZone(Arc::clone(&harness.zone)),
        FakeEvents(Arc::clone(&harness.events)),
    );
    let rebuilt = restarted.run(Trigger::Startup).unwrap();
    assert_eq!(rebuilt.failed_count, 1);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 3);

    harness.native.lock().unwrap().custom_sound_unsupported = false;
    let recovered = restarted.run(Trigger::Reconnect).unwrap();
    assert_eq!(recovered.registered, 1);
    assert_eq!(recovered.registered_count, 4);
    assert_eq!(recovered.pending_count, 4);
    assert_eq!(recovered.failed_count, 0);
    assert_eq!(recovered.failure, None);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 4);
    assert_eq!(harness.ledger.lock().unwrap().state.entries.len(), 4);
}

#[test]
fn a_missing_custom_tone_asset_does_not_block_system_delivery_and_removal_clears_it() {
    let profile = ProfileId::new();
    let mut missing = zoned_row(10_000);
    missing.stored.rule.tone_override = Some(ToneId::Chime);
    refresh(&mut missing);
    let system = zoned_row(20_000);
    let mut harness = Harness::new(profile, snapshot(vec![missing, system]), 1_000, "UTC");
    harness.native.lock().unwrap().sound_failures.push((
        Sound::Bundled(koru_native_notifications::BundledTone::Chime),
        FailureCode::AssetMissing,
    ));

    let report = harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(report.registered, 1);
    assert_eq!(report.registered_count, 1);
    assert_eq!(report.pending_count, 1);
    assert_eq!(report.failed_count, 1);
    assert_eq!(report.failure, Some(FailureCategory::NativeAsset));
    let native = harness.native.lock().unwrap();
    assert_eq!(native.scheduled.len(), 1);
    assert_eq!(native.scheduled[0].sound, Sound::System);
    drop(native);
    assert_eq!(harness.ledger.lock().unwrap().state.entries.len(), 1);

    harness
        .store
        .lock()
        .unwrap()
        .snapshots
        .get_mut(&profile)
        .unwrap()
        .reminders
        .retain(|row| row.stored.rule.tone_override.is_none());
    harness.wake_profile(profile, 6);
    let removed = harness.scheduler.run(Trigger::Events).unwrap();
    assert_eq!(removed.failed_count, 0);
    assert_eq!(removed.failure, None);
    assert_eq!(removed.cursor, Some(6));
    assert_eq!(removed.registered_count, 1);
}

#[test]
fn full_inventory_withdraws_registrations_for_a_removed_profile() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    {
        let mut store = harness.store.lock().unwrap();
        store.profiles.clear();
        store.snapshots.remove(&profile);
    }
    harness.scheduler.run(Trigger::Reconnect).unwrap();
    assert_eq!(harness.native.lock().unwrap().withdrawn.len(), 1);
    assert!(harness.ledger.lock().unwrap().state.entries.is_empty());
}

#[test]
fn failures_do_not_advance_the_cursor_and_a_failed_withdraw_blocks_replacement() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    {
        let mut store = harness.store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.stored.rule.revision = RuleRevisionId::new();
        refresh(row);
    }
    harness.native.lock().unwrap().fail_withdraw = true;
    harness.wake_profile(profile, 6);
    let failure = harness.scheduler.run(Trigger::Events).unwrap_err();
    assert_eq!(failure.category, FailureCategory::NativePlatform);
    assert_eq!(harness.ledger.lock().unwrap().state.event_cursor, Some(5));
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);

    harness.wake_profile(profile, 6);
    harness.native.lock().unwrap().fail_schedule = true;
    assert!(harness.scheduler.run(Trigger::Events).is_err());
    assert_eq!(harness.ledger.lock().unwrap().state.event_cursor, Some(5));
}

#[test]
fn event_store_and_timezone_failures_keep_the_last_durable_cursor() {
    let profile = ProfileId::new();
    let row = floating_row("2026-07-01T09:00:00");
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();

    harness.events.lock().unwrap().fail_read = true;
    assert_eq!(
        harness.scheduler.run(Trigger::Events).unwrap_err().category,
        FailureCategory::Events
    );
    assert_eq!(harness.ledger.lock().unwrap().state.event_cursor, Some(5));

    harness.wake_profile(profile, 6);
    harness.store.lock().unwrap().fail_snapshot = true;
    assert_eq!(
        harness.scheduler.run(Trigger::Events).unwrap_err().category,
        FailureCategory::Store
    );
    assert_eq!(harness.ledger.lock().unwrap().state.event_cursor, Some(5));

    harness.wake_profile(profile, 6);
    harness.zone.lock().unwrap().fail = true;
    assert_eq!(
        harness.scheduler.run(Trigger::Events).unwrap_err().category,
        FailureCategory::TimeZone
    );
    assert_eq!(harness.ledger.lock().unwrap().state.event_cursor, Some(5));
}

#[test]
fn startup_repairs_from_store_even_if_the_event_worker_is_temporarily_down() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.events.lock().unwrap().fail_head = true;
    let failure = harness.scheduler.run(Trigger::Startup).unwrap_err();
    assert_eq!(failure.category, FailureCategory::Events);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
    let ledger = &harness.ledger.lock().unwrap().state;
    assert_eq!(ledger.event_cursor, None);
    assert_eq!(ledger.entries.len(), 1);
}

#[test]
fn head_is_captured_before_the_first_snapshot_and_ledger_failure_keeps_the_boundary() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    let audit = Arc::clone(&harness.events.lock().unwrap().audit);
    harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(
        audit.lock().unwrap().as_slice(),
        &["head", "profiles", "snapshot"]
    );

    harness.ledger.lock().unwrap().fail_replace = true;
    harness.events.lock().unwrap().pages.push_back(EventPage {
        next_cursor: 9,
        wakes: vec![EventWake::Irrelevant],
        has_more: false,
    });
    let failure = harness.scheduler.run(Trigger::Events).unwrap_err();
    assert_eq!(failure.category, FailureCategory::Ledger);
    assert_eq!(harness.ledger.lock().unwrap().state.event_cursor, Some(5));
}

#[test]
fn an_event_after_the_captured_head_is_replayed_even_when_the_snapshot_saw_it() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    // This page represents an event accepted just after head=5. The full
    // snapshot already contains its resulting rule state.
    harness.events.lock().unwrap().pages.push_back(EventPage {
        next_cursor: 6,
        wakes: vec![EventWake::Profile(profile)],
        has_more: false,
    });
    assert_eq!(
        harness.scheduler.run(Trigger::Startup).unwrap().cursor,
        Some(5)
    );
    let replay = harness.scheduler.run(Trigger::Events).unwrap();
    assert_eq!(replay.cursor, Some(6));
    assert_eq!(replay.unchanged, 1);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
}

#[test]
fn multi_page_replay_advances_past_irrelevant_events_only_after_success() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    {
        let mut events = harness.events.lock().unwrap();
        events.pages.push_back(EventPage {
            next_cursor: 6,
            wakes: vec![EventWake::Irrelevant],
            has_more: true,
        });
        events.pages.push_back(EventPage {
            next_cursor: 9,
            wakes: vec![EventWake::Profile(profile), EventWake::Irrelevant],
            has_more: false,
        });
    }
    let report = harness.scheduler.run(Trigger::Events).unwrap();
    assert_eq!(report.cursor, Some(9));
    assert_eq!(report.unchanged, 1);
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
}

#[test]
fn activation_validation_is_worker_side_and_open_reuses_the_exact_router_target() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let entity = row.target.target.clone();
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    let registration = harness.native.lock().unwrap().scheduled[0]
        .registration
        .clone();
    let result = harness
        .scheduler
        .activate(Activation::new(
            registration.clone(),
            koru_native_notifications::Action::Open,
        ))
        .unwrap();
    let ActivationResult::Open(target) = result else {
        panic!("open activation must return a routing target")
    };
    assert_eq!(target.entity, entity);
    let wire = koru_agent_wire::Target {
        profile: profile.to_storage(),
        workspace: entity.workspace().to_storage(),
        document: Some(entity.document().to_storage()),
        node: Some(entity.node().to_string()),
        trashed: false,
        match_text: None,
        new_window: false,
        kind: koru_agent_wire::TargetKind::Document,
        list: None,
    };
    assert!(matches!(
        crate::route::Router::new().open(
            &crate::registry::Registry::new(),
            profile,
            entity.workspace(),
            wire,
            std::time::Instant::now()
        ),
        crate::route::Routed::Launch { profile: p, workspace: w }
            if p == profile && w == entity.workspace()
    ));

    let snooze = harness
        .scheduler
        .activate(Activation::new(
            registration,
            koru_native_notifications::Action::Snooze,
        ))
        .unwrap();
    assert!(matches!(snooze, ActivationResult::SnoozeManagement(_)));
}

#[test]
fn activation_waits_for_an_authoritative_workspace_projection() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let workspace = row.target.target.workspace();
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    let registration = harness.native.lock().unwrap().scheduled[0]
        .registration
        .clone();
    harness
        .store
        .lock()
        .unwrap()
        .snapshots
        .get_mut(&profile)
        .unwrap()
        .updating_workspaces
        .push(workspace);

    assert_eq!(
        harness
            .scheduler
            .activate(Activation::new(
                registration,
                koru_native_notifications::Action::Open,
            ))
            .unwrap_err()
            .category,
        FailureCategory::ProjectionUpdating
    );
}

#[test]
fn worker_retries_open_after_a_transient_store_failure() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let entity = row.target.target.clone();
    let harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = ReminderWorker::start(
        harness.scheduler,
        crate::event::Waker::new(move |event| {
            let _ = tx.send(event);
        }),
    );
    assert!(matches!(
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
        crate::event::AgentEvent::ReminderWorkerChanged { report }
            if report.failure.is_none()
    ));
    let registration = harness.native.lock().unwrap().scheduled[0]
        .registration
        .clone();
    harness.store.lock().unwrap().fail_snapshot = true;
    worker
        .activate(Activation::new(
            registration,
            koru_native_notifications::Action::Open,
        ))
        .unwrap();

    let mut saw_transient_failure = false;
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap() {
            crate::event::AgentEvent::ReminderWorkerChanged { report }
                if report.failure == Some(FailureCategory::Store) =>
            {
                saw_transient_failure = true;
            }
            crate::event::AgentEvent::ReminderOpen { target } => {
                assert_eq!(target.entity, entity);
                break;
            }
            _ => {}
        }
    }
    assert!(saw_transient_failure);
    drop(worker);
}

#[test]
fn worker_queues_activations_in_order_and_only_routes_content_free_results_on_winit() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let entity = row.target.target.clone();
    let harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    let caller = std::thread::current().id();
    let (tx, rx) = std::sync::mpsc::channel();
    let waker = crate::event::Waker::new(move |event| {
        let _ = tx.send(event);
    });
    let worker = ReminderWorker::start(harness.scheduler, waker);
    let startup = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert!(matches!(
        startup,
        crate::event::AgentEvent::ReminderWorkerChanged { .. }
    ));
    let registration = harness.native.lock().unwrap().scheduled[0]
        .registration
        .clone();
    for action in [
        koru_native_notifications::Action::Open,
        koru_native_notifications::Action::Snooze,
        koru_native_notifications::Action::Handled,
    ] {
        worker
            .activate(Activation::new(registration.clone(), action))
            .unwrap();
    }

    let mut significant = Vec::new();
    let mut open_target = None;
    while significant.len() < 3 {
        match rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap() {
            crate::event::AgentEvent::ReminderOpen { target } => {
                open_target = Some(target);
                significant.push("open");
            }
            crate::event::AgentEvent::ReminderSnooze { target } => {
                assert_eq!(target.entity, entity);
                significant.push("snooze");
            }
            crate::event::AgentEvent::ReminderWorkerChanged { report } if report.withdrawn > 0 => {
                significant.push("handled");
            }
            _ => {}
        }
    }
    assert_eq!(significant, ["open", "snooze", "handled"]);

    let target = open_target.unwrap();
    let wire = koru_agent_wire::Target {
        profile: target.profile.to_storage(),
        workspace: target.entity.workspace().to_storage(),
        document: Some(target.entity.document().to_storage()),
        node: Some(target.entity.node().to_string()),
        trashed: false,
        match_text: None,
        new_window: false,
        kind: koru_agent_wire::TargetKind::Document,
        list: None,
    };
    assert!(matches!(
        crate::route::Router::new().open(
            &crate::registry::Registry::new(),
            target.profile,
            target.entity.workspace(),
            wire,
            std::time::Instant::now()
        ),
        crate::route::Routed::Launch { .. }
    ));
    drop(worker);

    assert!(
        harness
            .store
            .lock()
            .unwrap()
            .operation_threads
            .iter()
            .all(|thread| *thread != caller)
    );
    assert!(
        harness
            .native
            .lock()
            .unwrap()
            .operation_threads
            .iter()
            .all(|thread| *thread != caller)
    );
}

#[test]
fn notification_controls_run_end_to_end_on_the_worker_with_typed_results() {
    let profile = ProfileId::new();
    let harness = Harness::new(profile, snapshot(Vec::new()), 1_000, "UTC");
    let caller = std::thread::current().id();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = ReminderWorker::start(
        harness.scheduler,
        crate::event::Waker::new(move |event| {
            let _ = tx.send(event);
        }),
    );
    assert!(matches!(
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
        crate::event::AgentEvent::ReminderWorkerChanged { .. }
    ));

    let cases = [
        (
            NotificationControl::Inspect,
            NotificationControlResponse::Capability(Capability {
                authorization: Authorization::Allowed,
                actions: true,
                stable_replacement: true,
                withdrawal: true,
                custom_sound: true,
                activation_while_stopped: true,
            }),
        ),
        (
            NotificationControl::RequestAuthorization,
            NotificationControlResponse::Authorization(Authorization::Allowed),
        ),
        (
            NotificationControl::OpenSettings,
            NotificationControlResponse::Done,
        ),
        (
            NotificationControl::SetDeliveryEnabled { enabled: false },
            NotificationControlResponse::Done,
        ),
        (
            NotificationControl::SetDeliveryEnabled { enabled: true },
            NotificationControlResponse::Done,
        ),
        (
            NotificationControl::Repair,
            NotificationControlResponse::Done,
        ),
        (
            NotificationControl::SendTest {
                tone: ToneId::Soft,
                content: koru_native_notifications::Content {
                    title: "localized title".into(),
                    body: Some("localized body".into()),
                },
            },
            NotificationControlResponse::Done,
        ),
    ];
    for (request, expected) in cases {
        let reply = worker.notification_control(request).unwrap();
        assert_eq!(
            reply
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            expected
        );
    }

    let failures = [
        FailureCode::Unsupported,
        FailureCode::PermissionDenied,
        FailureCode::AssetMissing,
        FailureCode::InvalidRequest,
        FailureCode::Platform,
    ];
    for code in failures {
        harness.native.lock().unwrap().fail_control = Some(code);
        let reply = worker
            .notification_control(NotificationControl::OpenSettings)
            .unwrap();
        assert_eq!(
            reply
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            NotificationControlResponse::Failed(code)
        );
    }
    drop(worker);

    let native = harness.native.lock().unwrap();
    assert_eq!(
        native.control_calls[..4],
        [
            "inspect",
            "request_authorization",
            "open_settings",
            "send_test"
        ]
    );
    assert_eq!(native.namespace_resets, 1);
    assert_eq!(native.test_notifications.len(), 1);
    assert_eq!(
        native.test_notifications[0],
        (
            koru_native_notifications::Content {
                title: "localized title".into(),
                body: Some("localized body".into()),
            },
            koru_native_notifications::Sound::Bundled(koru_native_notifications::BundledTone::Soft),
        )
    );
    assert!(
        native
            .operation_threads
            .iter()
            .all(|thread| *thread != caller),
        "a native control escaped onto the caller/winit thread"
    );
}

#[test]
fn allowed_permission_inspection_repairs_a_rejected_reminder_without_a_store_event() {
    let profile = ProfileId::new();
    let harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, TEST_ZONE);
    harness.native.lock().unwrap().authorization = Some(Authorization::Denied);
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = ReminderWorker::start(
        harness.scheduler,
        crate::event::Waker::new(move |event| {
            let _ = tx.send(event);
        }),
    );

    assert!(matches!(
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
        crate::event::AgentEvent::ReminderWorkerChanged { report }
            if report.failure == Some(FailureCategory::NativePermission)
    ));
    assert!(harness.native.lock().unwrap().scheduled.is_empty());

    harness.native.lock().unwrap().authorization = Some(Authorization::Allowed);
    let reply = worker
        .notification_control(NotificationControl::Inspect)
        .unwrap();
    assert!(matches!(
        reply
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap(),
        NotificationControlResponse::Capability(Capability {
            authorization: Authorization::Allowed,
            ..
        })
    ));
    assert!(matches!(
        rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
        crate::event::AgentEvent::ReminderWorkerChanged { report }
            if report.failure.is_none() && report.registered == 1
    ));
    assert_eq!(harness.native.lock().unwrap().scheduled.len(), 1);
    drop(worker);
}

#[test]
fn handled_or_non_authoritative_activations_are_stale() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let occurrence = current_occurrence(&row);
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    let registration = harness.native.lock().unwrap().scheduled[0]
        .registration
        .clone();
    {
        let mut store = harness.store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.stored.actions.push(OccurrenceAction {
            id: ActionId::new(),
            occurrence,
            kind: ActionKind::Handled,
        });
        refresh(row);
    }
    let failure = harness
        .scheduler
        .activate(Activation::new(
            registration,
            koru_native_notifications::Action::Open,
        ))
        .unwrap_err();
    assert_eq!(failure.category, FailureCategory::StaleActivation);
}

#[test]
fn worker_reports_a_deadline_ledger_read_failure_instead_of_waiting_indefinitely() {
    let profile = ProfileId::new();
    let harness = Harness::new(profile, snapshot(Vec::new()), 1_000, "UTC");
    harness.ledger.lock().unwrap().fail_load_after_replace = true;
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = ReminderWorker::start(
        harness.scheduler,
        crate::event::Waker::new(move |event| {
            let _ = tx.send(event);
        }),
    );

    assert!(matches!(
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .unwrap(),
        crate::event::AgentEvent::ReminderWorkerChanged { report }
            if report.failure.is_none()
    ));
    assert!(matches!(
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .unwrap(),
        crate::event::AgentEvent::ReminderWorkerChanged { report }
            if report.failure == Some(FailureCategory::Ledger)
    ));
    drop(worker);
}

#[test]
fn invalid_unknown_stale_revision_and_occurrence_activations_are_refused() {
    let profile = ProfileId::new();
    let row = zoned_row(10_000);
    let mut harness = Harness::new(profile, snapshot(vec![row]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    let registration = harness.native.lock().unwrap().scheduled[0]
        .registration
        .clone();

    let mut invalid = Activation::new(
        registration.clone(),
        koru_native_notifications::Action::Open,
    );
    invalid.schema_version += 1;
    assert_eq!(
        harness.scheduler.activate(invalid).unwrap_err().category,
        FailureCategory::UnsupportedActivation
    );
    assert_eq!(
        harness
            .scheduler
            .activate(Activation::new(
                RegistrationId::new(format!("{:064x}", 999)).unwrap(),
                koru_native_notifications::Action::Open,
            ))
            .unwrap_err()
            .category,
        FailureCategory::StaleActivation
    );

    {
        let mut store = harness.store.lock().unwrap();
        store.snapshots.get_mut(&profile).unwrap().reminders[0]
            .target
            .projected = false;
    }
    assert_eq!(
        harness
            .scheduler
            .activate(Activation::new(
                registration.clone(),
                koru_native_notifications::Action::Open,
            ))
            .unwrap_err()
            .category,
        FailureCategory::StaleActivation
    );

    {
        let mut store = harness.store.lock().unwrap();
        let row = &mut store.snapshots.get_mut(&profile).unwrap().reminders[0];
        row.target.projected = true;
        row.stored.rule.revision = RuleRevisionId::new();
        // Keep the previous reduced occurrence intentionally: revision alone
        // is enough to reject an old platform activation.
    }
    assert_eq!(
        harness
            .scheduler
            .activate(Activation::new(
                registration.clone(),
                koru_native_notifications::Action::Open,
            ))
            .unwrap_err()
            .category,
        FailureCategory::StaleActivation
    );

    {
        let original_revision = harness.ledger.lock().unwrap().state.entries[0].revision;
        harness
            .store
            .lock()
            .unwrap()
            .snapshots
            .get_mut(&profile)
            .unwrap()
            .reminders[0]
            .stored
            .rule
            .revision = original_revision;
        let other_occurrence = current_occurrence(&zoned_row(20_000));
        harness.ledger.lock().unwrap().state.entries[0].occurrence = other_occurrence;
    }
    assert_eq!(
        harness
            .scheduler
            .activate(Activation::new(
                registration,
                koru_native_notifications::Action::Open,
            ))
            .unwrap_err()
            .category,
        FailureCategory::StaleActivation
    );
}

#[test]
fn wait_plan_uses_the_injected_clock_and_ledger_stays_content_free() {
    let profile = ProfileId::new();
    let mut harness = Harness::new(profile, snapshot(vec![zoned_row(10_000)]), 1_000, "UTC");
    harness.scheduler.run(Trigger::Startup).unwrap();
    assert_eq!(
        harness.scheduler.wait_plan().unwrap(),
        WaitPlan::UntilUnixMs(10_000)
    );
    *harness.clock.lock().unwrap() = 10_000;
    assert_eq!(harness.scheduler.wait_plan().unwrap(), WaitPlan::Indefinite);

    let debug = format!("{:?}", harness.ledger.lock().unwrap().state);
    assert!(!debug.contains("private customer title"));
    assert!(!debug.contains("task:"));
}

#[test]
fn durable_event_filter_keeps_all_reminder_and_target_lifecycle_wakes() {
    fn envelope(name: &str, profile: Option<ProfileId>) -> koru_events::Envelope {
        let mut draft = koru_events::Draft::new(koru_events::EventName::new(name));
        if let Some(profile) = profile {
            draft = draft.profile(koru_events::ProfileId(profile.as_uuid()));
        }
        draft.build(
            &koru_events::EmitContext::system("reminder-test"),
            koru_events::EventId::mint(),
            0,
        )
    }

    let profile = ProfileId::new();
    for name in [
        "due.changed",
        "entity.removed",
        "document.created",
        "document.trashed",
        "document.restored",
        "document.removed",
        "workspace.created",
        "workspace.deleted",
        "workspace.observed",
    ] {
        assert_eq!(event_wake(&envelope(name, None)), EventWake::AllProfiles);
    }
    for name in [
        "reminder.rule.changed",
        "reminder.action.appended",
        "reminder.history.cleared",
        "reminder.settings.changed",
    ] {
        assert_eq!(
            event_wake(&envelope(name, Some(profile))),
            EventWake::Profile(profile)
        );
        assert_eq!(event_wake(&envelope(name, None)), EventWake::AllProfiles);
    }
    assert_eq!(
        event_wake(&envelope("workspace.renamed", None)),
        EventWake::Irrelevant
    );
}
