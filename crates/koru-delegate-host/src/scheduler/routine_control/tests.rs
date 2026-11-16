// SPDX-License-Identifier: MPL-2.0
//! Delegate IPC 17 at the host: the wire mapping, and the owner path composed
//! with the real shared SQLite ledger, the real actor rendezvous and the real
//! reverse bridge. In-memory SQL and a loopback fake gateway; no Store,
//! provider, timer or wall clock (the fixture clock is fixed at 61 s).
use super::*;
use crate::actor::tests::routine::Fixture;
use crate::ledger::SharedLedger;
use crate::routine_bridge::{
    HostRoutineBridge,
    tests::{accept, authority, gateway, receive, send},
};
use crate::scheduler::routine::tests::composition::{Clock, Docs, Preparation};
use koru_delegate_api::bots::{RoutineControlLifecycle, RoutineControlOutcome};
use koru_delegate_core::*;
use koru_delegate_document::{ScheduleClaimPlan, ScheduleDocument};
use koru_delegate_runtime::{RoutineLedger, ScheduleLedger, SchedulePhaseGate};
use koru_delegate_schedule::*;
use std::sync::atomic::AtomicUsize;

fn zone() -> koru_time_zone::IanaZoneRules {
    koru_time_zone::IanaZoneRules::parse("UTC").unwrap()
}
fn manual() -> ScheduleLifecycle {
    ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::Manual,
    }
}

/// The real ledger and actor with one Routine as creation seeds it: revision
/// 1, exactly Paused(Manual), and a prepared occurrence of minute 60 000
/// bound to that paused definition.
struct Paused {
    f: Fixture,
    ledger: SharedLedger,
    route: RoutineRouteIdentity,
    docs: Docs,
    gate: Arc<ScheduleEventAuthorityGate>,
}
impl Paused {
    fn new() -> Self {
        let ledger = SharedLedger::new(
            koru_delegate_sqlite::RunsDb::open_with_clock(
                std::path::Path::new(":memory:"),
                Box::new(Clock),
            )
            .unwrap(),
        );
        let mut f = Fixture::with_store(Box::new(ledger.clone()));
        let route = RoutineLedger::routine_route(&ledger, f.prepared.slot.schedule)
            .unwrap()
            .unwrap();
        let p = &mut f.prepared;
        let definition = ScheduleDefinition {
            revision: 1,
            effective_at_ms: 0,
            template: DelegationTemplate {
                requested_by: p.submission.request.requested_by.clone(),
                objective: p.submission.request.objective.clone(),
                agent: p.submission.request.agent.clone(),
                requested_model: None,
                scope: p.submission.request.scope.clone(),
                grants: [p.submission.workspace].into_iter().collect(),
                context: vec![],
            },
            recurrence: RecurrenceRule::floating("* * * * *".parse().unwrap()),
            lifecycle: manual(),
            routine: Some(
                RoutineAssociation::new(
                    route.owner,
                    route.bot,
                    route.workspace,
                    route.creation_request,
                )
                .unwrap(),
            ),
        };
        let document = ScheduleDocument::create(&ScheduleHeader {
            schedule: route.schedule,
            created_at_ms: 0,
            initial_owner: route.owner.device,
            definition: definition.clone(),
        })
        .unwrap();
        p.slot.occurrence_ms = 60_000;
        p.definition = RoutineDefinitionIdentity {
            revision: 1,
            digest: routine_definition_digest(&definition, route.workspace, None).unwrap(),
            owner_handoff: None,
        };
        p.submission.provenance.routine = Some(RoutineProvenance {
            schedule: route.schedule,
            occurrence_ms: 60_000,
            definition: p.definition,
            grant_revision: 1,
        });
        p.submission.request.origin = RunOrigin::Scheduled {
            schedule: route.schedule,
            occurrence_ms: 60_000,
            revision: 1,
        };
        p.semantic_hash =
            routine_semantic_digest(&p.slot, &definition, route.workspace, None).unwrap();
        assert!(p.valid());
        let docs = Docs {
            workspace: route.workspace,
            document: DocumentId::from_uuid(route.schedule.as_uuid()),
            snapshot: Arc::new(Mutex::new(document.export_snapshot().unwrap())),
            handles: Arc::new(AtomicUsize::new(0)),
        };
        Self {
            f,
            ledger,
            route,
            docs,
            gate: Arc::new(ScheduleEventAuthorityGate::new()),
        }
    }
    fn coordinator(
        &self,
        client: &koru_api_client::Client,
        stopped: Arc<AtomicBool>,
    ) -> ScheduleCoordinator {
        ScheduleCoordinator::new(
            Box::new(self.docs.clone()),
            Box::new(self.ledger.clone()),
            Box::new(Preparation(self.f.scope.clone())),
            Box::new(Clock),
            self.route.owner.device,
        )
        .with_routines(
            Box::new(self.ledger.clone()),
            Box::new(HostRoutineBridge::new(client, Some(&authority()), stopped)),
            Box::new(self.gate.as_ref().clone()),
        )
    }
    fn document(&self) -> ScheduleDocument {
        ScheduleDocument::from_snapshot(&self.docs.snapshot.lock().unwrap()).unwrap()
    }
    fn barrier(&self, epoch: u64, paused: bool) {
        koru_delegate_sqlite::BotLedger::apply_bot_barrier(
            &mut self.ledger.clone(),
            &BotAdmissionBarrier {
                request: BotRequestId::mint(),
                epoch,
                paused,
                ..self.f.admission.clone()
            },
        )
        .unwrap();
    }
}
fn control(
    coordinator: &mut ScheduleCoordinator,
    route: &RoutineRouteIdentity,
    expected_revision: u32,
    action: RoutineControlAction,
) -> Result<BotReply, Failure> {
    controlled(coordinator.control_routine(
        route,
        expected_revision,
        request(action),
        61_000,
        &zone(),
    ))
}
fn test(
    coordinator: &mut ScheduleCoordinator,
    route: &RoutineRouteIdentity,
    expected_revision: u32,
    occurrence_ms: i64,
) -> Result<BotReply, Failure> {
    accepted(coordinator.accept_routine_test(route, expected_revision, occurrence_ms, 61_000))
}

