// SPDX-License-Identifier: MPL-2.0

//! P4.3 Piece 1: the Bot Routine owner path on the real coordinator, live
//! Loro and SQLite memory. No Store, provider, timer or network: the clock is
//! the harness's atomic and every pass is driven explicitly by the case.
use super::*;
use koru_delegate_schedule::ScheduleName;

type EventLog = Arc<Mutex<Vec<(&'static str, Option<u32>)>>>;
type Snapshot = Arc<Mutex<Option<Arc<Mutex<Vec<u8>>>>>>;

/// Records each event-port call with the definition revision the Store held
/// at that moment, so ordering against the commit is observable.
struct RecordingEvents {
    log: EventLog,
    source: Snapshot,
}
impl RecordingEvents {
    fn push(&self, call: &'static str) {
        let revision = self.source.lock().unwrap().as_ref().map(|snapshot| {
            ScheduleDocument::from_snapshot(&snapshot.lock().unwrap())
                .unwrap()
                .header()
                .unwrap()
                .definition
                .revision
        });
        self.log.lock().unwrap().push((call, revision));
    }
    fn prepared() -> PreparedScheduleEvent {
        PreparedScheduleEvent {
            event: koru_delegate_core::EventId::mint(),
        }
    }
}
impl ScheduleEvents for RecordingEvents {
    fn maintain(&mut self) -> PortResult<ScheduleEventMaintenance> {
        Ok(ScheduleEventMaintenance::default())
    }
    fn drain_ready(&mut self, _: usize) -> PortResult<ScheduleEventMaintenance> {
        Ok(ScheduleEventMaintenance::default())
    }
    fn reconcile_prepared(&mut self, _: usize) -> PortResult<ScheduleEventMaintenance> {
        Ok(ScheduleEventMaintenance::default())
    }
    fn reconcile_created(&mut self, _: ScheduleId) -> PortResult<()> {
        Ok(())
    }
    fn existing_created(&mut self, _: ScheduleEventRef, _: &ScheduleHeader) -> PortResult<()> {
        Ok(())
    }
    fn prepare_created(
        &mut self,
        _: ScheduleEventRef,
        _: &ScheduleHeader,
    ) -> PortResult<PreparedScheduleEvent> {
        panic!("an owner control creates nothing")
    }
    fn prepare_definition(
        &mut self,
        _: ScheduleEventRef,
        definition: &ScheduleDefinition,
    ) -> PortResult<PreparedScheduleEvent> {
        self.push(match schedule_definition_event(definition) {
            ScheduleDefinitionEvent::Updated => "prepare_updated",
            ScheduleDefinitionEvent::Paused(_) => "prepare_paused",
        });
        Ok(Self::prepared())
    }
    fn prepare_occurrence(
        &mut self,
        _: ScheduleOccurrenceEvent,
    ) -> PortResult<Option<PreparedScheduleEvent>> {
        panic!("the Routine path prepares no ordinary occurrence")
    }
    fn reconcile_occurrence(
        &mut self,
        _: ScheduleEventRef,
        prepared: Option<PreparedScheduleEvent>,
        claim: &SlotClaim,
    ) -> PortResult<Option<PreparedScheduleEvent>> {
        assert!(prepared.is_none());
        self.push(match claim.state {
            SlotState::Settled(SlotOutcome::SkippedBusy) => "skip_busy",
            SlotState::Settled(SlotOutcome::SkippedMissed) => "skip_missed",
            _ => panic!("only a durable skip is projected here"),
        });
        Ok(Some(Self::prepared()))
    }
    fn ready(&mut self, _: PreparedScheduleEvent) -> PortResult<()> {
        self.push("ready");
        Ok(())
    }
    fn abandon(&mut self, _: PreparedScheduleEvent) -> PortResult<()> {
        self.push("abandon");
        Ok(())
    }
}

fn manual() -> ScheduleLifecycle {
    ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::Manual,
    }
}
fn zone() -> koru_time_zone::IanaZoneRules {
    koru_time_zone::IanaZoneRules::parse("UTC").unwrap()
}
/// Revision 1, exactly Paused(Manual), as creation seeds a Routine.
fn paused() -> Composition {
    Composition::assembled(false, None, |_, _, _, _| {}, |_, _| {})
}
fn paused_recording() -> (Composition, EventLog) {
    let log = EventLog::default();
    let source = Snapshot::default();
    let c = Composition::assembled(
        false,
        Some(Box::new(RecordingEvents {
            log: log.clone(),
            source: source.clone(),
        })),
        |_, _, _, _| {},
        |_, _| {},
    );
    *source.lock().unwrap() = Some(c.docs.snapshot.clone());
    (c, log)
}
fn route(c: &Composition) -> RoutineRouteIdentity {
    c.h.schedule_store
        .inner
        .lock()
        .unwrap()
        .routine_route(c.prepared.slot.schedule)
        .unwrap()
        .expect("retained route")
}
fn now(c: &Composition) -> i64 {
    c.h.schedule_clock.load(Ordering::SeqCst)
}
fn set_now(c: &Composition, now_ms: i64) {
    c.h.schedule_clock.store(now_ms, Ordering::SeqCst);
}
/// The host holds its event-authority gate around an owner command.
fn control_as(
    c: &mut Composition,
    route: &RoutineRouteIdentity,
    expected_revision: u32,
    control: RoutineControl,
) -> PortResult<RoutineControlled> {
    c.probe.lock().unwrap().gate = true;
    let now_ms = now(c);
    let result = c
        .coordinator
        .control_routine(route, expected_revision, control, now_ms, &zone());
    c.probe.lock().unwrap().gate = false;
    result
}
fn control(
    c: &mut Composition,
    expected_revision: u32,
    control: RoutineControl,
) -> PortResult<RoutineControlled> {
    let route = route(c);
    control_as(c, &route, expected_revision, control)
}
fn accept(
    c: &mut Composition,
    expected_revision: u32,
    occurrence_ms: i64,
) -> PortResult<RoutineTestAccepted> {
    let route = route(c);
    c.probe.lock().unwrap().gate = true;
    let now_ms = now(c);
    let result =
        c.coordinator
            .accept_routine_test(&route, expected_revision, occurrence_ms, now_ms);
    c.probe.lock().unwrap().gate = false;
    result
}
fn code<T>(result: PortResult<T>) -> String {
    match result {
        Ok(_) => panic!("expected a refusal"),
        Err(error) => error.code,
    }
}
fn evaluate(c: &mut Composition) -> ScheduleEvaluation {
    c.probe.lock().unwrap().gate = true;
    let now_ms = now(c);
    let report = c
        .coordinator
        .evaluate(&mut c.effects, now_ms, &zone())
        .unwrap();
    c.probe.lock().unwrap().gate = false;
    report
}
fn run_work(c: &mut Composition, work: RoutineWork) -> PortResult<ScheduleEvaluation> {
    c.coordinator.evaluate_routine_work(
        work,
        &mut c.effects,
        Instant::now() + Duration::from_secs(5),
    )
}
fn test_work(c: &mut Composition) -> RoutineWork {
    let work = evaluate(c).routine_work.expect("the outstanding Test");
    assert!(matches!(work.candidate, RoutineCandidate::Test { .. }));
    work
}
fn definition(c: &Composition) -> ScheduleDefinition {
    c.docs.read().header().unwrap().definition
}
fn mutate(c: &Composition, write: impl FnOnce(&ScheduleDocument)) {
    let doc = c.docs.read();
    write(&doc);
    *c.docs.snapshot.lock().unwrap() = doc.export_snapshot().unwrap();
}
fn pause_bot(c: &Composition) {
    c.h.schedule_store
        .inner
        .lock()
        .unwrap()
        .apply_bot_barrier(&BotAdmissionBarrier {
            request: BotRequestId::mint(),
            epoch: 2,
            paused: true,
            ..admission(&c.prepared)
        })
        .unwrap();
}
fn ledger_calls(c: &Composition) -> Vec<&'static str> {
    c.probe.lock().unwrap().ledger.clone()
}
fn bridge_calls(c: &Composition) -> Vec<&'static str> {
    c.probe.lock().unwrap().calls.clone()
}
fn name(c: &Composition) -> ScheduleName {
    c.docs.read().name().unwrap()
}
fn edit(c: &Composition, to: &str, cron: &str) -> RoutineControl {
    RoutineControl::Edit {
        expected_name: name(c),
        name: ScheduleName::new(to).unwrap(),
        recurrence: RecurrenceRule::floating(CronExpression::parse(cron).unwrap()),
    }
}
/// Nothing claimed, skipped, prepared or written for the Routine.
fn untouched(c: &Composition, document_unchanged: bool) {
    assert!(document_unchanged, "the Schedule document was written");
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
    assert!(bridge_calls(c).is_empty());
    assert!(
        !ledger_calls(c)
            .iter()
            .any(|call| matches!(*call, "claim" | "claim_test"))
    );
}

// ------------------------------------------------------------------ control

#[test]
fn routine_control_enable_is_refused_while_the_bot_barrier_is_paused_or_absent() {
    let (mut c, events) = paused_recording();
    let before = c.docs.read().version();
    {
        // The port reads the real ledger row: positive absence for a Bot the
        // owner never synchronized, then the row as it is replaced.
        let db = c.h.schedule_store.inner.lock().unwrap();
        let slot = c.prepared.slot;
        assert_eq!(
            RoutineLedger::bot_admission_barrier(&*db, slot.owner, BotId::mint()).unwrap(),
            None
        );
        assert_eq!(
            RoutineLedger::bot_admission_barrier(&*db, slot.owner, slot.bot).unwrap(),
            Some(admission(&c.prepared))
        );
    }
    pause_bot(&c);
    assert!(
        RoutineLedger::bot_admission_barrier(
            &*c.h.schedule_store.inner.lock().unwrap(),
            c.prepared.slot.owner,
            c.prepared.slot.bot
        )
        .unwrap()
        .unwrap()
        .paused
    );
    assert_eq!(
        code(control(&mut c, 1, RoutineControl::Enable)),
        "routine_bot_paused"
    );
    assert_eq!(code(accept(&mut c, 1, 60_000)), "routine_bot_paused");
    c.probe.lock().unwrap().no_barrier = true;
    assert_eq!(
        code(control(&mut c, 1, RoutineControl::Enable)),
        "routine_barrier_absent"
    );
    assert_eq!(code(accept(&mut c, 1, 60_000)), "routine_barrier_absent");
    c.probe.lock().unwrap().no_barrier = false;
    // Refused before anything was prepared or written.
    untouched(&c, c.docs.read().version() == before);
    assert!(events.lock().unwrap().is_empty());
    assert_eq!(definition(&c).lifecycle, manual());
    assert_eq!(c.coordinator.routine_test_outstanding(), None);
    assert_eq!(ledger_calls(&c), ["barrier"; 4]);

    // Pause and Edit only reduce or reshape work: they do not read the barrier.
    let next = edit(&c, "Renamed while the Bot is paused", "5 4 * * *");
    let edited = control(&mut c, 1, next).unwrap();
    assert_eq!(edited.outcome, RoutineControlOutcome::Applied);
    assert_eq!(edited.lifecycle, RoutineControlLifecycle::Paused);
    assert_eq!(
        code(control(&mut c, 2, RoutineControl::Pause)),
        "schedule_state_conflict"
    );
    assert_eq!(ledger_calls(&c), ["barrier"; 4]);
    // The Bot's pause still holds the Routine off after those.
    assert_eq!(
        code(control(&mut c, 2, RoutineControl::Enable)),
        "routine_bot_paused"
    );
    assert_eq!(definition(&c).lifecycle, manual());
}