#[test]
fn routine_control_failure_table_keeps_every_code_and_its_retryability() {
    let failure = |code: &'static str| {
        let failure = routine_failure(PortError::new(Port::Schedules, code, "internal detail"));
        // The delegate's message is an English literal for logs, the same for
        // every code; nothing internal is carried in it.
        assert_eq!(
            failure.message,
            "The Routine operation could not be completed."
        );
        (failure.code, failure.retryable)
    };
    for (internal, wire, retryable) in [
        ("routine_control_invalid", "invalid_request", false),
        ("unknown_workspace", "invalid_request", false),
        ("schedule_document", "invalid_schedule", false),
        ("recurrence", "invalid_schedule", false),
        ("routine_route_mismatch", "routine_route_mismatch", false),
        ("routine_catalog_identity", "routine_route_mismatch", false),
        (
            "routine_physical_route_mismatch",
            "routine_route_mismatch",
            false,
        ),
        ("schedule_not_found", "schedule_not_found", false),
        ("schedule_not_owner", "schedule_not_owner", false),
        (
            "schedule_handoff_pending",
            "schedule_handoff_pending",
            false,
        ),
        ("schedule_retired", "schedule_retired", false),
        ("schedule_read_only", "schedule_read_only", false),
        (
            "schedule_revision_conflict",
            "schedule_revision_conflict",
            false,
        ),
        ("schedule_state_conflict", "schedule_state_conflict", false),
        ("schedule_name_conflict", "schedule_name_conflict", false),
        ("routine_bot_paused", "routine_bot_paused", false),
        (
            "routine_test_requires_paused",
            "routine_test_requires_paused",
            false,
        ),
        (
            "routine_test_minute_changed",
            "routine_test_minute_changed",
            true,
        ),
        ("routine_test_too_soon", "routine_test_too_soon", true),
        (
            "routine_test_minute_taken",
            "routine_test_minute_taken",
            false,
        ),
        ("routine_test_busy", "routine_test_busy", true),
        // Nothing was decided: the caller may retry.
        ("schedule_unavailable", "schedule_unavailable", true),
        ("routine_barrier_absent", "schedule_unavailable", true),
        ("routine_owner_unavailable", "schedule_unavailable", true),
        ("routine_invalid", "schedule_unavailable", true),
        ("some_future_internal_code", "schedule_unavailable", true),
    ] {
        assert_eq!(
            failure(internal),
            (wire.to_owned(), retryable),
            "{internal}"
        );
    }
    // The ordinary mapping would have folded every Routine code away.
    assert_eq!(
        schedule_failure(PortError::new(Port::Schedules, "routine_bot_paused", "")).code,
        "schedule_unavailable"
    );
}