#[test]
fn routine_control_enable_then_pause_in_either_order_leaves_the_routine_paused() {
    // The owner's own sequence.
    let mut c = paused();
    let on = control(&mut c, 1, RoutineControl::Enable).unwrap();
    assert_eq!(
        (on.outcome, on.lifecycle, on.definition.revision),
        (
            RoutineControlOutcome::Applied,
            RoutineControlLifecycle::Active,
            2
        )
    );
    let off = control(&mut c, 2, RoutineControl::Pause).unwrap();
    assert_eq!(
        (off.outcome, off.lifecycle, off.definition.revision),
        (
            RoutineControlOutcome::Applied,
            RoutineControlLifecycle::Paused,
            3
        )
    );
    assert_eq!(definition(&c).lifecycle, manual());
    assert!(evaluate(&mut c).routine_work.is_none());

    // Enable racing a Trash, Enable handled first: its commit lands, then the
    // driver's Pause (sent after the Bot's pause is in the ledger) turns it off.
    let mut c = paused();
    control(&mut c, 1, RoutineControl::Enable).unwrap();
    pause_bot(&c);
    let driver = control(&mut c, 2, RoutineControl::Pause).unwrap();
    assert_eq!(driver.lifecycle, RoutineControlLifecycle::Paused);
    for stale_or_current in [1, 2, 3] {
        assert_eq!(
            code(control(&mut c, stale_or_current, RoutineControl::Enable)),
            "routine_bot_paused"
        );
    }
    assert_eq!(definition(&c).lifecycle, manual());
    assert!(evaluate(&mut c).routine_work.is_none());

    // Enable handled second: the driver finds it already paused, and the
    // Enable is refused by the Bot's barrier. Never on inside the operation.
    let mut c = paused();
    pause_bot(&c);
    assert_eq!(
        code(control(&mut c, 1, RoutineControl::Pause)),
        "schedule_state_conflict"
    );
    assert_eq!(
        code(control(&mut c, 1, RoutineControl::Enable)),
        "routine_bot_paused"
    );
    assert_eq!(definition(&c).revision, 1);
    assert_eq!(definition(&c).lifecycle, manual());
    assert!(evaluate(&mut c).routine_work.is_none());
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
}

#[test]
fn routine_control_lost_reply_replay_is_already_applied_and_a_later_revision_conflicts() {
    let mut c = paused();
    let first = control(&mut c, 1, RoutineControl::Enable).unwrap();
    assert_eq!(first.outcome, RoutineControlOutcome::Applied);
    assert_eq!(first.effective_at_ms, 61_000);
    let written = c.docs.read().version();
    // The reply was lost; the clock moved; the same command is sent again.
    set_now(&c, 61_900);
    let replay = control(&mut c, 1, RoutineControl::Enable).unwrap();
    assert_eq!(replay.outcome, RoutineControlOutcome::AlreadyApplied);
    assert_eq!(
        (replay.definition, replay.effective_at_ms, replay.lifecycle),
        (first.definition, first.effective_at_ms, first.lifecycle)
    );
    assert_eq!(c.docs.read().version(), written, "a replay writes nothing");

    let paused = control(&mut c, 2, RoutineControl::Pause).unwrap();
    assert_eq!(paused.outcome, RoutineControlOutcome::Applied);
    let written = c.docs.read().version();
    let replay = control(&mut c, 2, RoutineControl::Pause).unwrap();
    assert_eq!(replay.outcome, RoutineControlOutcome::AlreadyApplied);
    assert_eq!(replay.definition, paused.definition);
    assert_eq!(c.docs.read().version(), written);
    // A later revision exists: the older commands are conflicts, not replays,
    // and no inverse is ever applied.
    assert_eq!(
        code(control(&mut c, 1, RoutineControl::Pause)),
        "schedule_revision_conflict"
    );
    assert_eq!(
        code(control(&mut c, 1, RoutineControl::Enable)),
        "schedule_revision_conflict"
    );

    let change = edit(&c, "Weekly digest", "0 8 1 * *");
    let edited = control(&mut c, 3, change.clone()).unwrap();
    assert_eq!(edited.outcome, RoutineControlOutcome::Applied);
    assert_eq!(edited.name, ScheduleName::new("Weekly digest").unwrap());
    let written = c.docs.read().version();
    set_now(&c, 62_500);
    // The replay still carries the name the form displayed before the edit.
    let replay = control(&mut c, 3, change.clone()).unwrap();
    assert_eq!(replay.outcome, RoutineControlOutcome::AlreadyApplied);
    assert_eq!(
        (replay.definition, replay.effective_at_ms, &replay.name),
        (edited.definition, edited.effective_at_ms, &edited.name)
    );
    assert_eq!(c.docs.read().version(), written);
    // The same revision with other content is not that replay.
    let RoutineControl::Edit { expected_name, .. } = change else {
        unreachable!()
    };
    let other = RoutineControl::Edit {
        expected_name,
        name: ScheduleName::new("Something else").unwrap(),
        recurrence: RecurrenceRule::floating(CronExpression::parse("0 8 1 * *").unwrap()),
    };
    assert_eq!(
        code(control(&mut c, 3, other)),
        "schedule_revision_conflict"
    );
    assert_eq!(c.docs.read().version(), written);
    assert_eq!(definition(&c).lifecycle, manual());
}

#[test]
fn routine_control_edit_replaces_only_name_and_timing_while_paused_or_on() {
    for on in [false, true] {
        let mut c = paused();
        let mut revision = 1;
        if on {
            control(&mut c, 1, RoutineControl::Enable).unwrap();
            revision = 2;
        }
        let before = definition(&c);
        set_now(&c, 61_700);
        let change = edit(&c, "Morning summary", "30 7 * * *");
        let edited = control(&mut c, revision, change).unwrap();
        let after = definition(&c);
        assert_eq!(edited.outcome, RoutineControlOutcome::Applied);
        assert_eq!(edited.definition.revision, revision + 1);
        assert_eq!(edited.effective_at_ms, 61_700);
        assert_eq!(after.effective_at_ms, 61_700);
        assert_eq!(name(&c), ScheduleName::new("Morning summary").unwrap());
        assert_eq!(
            after.recurrence,
            RecurrenceRule::floating(CronExpression::parse("30 7 * * *").unwrap())
        );
        // Task text, context, inert execution fields, association and
        // lifecycle are copied, never supplied.
        assert_eq!(after.template, before.template);
        assert_eq!(after.routine, before.routine);
        assert_eq!(after.lifecycle, before.lifecycle);
        assert_eq!(
            edited.lifecycle,
            if on {
                RoutineControlLifecycle::Active
            } else {
                RoutineControlLifecycle::Paused
            }
        );
        assert_eq!(
            edited.definition.digest,
            routine_definition_digest(&after, c.prepared.submission.workspace, None).unwrap()
        );
        // A form that displayed another name saves nothing.
        let written = c.docs.read().version();
        let stale = RoutineControl::Edit {
            expected_name: ScheduleName::new("A name it never had").unwrap(),
            name: ScheduleName::new("Overwrite").unwrap(),
            recurrence: after.recurrence.clone(),
        };
        assert_eq!(
            code(control(&mut c, revision + 1, stale)),
            "schedule_name_conflict"
        );
        assert_eq!(c.docs.read().version(), written);
        // No occurrence of the old rule before the new effective time is due.
        assert!(evaluate(&mut c).routine_work.is_none());
    }
}

#[test]
fn routine_control_route_must_be_the_retained_route_at_its_exact_document() {
    let mut c = paused();
    let before = c.docs.read().version();
    let retained = route(&c);
    for field in 0..5 {
        let mut offered = retained.clone();
        match field {
            0 => offered.seed_hash = RoutineDigest32::from_bytes([0xee; 32]),
            1 => offered.created_at_ms += 1,
            2 => offered.creation_request = BotRequestId::mint(),
            3 => offered.workspace = WorkspaceId::from_uuid(v4(0xfd)),
            _ => offered.bot = BotId::mint(),
        }
        assert_eq!(
            code(control_as(&mut c, &offered, 1, RoutineControl::Enable)),
            "routine_route_mismatch",
            "field {field}"
        );
    }
    let mut elsewhere = retained.clone();
    elsewhere.owner.device = DeviceId::from_uuid(v4(0xfc));
    assert_eq!(
        code(control_as(&mut c, &elsewhere, 1, RoutineControl::Pause)),
        "schedule_not_owner"
    );
    assert_eq!(
        code(control(&mut c, 0, RoutineControl::Enable)),
        "routine_control_invalid"
    );
    untouched(&c, c.docs.read().version() == before);

    // A lower-sorted copy of the header elsewhere does not capture the owner
    // path: it opens the exact location, where the ordinary path would refuse.
    let mut c = Composition::assembled(false, None, |_, _, _, _| {}, |docs, _| docs.alias = true);
    let workspace = c.prepared.submission.workspace;
    let schedule = c.prepared.slot.schedule;
    c.probe.lock().unwrap().gate = true;
    assert!(
        c.coordinator
            .pause_schedule(workspace, schedule, 1, 61_000, &zone())
            .is_err()
    );
    c.probe.lock().unwrap().gate = false;
    let on = control(&mut c, 1, RoutineControl::Enable).unwrap();
    assert_eq!(on.lifecycle, RoutineControlLifecycle::Active);
    assert_eq!(definition(&c).lifecycle, ScheduleLifecycle::Active);
}

#[test]
fn routine_control_answers_only_after_the_commit_and_makes_its_event_ready_last() {
    let (mut c, events) = paused_recording();
    control(&mut c, 1, RoutineControl::Enable).unwrap();
    control(&mut c, 2, RoutineControl::Pause).unwrap();
    let change = edit(&c, "Edited", "0 6 * * *");
    control(&mut c, 3, change).unwrap();
    // Prepared against the old revision, made Ready only once the Store holds
    // the new one. Enable and Edit are `updated`; Pause is `paused`.
    assert_eq!(
        *events.lock().unwrap(),
        [
            ("prepare_updated", Some(1)),
            ("ready", Some(2)),
            ("prepare_paused", Some(2)),
            ("ready", Some(3)),
            ("prepare_paused", Some(3)),
            ("ready", Some(4)),
        ]
    );
    // A refusal the document makes leaves a prepared event that is never Ready.
    events.lock().unwrap().clear();
    assert_eq!(
        code(control(&mut c, 2, RoutineControl::Enable)),
        "schedule_revision_conflict"
    );
    assert_eq!(*events.lock().unwrap(), [("prepare_updated", Some(4))]);
}

#[test]
fn routine_control_document_and_catalog_refusals_write_nothing() {
    let owner = paused().prepared.slot.owner.device;
    let other = DeviceId::from_uuid(v4(0xfb));
    type Break = fn(&Composition, DeviceId, DeviceId);
    let cases: [(&str, Break); 7] = [
        ("schedule_retired", |c, owner, _| {
            mutate(c, |doc| {
                doc.retire(owner, 500).unwrap();
            })
        }),
        ("schedule_handoff_pending", |c, _, other| {
            mutate(c, |doc| {
                doc.request_handoff(HandoffId::mint(), other, other, 500)
                    .unwrap();
            })
        }),
        ("schedule_not_owner", |c, owner, other| {
            mutate(c, |doc| {
                let id = HandoffId::mint();
                doc.request_handoff(id, other, other, 500).unwrap();
                doc.confirm_handoff(id, owner, 600).unwrap();
            })
        }),
        ("schedule_read_only", |c, _, _| {
            mutate(c, |doc| {
                doc.doc()
                    .get_map("schedule")
                    .insert("schema", 3i64)
                    .unwrap();
                doc.doc().commit();
            })
        }),
        ("schedule_unavailable", |c, _, _| {
            c.probe.lock().unwrap().partial_catalog = true
        }),
        ("schedule_not_found", |c, _, _| {
            c.probe.lock().unwrap().empty_catalog = true
        }),
        ("schedule_revision_conflict", |_, _, _| {}),
    ];
    for (expected, break_it) in cases {
        let mut c = paused();
        break_it(&c, owner, other);
        let before = c.docs.read().version();
        let revision = if expected == "schedule_revision_conflict" {
            7
        } else {
            1
        };
        for request in [
            RoutineControl::Enable,
            RoutineControl::Pause,
            edit_for_refusal(),
        ] {
            let refused = code(control(&mut c, revision, request));
            assert_eq!(refused, expected, "{expected}");
        }
        assert_eq!(code(accept(&mut c, revision, 60_000)), expected);
        assert_eq!(c.docs.read().version(), before, "{expected}");
        assert_eq!(c.coordinator.routine_test_outstanding(), None);
        assert!(bridge_calls(&c).is_empty());
    }
}
fn edit_for_refusal() -> RoutineControl {
    RoutineControl::Edit {
        expected_name: ScheduleName::new("unused").unwrap(),
        name: ScheduleName::new("unused too").unwrap(),
        recurrence: RecurrenceRule::floating(CronExpression::parse("0 6 * * *").unwrap()),
    }
}