#[test]
fn routine_control_real_ledger_refuses_enable_and_test_while_the_bot_barrier_is_paused() {
    let p = Paused::new();
    let mut coordinator = p.coordinator(
        &gateway("http://127.0.0.1:1"),
        Arc::new(AtomicBool::new(false)),
    );
    let _authority = p.gate.enter().unwrap();
    let before = p.document().version();
    // The Bot's pause is a row in the real ledger, as Trash or Archive install it.
    p.barrier(2, true);
    for refused in [
        control(
            &mut coordinator,
            &p.route,
            1,
            RoutineControlAction::Enable {},
        ),
        test(&mut coordinator, &p.route, 1, 60_000),
    ] {
        let failure = refused.unwrap_err();
        assert_eq!(failure.code, "routine_bot_paused");
        assert!(!failure.retryable);
    }
    // Already paused: the answer a Trash driver counts as acknowledged.
    let conflict = control(
        &mut coordinator,
        &p.route,
        1,
        RoutineControlAction::Pause {},
    )
    .unwrap_err();
    assert_eq!(conflict.code, "schedule_state_conflict");
    assert!(!conflict.retryable);
    assert_eq!(p.document().version(), before);
    assert_eq!(coordinator.routine_test_outstanding(), None);
    assert!(matches!(
        RoutineLedger::read_routine_claim(&p.ledger, &p.f.prepared.slot).unwrap(),
        RoutineClaimObservation::Absent { .. }
    ));

    // The Bot is resumed: Enable applies once, and its lost-reply replay is
    // the same answer with nothing written.
    p.barrier(3, false);
    let on = control(
        &mut coordinator,
        &p.route,
        1,
        RoutineControlAction::Enable {},
    )
    .unwrap();
    assert_eq!(on.routine_control_valid(), Some(true));
    let BotReply::RoutineControlled {
        route,
        outcome,
        definition,
        effective_at_ms,
        lifecycle,
        ..
    } = &on
    else {
        panic!("controlled")
    };
    assert_eq!(route, &p.route);
    assert_eq!(
        (*outcome, *lifecycle, definition.revision, *effective_at_ms),
        (
            RoutineControlOutcome::Applied,
            RoutineControlLifecycle::Active,
            2,
            61_000
        )
    );
    let written = p.document().version();
    let replay = control(
        &mut coordinator,
        &p.route,
        1,
        RoutineControlAction::Enable {},
    )
    .unwrap();
    let BotReply::RoutineControlled {
        outcome: replayed,
        definition: same,
        ..
    } = &replay
    else {
        panic!("controlled")
    };
    assert_eq!(*replayed, RoutineControlOutcome::AlreadyApplied);
    assert_eq!(same, definition);
    assert_eq!(p.document().version(), written);
    let off = control(
        &mut coordinator,
        &p.route,
        2,
        RoutineControlAction::Pause {},
    )
    .unwrap();
    assert!(matches!(
        off,
        BotReply::RoutineControlled {
            outcome: RoutineControlOutcome::Applied,
            lifecycle: RoutineControlLifecycle::Paused,
            ..
        }
    ));
    assert_eq!(
        p.document().header().unwrap().definition.lifecycle,
        manual()
    );
    let stale = control(
        &mut coordinator,
        &p.route,
        1,
        RoutineControlAction::Enable {},
    )
    .unwrap_err();
    assert_eq!(stale.code, "schedule_revision_conflict");
}