#[test]
fn routine_control_state_conflicts_and_the_ordinary_paths_still_refuse_a_bot_routine() {
    let mut c = paused();
    let workspace = c.prepared.submission.workspace;
    let schedule = c.prepared.slot.schedule;
    let before = c.docs.read().version();
    // Already in the requested state, and not the replay of this command.
    assert_eq!(
        code(control(&mut c, 1, RoutineControl::Pause)),
        "schedule_state_conflict"
    );
    // The ordinary Resume and Edit keep answering as before P4.3.
    c.probe.lock().unwrap().gate = true;
    let resumed = c
        .coordinator
        .resume_schedule(workspace, schedule, 1, 61_000, &zone());
    let template = definition(&c).template;
    let edited = c.coordinator.edit_schedule(
        workspace,
        schedule,
        1,
        &name(&c),
        &name(&c),
        template,
        RecurrenceRule::floating(CronExpression::parse("0 6 * * *").unwrap()),
        61_000,
        &zone(),
    );
    c.probe.lock().unwrap().gate = false;
    assert_eq!(code(resumed), "bot_routine_unavailable");
    assert_eq!(code(edited), "bot_routine_unavailable");
    assert_eq!(c.docs.read().version(), before);

    control(&mut c, 1, RoutineControl::Enable).unwrap();
    let written = c.docs.read().version();
    assert_eq!(
        code(control(&mut c, 2, RoutineControl::Enable)),
        "schedule_state_conflict"
    );
    assert_eq!(c.docs.read().version(), written);
    // A Routine paused for a reason that is not the owner's: Pause conflicts,
    // and a Test requires exactly the owner's pause.
    mutate(&c, |doc| {
        let mut automatic = doc.header().unwrap().definition;
        automatic.revision = 3;
        automatic.effective_at_ms += 1;
        automatic.lifecycle = ScheduleLifecycle::Paused {
            reason: SchedulePauseReason::ScopeUnavailable,
        };
        doc.update_definition(c.prepared.slot.owner.device, &automatic)
            .unwrap();
    });
    assert_eq!(
        code(control(&mut c, 3, RoutineControl::Pause)),
        "schedule_state_conflict"
    );
    set_now(&c, 121_000);
    assert_eq!(
        code(accept(&mut c, 3, 120_000)),
        "routine_test_requires_paused"
    );
}

// --------------------------------------------------------------------- test

#[test]
fn routine_control_test_claims_exactly_once_through_its_own_authority_and_cannot_double_fire() {
    let mut c = paused();
    c.probe.lock().unwrap().missing = true;
    let accepted = accept(&mut c, 1, 60_000).unwrap();
    assert_eq!(accepted.slot, c.prepared.slot);
    assert_eq!(accepted.definition, c.prepared.definition);
    // Acceptance is memory only.
    assert_eq!(
        c.coordinator.routine_test_outstanding(),
        Some(accepted.slot)
    );
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
    // The acceptance's reply was lost: the same command is the same Test.
    assert_eq!(accept(&mut c, 1, 60_000).unwrap(), accepted);

    let work = test_work(&mut c);
    assert!(run_work(&mut c, work).unwrap().started.is_empty());
    let RoutineClaimObservation::Claimed { record } = c.observation() else {
        panic!("the Test's claim")
    };
    assert_eq!(record.claim.ids, c.prepared.ids);
    assert_eq!(record.claim.definition, accepted.definition);
    assert_eq!(record.claim.definition.revision, 1);
    assert_eq!(record.link_state, RoutineLinkState::Linked {});
    // Its own port method, never the ordinary one; the paused definition went
    // to the AI owner; and the due path's cursor was never touched.
    assert_eq!(ledger_calls(&c), ["barrier", "barrier", "claim_test"]);
    assert_eq!(c.probe.lock().unwrap().prepared_paused, [true]);
    assert_eq!(
        bridge_calls(&c),
        [
            "read_preparation",
            "prepare",
            "preclaim",
            "begin",
            "release",
            "read_link",
            "finalize"
        ]
    );
    assert_eq!(c.coordinator.routine_attempted_slot(), None);
    assert_eq!(c.coordinator.routine_test_outstanding(), None);
    // The Routine is still paused and still unclaimable by the ordinary path.
    let doc = c.docs.read();
    assert_eq!(doc.header().unwrap().definition.lifecycle, manual());
    assert_eq!(doc.header().unwrap().definition.revision, 1);
    assert!(!doc.may_claim(c.prepared.slot.owner.device).unwrap());
    assert!(doc.firings().unwrap().is_empty());
    c.no_launch();

    // Asked again in the same minute: the same acceptance, and nothing runs.
    let settled = bridge_calls(&c);
    assert_eq!(accept(&mut c, 1, 60_000).unwrap(), accepted);
    assert_eq!(c.coordinator.routine_test_outstanding(), None);
    for _ in 0..3 {
        assert!(evaluate(&mut c).routine_work.is_none());
    }
    assert_eq!(bridge_calls(&c), settled);
    assert_eq!(
        ledger_calls(&c)
            .iter()
            .filter(|call| **call == "claim_test")
            .count(),
        1
    );
    let RoutineClaimObservation::Claimed { record: again } = c.observation() else {
        panic!("the same claim")
    };
    assert_eq!(again.claim, record.claim);
    // A later minute is a different Test and is never inferred from this one.
    set_now(&c, 121_000);
    assert_eq!(
        code(accept(&mut c, 1, 60_000)),
        "routine_test_minute_changed"
    );
    assert!(evaluate(&mut c).routine_work.is_none());
}

#[test]
fn routine_control_test_work_cannot_be_run_twice_or_after_its_slot_resolved() {
    let mut c = paused();
    accept(&mut c, 1, 60_000).unwrap();
    // Two passes select the same outstanding slot. Work from an older pass is
    // refused, and refusing it spends the newer pass's serial too, exactly as
    // for a due candidate: neither reaches the bridge or the ledger.
    let stale = test_work(&mut c);
    let current = test_work(&mut c);
    assert_eq!(code(run_work(&mut c, stale)), "routine_stale_work");
    assert_eq!(code(run_work(&mut c, current)), "routine_stale_work");
    assert!(bridge_calls(&c).is_empty());
    assert_eq!(ledger_calls(&c), ["barrier"]);
    // The Test is still outstanding, so the next pass selects it and it runs
    // once.
    assert_eq!(
        c.coordinator.routine_test_outstanding(),
        Some(c.prepared.slot)
    );
    let work = test_work(&mut c);
    run_work(&mut c, work).unwrap();
    assert_eq!(ledger_calls(&c), ["barrier", "claim_test"]);
    // No candidate is left to select, so nothing can claim the minute again.
    assert_eq!(c.coordinator.routine_test_outstanding(), None);
    assert!(evaluate(&mut c).routine_work.is_none());
    assert_eq!(
        ledger_calls(&c)
            .iter()
            .filter(|call| **call == "claim_test")
            .count(),
        1
    );
}

#[test]
fn routine_control_test_candidate_is_dropped_without_a_write_after_its_minute() {
    // Never selected before the margin: forgotten at selection.
    for late in [118_000, 119_999, 120_000, 181_000] {
        let mut c = paused();
        let before = c.docs.read().version();
        accept(&mut c, 1, 60_000).unwrap();
        set_now(&c, late);
        assert!(evaluate(&mut c).routine_work.is_none(), "{late}");
        assert_eq!(c.coordinator.routine_test_outstanding(), None);
        untouched(&c, c.docs.read().version() == before);
        assert_eq!(c.coordinator.routine_attempted_slot(), None);
    }
    // Selected in time, run after the margin: dropped by the candidate itself.
    for late in [118_000, 121_000] {
        let mut c = paused();
        let before = c.docs.read().version();
        accept(&mut c, 1, 60_000).unwrap();
        let work = test_work(&mut c);
        set_now(&c, late);
        run_work(&mut c, work).unwrap();
        assert_eq!(c.coordinator.routine_test_outstanding(), None);
        untouched(&c, c.docs.read().version() == before);
        assert_eq!(c.coordinator.routine_attempted_slot(), None);
        // The ended minute is not expired into a "Missed", and the paused
        // Routine has nothing due in the next one.
        for _ in 0..2 {
            assert!(evaluate(&mut c).routine_work.is_none());
        }
        let db = c.h.schedule_store.inner.lock().unwrap();
        for occurrence_ms in [60_000, 120_000] {
            assert!(matches!(
                db.read_routine_claim(&RoutineSlot {
                    occurrence_ms,
                    ..c.prepared.slot
                })
                .unwrap(),
                RoutineClaimObservation::Absent { .. }
            ));
        }
    }
    // One second before the margin it still claims.
    let mut c = paused();
    accept(&mut c, 1, 60_000).unwrap();
    set_now(&c, 117_999);
    let work = test_work(&mut c);
    run_work(&mut c, work).unwrap();
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Claimed { .. }
    ));
}

#[test]
fn routine_control_enable_racing_test_drops_the_test_and_never_claims_twice() {
    // Enable is handled before the Test's pass.
    let mut c = paused();
    accept(&mut c, 1, 60_000).unwrap();
    let on = control(&mut c, 1, RoutineControl::Enable).unwrap();
    assert_eq!(on.effective_at_ms, 61_000);
    let before = c.docs.read().version();
    let work = test_work(&mut c);
    run_work(&mut c, work).unwrap();
    // It reached the live document, found another definition and stopped:
    // no authority minted, no claim method called, the lease released.
    assert_eq!(
        bridge_calls(&c),
        ["read_preparation", "preclaim", "begin", "release"]
    );
    assert_eq!(ledger_calls(&c), ["barrier", "barrier"]);
    assert!(c.probe.lock().unwrap().lease.is_none());
    assert_eq!(c.docs.read().version(), before);
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
    assert_eq!(c.coordinator.routine_test_outstanding(), None);
    // The Routine is on from 61_000: the 60_000 slot is not due under it.
    let report = evaluate(&mut c);
    assert!(report.routine_work.is_none());
    assert_eq!(report.next_deadline_ms, Some(120_000));

    // Enable lands after the Test's work was selected, before it runs.
    let mut c = paused();
    accept(&mut c, 1, 60_000).unwrap();
    let work = test_work(&mut c);
    control(&mut c, 1, RoutineControl::Enable).unwrap();
    run_work(&mut c, work).unwrap();
    assert!(
        !ledger_calls(&c)
            .iter()
            .any(|call| matches!(*call, "claim" | "claim_test"))
    );
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));

    // The Test claims first: it keeps its slot, and the Enable that follows
    // neither removes that claim nor plans the same minute again.
    let mut c = paused();
    accept(&mut c, 1, 60_000).unwrap();
    let work = test_work(&mut c);
    run_work(&mut c, work).unwrap();
    let RoutineClaimObservation::Claimed { record } = c.observation() else {
        panic!("the Test's claim")
    };
    control(&mut c, 1, RoutineControl::Enable).unwrap();
    for _ in 0..3 {
        assert!(evaluate(&mut c).routine_work.is_none());
    }
    let RoutineClaimObservation::Claimed { record: kept } = c.observation() else {
        panic!("the claim survives the Enable")
    };
    assert_eq!(kept.claim, record.claim);
    assert_eq!(kept.claim.definition.revision, 1);
    assert_eq!(ledger_calls(&c), ["barrier", "claim_test", "barrier"]);
    assert_eq!(definition(&c).lifecycle, ScheduleLifecycle::Active);
}