/// One owner Test end to end on the host: accepted in memory, prepared through
/// the real reverse bridge with a paused definition, admitted by the real
/// actor, and claimed once through the shared ledger's Test method while the
/// Routine stays paused.
#[test]
fn routine_control_test_through_the_real_bridge_actor_and_shared_ledger_claims_once_while_paused() {
    let mut p = Paused::new();
    let prepared = p.f.prepared.clone();
    let offered = p.f.proof.clone();
    let admission = p.f.admission.clone();
    let handles = p.docs.handles.clone();
    let server_gate = p.gate.clone();
    let server_ledger = p.ledger.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = gateway(&format!("http://{}", listener.local_addr().unwrap()));
    let server = std::thread::spawn(move || {
        for phase in 0..5 {
            let mut stream = accept(&listener);
            let request = receive(&mut stream);
            assert_eq!(handles.load(Ordering::SeqCst), 0);
            drop(
                server_gate
                    .enter_until(Instant::now() + Duration::from_secs(1))
                    .expect("HTTP outside shared gate"),
            );
            let payload = match phase {
                0 => serde_json::json!({
                    "kind":"missing","slot":prepared.slot,"semantic_hash":prepared.semantic_hash
                }),
                1 => {
                    // The bridge's own codec built this request: a paused
                    // definition is well formed for the owner Test.
                    let sent: koru_delegate_api::routines::PrepareOccurrence =
                        serde_json::from_value(request["payload"].clone()).unwrap();
                    assert_eq!(sent.slot, prepared.slot);
                    assert_eq!(
                        sent.schedule_definition.lifecycle,
                        ScheduleLifecycle::Paused {
                            reason: SchedulePauseReason::Manual
                        }
                    );
                    serde_json::json!({"kind":"prepared","preparation":prepared})
                }
                2 => {
                    serde_json::json!({"kind":"ready","slot":prepared.slot,"semantic_hash":prepared.semantic_hash,"preparation_hash":prepared.preparation_hash,"admission":admission,"proof":offered})
                }
                3 => serde_json::json!({"kind":"missing","claim":request["payload"]["claim"]}),
                _ => {
                    let claim: RoutineClaimIdentity =
                        serde_json::from_value(request["payload"]["claim"].clone()).unwrap();
                    let observed =
                        RoutineLedger::read_routine_claim(&server_ledger, &claim.slot).unwrap();
                    assert!(
                        matches!(observed,RoutineClaimObservation::Claimed{record} if record.link_state==RoutineLinkState::Held{} && record.claim==claim)
                    );
                    RoutineLedger::acknowledge_routine_link(
                        &mut server_ledger.clone(),
                        &RoutineLinkAcknowledgement {
                            claim: claim.clone(),
                            outcome: RoutineLinkOutcome::Linked {},
                        },
                    )
                    .unwrap();
                    serde_json::json!({"kind":"linked","claim":claim})
                }
            };
            send(&mut stream,"200 OK",&serde_json::to_vec(&serde_json::json!({"version":1,"request_id":request["request_id"],"payload":payload})).unwrap());
        }
    });
    let stopped = Arc::new(AtomicBool::new(false));
    let mut coordinator = p.coordinator(&client, stopped.clone());
    let (commands, inbox) = mpsc::channel();
    let mut effects = ActorEffects::new(commands, stopped);
    let gate = p.gate.clone();
    let route = p.route.clone();
    let slot = p.f.prepared.slot;
    let task = std::thread::spawn(move || {
        let guard = gate.enter().unwrap();
        let reply = test(&mut coordinator, &route, 1, 60_000).unwrap();
        assert_eq!(reply.routine_control_valid(), Some(true));
        assert!(
            matches!(&reply, BotReply::RoutineTestAccepted { slot: accepted, definition } if *accepted == slot && definition.revision == 1)
        );
        // The same command again is the same Test, not a second one.
        assert_eq!(test(&mut coordinator, &route, 1, 60_000).unwrap(), reply);
        let mut report = coordinator.evaluate(&mut effects, 61_000, &zone()).unwrap();
        drop(guard);
        let work = report.routine_work.take().expect("the outstanding Test");
        assert!(matches!(
            work.candidate,
            koru_delegate_runtime::RoutineCandidate::Test { .. }
        ));
        let ran = coordinator
            .evaluate_routine_work(work, &mut effects, Instant::now() + Duration::from_secs(5))
            .unwrap();
        (coordinator, effects, ran.started.is_empty(), reply)
    });
    p.f.handle(inbox.recv_timeout(Duration::from_secs(3)).unwrap());
    assert!(p.f.held());
    p.f.handle(inbox.recv_timeout(Duration::from_secs(3)).unwrap());
    assert!(!p.f.held());
    let (mut coordinator, mut effects, nothing_started, reply) = task.join().unwrap();
    assert!(nothing_started);
    server.join().unwrap();

    let RoutineClaimObservation::Claimed { record } =
        RoutineLedger::read_routine_claim(&p.ledger, &p.f.prepared.slot).unwrap()
    else {
        panic!("the Test's claim")
    };
    assert_eq!(record.link_state, RoutineLinkState::Linked {});
    assert_eq!(record.claim.ids, p.f.prepared.ids);
    assert_eq!(record.claim.definition, p.f.prepared.definition);
    assert!(ScheduleLedger::unsettled(&p.ledger).unwrap().is_empty());
    assert!(
        koru_delegate_sqlite::BotLedger::reserve_bot(
            &mut p.ledger.clone(),
            p.f.prepared.ids.run,
            None
        )
        .is_err()
    );
    // The Routine is exactly as paused as before, and the ordinary path still
    // cannot claim it.
    let document = p.document();
    assert_eq!(document.header().unwrap().definition.lifecycle, manual());
    assert_eq!(document.header().unwrap().definition.revision, 1);
    assert!(!document.may_claim(p.route.owner.device).unwrap());
    assert!(
        document
            .claim_authority(
                p.route.workspace,
                p.route.owner.device,
                ScheduleClaimPlan::BotRoutine {
                    prepared: p.f.prepared.clone(),
                    representative_miss: None,
                },
            )
            .unwrap()
            .is_none()
    );
    // Asked again in the same minute: the same acceptance, and no further
    // pass has anything to run. The fake gateway has stopped, so any attempt
    // to prepare or claim again would fail this case.
    let guard = p.gate.enter().unwrap();
    assert_eq!(test(&mut coordinator, &p.route, 1, 60_000).unwrap(), reply);
    assert_eq!(coordinator.routine_test_outstanding(), None);
    for _ in 0..2 {
        let report = coordinator.evaluate(&mut effects, 61_000, &zone()).unwrap();
        assert!(report.routine_work.is_none());
    }
    drop(guard);
    let RoutineClaimObservation::Claimed { record: again } =
        RoutineLedger::read_routine_claim(&p.ledger, &p.f.prepared.slot).unwrap()
    else {
        panic!("the same claim")
    };
    assert_eq!(again.claim, record.claim);
}