#[test]
fn routine_control_a_paused_routine_is_never_selected_as_due_or_claimed_by_the_ordinary_path() {
    let mut c = paused();
    let before = c.docs.read().version();
    for at in [61_000, 119_000, 121_000, 181_000, 3_661_000] {
        set_now(&c, at);
        for _ in 0..2 {
            assert!(evaluate(&mut c).routine_work.is_none(), "{at}");
        }
    }
    untouched(&c, c.docs.read().version() == before);
    assert!(ledger_calls(&c).is_empty());
    assert_eq!(c.coordinator.routine_attempted_slot(), None);
    // The ordinary and expiry authorities cannot be minted for it at all.
    let doc = c.docs.read();
    let device = c.prepared.slot.owner.device;
    let workspace = c.prepared.submission.workspace;
    assert!(!doc.may_claim(device).unwrap());
    assert!(
        doc.claim_authority(
            workspace,
            device,
            ScheduleClaimPlan::BotRoutine {
                prepared: c.prepared.clone(),
                representative_miss: None,
            },
        )
        .unwrap()
        .is_none()
    );
    assert!(
        doc.expired_routine_authority(
            workspace,
            device,
            koru_delegate_document::RoutineExpiryPlan {
                occurrence_ms: 60_000,
                planned_run: RunId::mint(),
                representative_miss: None,
            },
        )
        .unwrap()
        .is_none()
    );
    // Turned on and paused again: still nothing is due, in this minute or later.
    set_now(&c, 61_000);
    control(&mut c, 1, RoutineControl::Enable).unwrap();
    control(&mut c, 2, RoutineControl::Pause).unwrap();
    for at in [61_500, 121_000, 181_000] {
        set_now(&c, at);
        assert!(evaluate(&mut c).routine_work.is_none(), "{at}");
    }
    assert!(bridge_calls(&c).is_empty());
    assert!(ledger_calls(&c).iter().all(|call| *call == "barrier"));
    // An accepted Test that was dropped leaves it exactly as unselectable.
    accept(&mut c, 3, 180_000).unwrap();
    set_now(&c, 241_000);
    assert!(evaluate(&mut c).routine_work.is_none());
    assert_eq!(c.coordinator.routine_test_outstanding(), None);
    assert!(bridge_calls(&c).is_empty());
}

#[test]
fn routine_control_test_acceptance_refusals_store_no_slot() {
    let mut c = paused();
    let before = c.docs.read().version();
    for (revision, minute, expected) in [
        (1, 60_001, "routine_control_invalid"),
        (1, -60_000, "routine_control_invalid"),
        (0, 60_000, "routine_control_invalid"),
        (2, 60_000, "schedule_revision_conflict"),
        (1, 0, "routine_test_minute_changed"),
        (1, 120_000, "routine_test_minute_changed"),
    ] {
        assert_eq!(code(accept(&mut c, revision, minute)), expected);
        assert_eq!(c.coordinator.routine_test_outstanding(), None);
    }
    let mut foreign = route(&c);
    foreign.seed_hash = RoutineDigest32::from_bytes([0xee; 32]);
    c.probe.lock().unwrap().gate = true;
    let refused = c
        .coordinator
        .accept_routine_test(&foreign, 1, 60_000, 61_000);
    c.probe.lock().unwrap().gate = false;
    assert_eq!(code(refused), "routine_route_mismatch");
    untouched(&c, c.docs.read().version() == before);

    // An Active Routine has no Test; pausing it inside the minute makes the
    // minute start earlier than the definition, which the next minute cures.
    control(&mut c, 1, RoutineControl::Enable).unwrap();
    assert_eq!(
        code(accept(&mut c, 2, 60_000)),
        "routine_test_requires_paused"
    );
    control(&mut c, 2, RoutineControl::Pause).unwrap();
    assert_eq!(code(accept(&mut c, 3, 60_000)), "routine_test_too_soon");
    assert_eq!(c.coordinator.routine_test_outstanding(), None);
    set_now(&c, 120_000);
    let accepted = accept(&mut c, 3, 120_000).unwrap();
    assert_eq!(accepted.definition.revision, 3);

    // One Test per delegate. With the clock stepped back, a second slot is a
    // different Test while the first can still claim.
    set_now(&c, 119_000);
    assert_eq!(
        code(accept(&mut c, 3, 60_000)),
        "routine_test_too_soon",
        "the effective-time rule comes first"
    );
    let mut c = paused();
    accept(&mut c, 1, 60_000).unwrap();
    set_now(&c, 59_000);
    assert_eq!(code(accept(&mut c, 1, 0)), "routine_test_busy");
    assert_eq!(
        c.coordinator.routine_test_outstanding(),
        Some(c.prepared.slot)
    );
    // Once the first Test's minute has ended it no longer blocks another.
    set_now(&c, 121_000);
    let next = accept(&mut c, 1, 120_000).unwrap();
    assert_eq!(c.coordinator.routine_test_outstanding(), Some(next.slot));
}

#[test]
fn routine_control_test_that_finds_the_bot_busy_records_the_skip_and_takes_the_minute() {
    let log = EventLog::default();
    let source = Snapshot::default();
    let holder = Arc::new(Mutex::new(None));
    let seeded = holder.clone();
    let mut c = Composition::assembled(
        false,
        Some(Box::new(RecordingEvents {
            log: log.clone(),
            source: source.clone(),
        })),
        // An earlier ordinary occurrence of the same Bot is still unsettled.
        move |h, doc, p, fresh| {
            *seeded.lock().unwrap() = Some(claim(h, doc, p, fresh, false).claim.ids.run);
        },
        |_, _| {},
    );
    *source.lock().unwrap() = Some(c.docs.snapshot.clone());
    let holder = holder.lock().unwrap().expect("seeded holder");
    assert_ne!(holder, c.prepared.ids.run);
    accept(&mut c, 1, 60_000).unwrap();
    let work = test_work(&mut c);
    run_work(&mut c, work).unwrap();
    let RoutineClaimObservation::Skipped { record } = c.observation() else {
        panic!("the busy skip")
    };
    assert_eq!(record.outcome, RoutineSkipOutcome::SkippedBusy { holder });
    assert_eq!(record.definition_revision, 1);
    assert_eq!(ledger_calls(&c), ["barrier", "claim_test"]);
    // The skip is projected and published for a Routine that is paused.
    let doc = c.docs.read();
    assert_eq!(doc.header().unwrap().definition.lifecycle, manual());
    let firings = doc.firings().unwrap();
    assert_eq!(firings.len(), 1);
    assert_eq!(
        (
            firings[0].occurrence_ms,
            firings[0].revision,
            firings[0].outcome,
            firings[0].busy_holder
        ),
        (60_000, 1, ScheduleFiringOutcome::SkippedBusy, Some(holder))
    );
    assert_eq!(
        *log.lock().unwrap(),
        [("skip_busy", Some(1)), ("ready", Some(1))]
    );
    assert_eq!(c.coordinator.routine_test_outstanding(), None);
    assert_eq!(c.coordinator.routine_attempted_slot(), None);
    // The minute now holds a skip that is not a claim: it is taken.
    assert_eq!(code(accept(&mut c, 1, 60_000)), "routine_test_minute_taken");
    assert_eq!(c.coordinator.routine_test_outstanding(), None);
}

#[test]
fn routine_control_test_that_crosses_the_minute_inside_the_claim_is_the_ledgers_miss() {
    let mut c = paused();
    accept(&mut c, 1, 60_000).unwrap();
    let work = test_work(&mut c);
    // The clock crosses after the candidate's own margin check, while the
    // authority is minted: the ledger's late branch, which no margin removes.
    c.probe.lock().unwrap().late_mint_ms = Some(120_000);
    run_work(&mut c, work).unwrap();
    let RoutineClaimObservation::Skipped { record } = c.observation() else {
        panic!("the ledger's miss")
    };
    assert_eq!(record.outcome, RoutineSkipOutcome::SkippedMissed {});
    assert_eq!(ledger_calls(&c), ["barrier", "claim_test"]);
    let firings = c.docs.read().firings().unwrap();
    assert_eq!(firings.len(), 1);
    assert_eq!(firings[0].outcome, ScheduleFiringOutcome::SkippedMissed);
    assert_eq!(definition(&c).lifecycle, manual());
    c.no_launch();
}

#[test]
fn routine_control_test_retries_only_inside_its_minute_and_yields_to_other_work() {
    let mut c = Composition::assembled(
        false,
        None,
        |h, doc, p, fresh| {
            claim(h, doc, p, fresh, false);
        },
        |_, _| {},
    );
    accept(&mut c, 1, 60_000).unwrap();
    // The AI owner cannot be reached: the Test stays outstanding and retries,
    // but every other pass goes to the held claim that is also waiting.
    c.probe.lock().unwrap().unavailable = true;
    let mut kinds = Vec::new();
    for _ in 0..6 {
        let work = evaluate(&mut c).routine_work.expect("work");
        kinds.push(match &work.candidate {
            RoutineCandidate::Test { .. } => "test",
            RoutineCandidate::Recover { .. } => "recover",
            RoutineCandidate::Due { .. } => "due",
        });
        assert!(run_work(&mut c, work).is_err());
    }
    assert_eq!(
        kinds,
        ["test", "recover", "test", "recover", "test", "recover"]
    );
    assert_eq!(
        c.coordinator.routine_test_outstanding(),
        Some(c.prepared.slot)
    );
    assert!(
        !ledger_calls(&c)
            .iter()
            .any(|call| matches!(*call, "claim" | "claim_test"))
    );
    // The retry ends with the minute, with nothing recorded for the slot.
    set_now(&c, 118_000);
    let work = evaluate(&mut c).routine_work.expect("recovery only");
    assert!(matches!(work.candidate, RoutineCandidate::Recover { .. }));
    assert_eq!(c.coordinator.routine_test_outstanding(), None);
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
    assert_eq!(c.coordinator.routine_attempted_slot(), None);
}

#[test]
fn routine_control_test_ends_on_an_answered_refusal_and_a_closed_preparation() {
    // The AI owner answers and refuses the preclaim (for example the Bot was
    // paused after acceptance): the Test ends there.
    let mut c = paused();
    let before = c.docs.read().version();
    accept(&mut c, 1, 60_000).unwrap();
    c.probe.lock().unwrap().refuse_preclaim = true;
    let work = test_work(&mut c);
    assert!(run_work(&mut c, work).is_err());
    assert_eq!(c.coordinator.routine_test_outstanding(), None);
    assert_eq!(bridge_calls(&c), ["read_preparation", "preclaim"]);
    assert_eq!(c.docs.read().version(), before);
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
    assert!(evaluate(&mut c).routine_work.is_none());

    // A preparation that does not carry the accepted paused definition.
    let mut c = paused();
    accept(&mut c, 1, 60_000).unwrap();
    c.probe.lock().unwrap().bad_prepared = true;
    let work = test_work(&mut c);
    assert_eq!(code(run_work(&mut c, work)), "routine_prepared_mismatch");
    assert_eq!(c.coordinator.routine_test_outstanding(), None);
    assert_eq!(bridge_calls(&c), ["read_preparation"]);
    assert!(evaluate(&mut c).routine_work.is_none());
}
