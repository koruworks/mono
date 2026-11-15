// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_delegate_core::*;
use koru_delegate_document::{RoutineExpiryPlan, ScheduleDocument};
use koru_delegate_runtime::{RoutineLedger, ScheduleEventRef, ScheduleOccurrenceEvent};

struct Fixture {
    events: BusScheduleEvents,
    ledger: SharedLedger,
    document: ScheduleDocument,
    fact: ScheduleOccurrenceEvent,
}
fn fixture(routine: bool) -> Fixture {
    let clock = Arc::new(AtomicI64::new(120_000));
    let mut db = RunsDb::open_with_clock(
        std::path::Path::new(":memory:"),
        Box::new(MutableClock(clock)),
    )
    .unwrap();
    let schedule = ScheduleId::mint();
    let workspace = WorkspaceId::new();
    let mut header = header(schedule, ProfileId::new());
    header.definition.recurrence = RecurrenceRule::floating("* * * * *".parse().unwrap());
    if routine {
        let owner = BotOwner {
            user_scope: serde_json::from_value(serde_json::json!(uuid::Uuid::new_v4())).unwrap(),
            device: header.initial_owner,
        };
        let association = koru_delegate_schedule::RoutineAssociation::new(
            owner,
            BotId::mint(),
            workspace,
            BotRequestId::mint(),
        )
        .unwrap();
        db.register_routine_route(&RoutineRouteIdentity {
            owner,
            bot: association.bot,
            schedule,
            workspace,
            creation_request: association.creation_request,
            seed_hash: RoutineDigest32::from_bytes([7; 32]),
            created_at_ms: 17,
        })
        .unwrap();
        header.definition.routine = Some(association);
        header.definition.lifecycle = ScheduleLifecycle::Paused {
            reason: SchedulePauseReason::Manual,
        };
    }
    let document = ScheduleDocument::create(&header).unwrap();
    if routine {
        document
            .set_lifecycle(header.initial_owner, 1, ScheduleLifecycle::Active, 18)
            .unwrap();
    }
    let run = RunId::mint();
    if routine {
        let authority = document
            .expired_routine_authority(
                workspace,
                header.initial_owner,
                RoutineExpiryPlan {
                    occurrence_ms: 60_000,
                    planned_run: run,
                    representative_miss: None,
                },
            )
            .unwrap()
            .unwrap();
        RoutineLedger::record_expired_routine(&mut db, authority).unwrap();
    } else {
        db.claim_skip_at(
            schedule,
            60_000,
            1,
            run,
            SlotOutcome::SkippedMissed,
            None,
            120_000,
        )
        .unwrap();
    }
    let ledger = SharedLedger::new(db);
    let client = koru_api_client::Client::new(koru_api_client::Discovery {
        port: 0,
        token: String::new(),
        expires_at_millis: i64::MAX,
    });
    let mut events = BusScheduleEvents::new(client, ledger.clone());
    let schedule = ScheduleEventRef {
        schedule,
        workspace,
        document: DocumentId::from_uuid(schedule.as_uuid()),
    };
    events
        .existing_created(schedule, &document.header().unwrap())
        .unwrap();
    let fact = ScheduleOccurrenceEvent {
        schedule,
        revision: document.header().unwrap().definition.revision,
        occurrence_ms: 60_000,
        run,
        outcome: ScheduleFiringOutcome::SkippedMissed,
        busy_holder: None,
        claimed_at_ms: 120_000,
    };
    Fixture {
        events,
        ledger,
        document,
        fact,
    }
}

#[test]
fn routine_publication_handwritten_fired_is_refused_at_every_owner_boundary() {
    for phase in 0..4 {
        let mut f = fixture(true);
        let mut fired = f.fact;
        fired.outcome = ScheduleFiringOutcome::Fired;
        assert!(f.events.prepare_occurrence(fired).is_err());
        let root = f
            .events
            .root_record(fired.schedule.schedule)
            .unwrap()
            .unwrap();
        let chain = ScheduleChain::rejoin(
            &f.events.ctx,
            BusScheduleEvents::chain_ref(fired.schedule),
            root.event,
        );
        let envelope = chain.fired(
            Stamp::fresh(fired.claimed_at_ms),
            ScheduleFired {
                revision: fired.revision,
                occurrence_ms: fired.occurrence_ms,
                run: fired.run,
            },
        );
        let stored = f
            .ledger
            .prepare_schedule_publication(
                envelope.event_id,
                BusScheduleEvents::persisted_ref(fired.schedule),
                SchedulePublicationFact::Fired {
                    revision: fired.revision,
                    occurrence_ms: fired.occurrence_ms,
                },
                &serde_json::to_string(&envelope).unwrap(),
            )
            .unwrap()
            .publication;
        match phase {
            0 => {
                assert!(
                    f.events
                        .ready(koru_delegate_runtime::PreparedScheduleEvent {
                            event: stored.event
                        })
                        .is_err()
                );
            }
            1 => {
                f.events.reconcile_one(stored.clone()).unwrap();
                assert!(
                    f.ledger
                        .ready_schedule_publications(16)
                        .unwrap()
                        .iter()
                        .all(|row| row.event != stored.event)
                );
            }
            2 => {
                f.ledger
                    .mark_schedule_publication_ready(stored.event)
                    .unwrap();
                assert!(f.events.drain_one(stored).is_err());
            }
            _ => {
                let claim = f
                    .ledger
                    .schedule_slot(fired.schedule.schedule, fired.occurrence_ms)
                    .unwrap()
                    .unwrap();
                let repaired = f
                    .events
                    .reconcile_occurrence(
                        fired.schedule,
                        Some(koru_delegate_runtime::PreparedScheduleEvent {
                            event: stored.event,
                        }),
                        &claim,
                    )
                    .unwrap()
                    .unwrap();
                assert_ne!(repaired.event, stored.event);
            }
        }
    }
    let f = fixture(true);
    let mut forged = f.fact;
    forged.schedule.schedule = ScheduleId::mint();
    forged.run = RunId::mint();
    forged.outcome = ScheduleFiringOutcome::Fired;
    // Authored ID and Run both have ordinary-negative lookups, but the
    // physical document UUID still names the retained Routine route.
    assert!(
        RoutineLedger::routine_route(&f.ledger, forged.schedule.schedule)
            .unwrap()
            .is_none()
    );
    f.ledger.validate_ordinary_schedule_run(forged.run).unwrap();
    assert!(f.events.validate_occurrence_authority(forged).is_err());
}

#[test]
fn routine_publication_retained_exact_skip_survives_pause_and_retirement() {
    for retired in [false, true] {
        let mut f = fixture(true);
        let owner = f.document.header().unwrap().initial_owner;
        if retired {
            f.document.retire(owner, 130_000).unwrap();
        } else {
            f.document
                .set_lifecycle(
                    owner,
                    2,
                    ScheduleLifecycle::Paused {
                        reason: SchedulePauseReason::Manual,
                    },
                    130_000,
                )
                .unwrap();
        }
        f.events
            .existing_created(f.fact.schedule, &f.document.header().unwrap())
            .unwrap();
        let prepared = f.events.prepare_occurrence(f.fact).unwrap().unwrap();
        f.events.ready(prepared).unwrap();
        let record = f
            .ledger
            .schedule_publication(prepared.event)
            .unwrap()
            .unwrap();
        assert!(f.events.validate_publication_authority(&record).is_ok());
        assert!(f.events.drain_one(record).unwrap());
    }
}

#[test]
fn routine_publication_missing_route_association_is_not_an_ordinary_negative() {
    let mut f = fixture(false);
    let mut header = f.document.header().unwrap();
    header.definition.routine = Some(
        koru_delegate_schedule::RoutineAssociation::new(
            BotOwner {
                user_scope: serde_json::from_value(serde_json::json!(uuid::Uuid::new_v4()))
                    .unwrap(),
                device: header.initial_owner,
            },
            BotId::mint(),
            f.fact.schedule.workspace,
            BotRequestId::mint(),
        )
        .unwrap(),
    );
    assert!(f.events.existing_created(f.fact.schedule, &header).is_err());
    assert!(f.events.prepare_created(f.fact.schedule, &header).is_err());
}

#[test]
fn routine_publication_positive_ordinary_negative_keeps_exact_skip_delivery() {
    let mut f = fixture(false);
    let prepared = f.events.prepare_occurrence(f.fact).unwrap().unwrap();
    f.events.ready(prepared).unwrap();
    let record = f
        .ledger
        .schedule_publication(prepared.event)
        .unwrap()
        .unwrap();
    let body = record.body.clone();
    assert!(f.events.drain_one(record).unwrap());
    assert_eq!(
        f.ledger
            .schedule_publication(prepared.event)
            .unwrap()
            .unwrap()
            .body,
        body
    );
}

#[test]
fn routine_publication_acknowledged_initial_turn_survives_terminal_policy_and_retirement() {
    use koru_delegate_runtime::RunStore;
    use koru_delegate_sqlite::{
        AttemptPhase, BotInitialSend, BotLedger, BotSettlement, NativeResultLedger,
    };
    let seed = crate::actor::tests::routine::Fixture::new();
    let p = seed.prepared;
    let proof = seed.proof;
    let admission = seed.admission;
    let mut db = RunsDb::open_with_clock(
        std::path::Path::new(":memory:"),
        Box::new(MutableClock(Arc::new(AtomicI64::new(1000)))),
    )
    .unwrap();
    let route = RoutineRouteIdentity {
        owner: p.slot.owner,
        bot: p.slot.bot,
        schedule: p.slot.schedule,
        workspace: p.submission.workspace,
        creation_request: BotRequestId::mint(),
        seed_hash: RoutineDigest32::from_bytes([9; 32]),
        created_at_ms: 0,
    };
    db.register_routine_route(&route).unwrap();
    db.apply_bot_barrier(&admission).unwrap();
    db.install_native_policy(&proof.global).unwrap();
    db.install_routine_policy(proof.routine.as_ref().unwrap())
        .unwrap();
    let facts = RoutineClaimFacts::from_consumed_authority(
        p.clone(),
        p.slot,
        p.definition,
        route.workspace,
        route.creation_request,
        None,
    )
    .unwrap();
    let RoutineClaimObservation::Claimed { record } =
        db.claim_bot_routine(facts, &admission, &proof).unwrap().0
    else {
        panic!("claim")
    };
    db.acknowledge_routine_link(&RoutineLinkAcknowledgement {
        claim: record.claim.clone(),
        outcome: RoutineLinkOutcome::Linked {},
    })
    .unwrap();
    db.reserve_bot(p.ids.run, Some(&proof)).unwrap();
    db.satisfy_intent(p.ids.run, DocumentId::new()).unwrap();
    let a = db.open_bot_attempt(p.ids.run, Some(&proof)).unwrap();
    db.activate_bot(a.run, a.attempt, a.generation, Some(&proof))
        .unwrap();
    db.bind_session(
        a.run,
        a.generation,
        ProviderSessionKey {
            idempotency: IdempotencyKey::mint(),
            session: Some(SessionId::mint()),
        },
        HostInstanceId::from_uuid(uuid::Uuid::new_v4()),
    )
    .unwrap();
    let send = BotInitialSend {
        attempt: a.attempt,
        generation: a.generation,
        key: IdempotencyKey::mint(),
        fingerprint: [8; 32],
        turn: None,
    };
    db.prepare_bot_initial_send(a.run, &send, Some(&proof))
        .unwrap();
    let mut ledger = SharedLedger::new(db);
    let location = ScheduleEventRef {
        schedule: p.slot.schedule,
        workspace: route.workspace,
        document: DocumentId::from_uuid(p.slot.schedule.as_uuid()),
    };
    let fact = ScheduleOccurrenceEvent {
        schedule: location,
        revision: p.definition.revision,
        occurrence_ms: p.slot.occurrence_ms,
        run: p.ids.run,
        outcome: ScheduleFiringOutcome::Fired,
        busy_holder: None,
        claimed_at_ms: record.claim.claimed_at_ms,
    };
    let mut events = BusScheduleEvents::new(
        koru_api_client::Client::new(koru_api_client::Discovery {
            port: 0,
            token: String::new(),
            expires_at_millis: i64::MAX,
        }),
        ledger.clone(),
    );
    let mut header = header(p.slot.schedule, ProfileId::new());
    header.initial_owner = p.slot.owner.device;
    header.definition.routine = Some(
        koru_delegate_schedule::RoutineAssociation::new(
            route.owner,
            route.bot,
            route.workspace,
            route.creation_request,
        )
        .unwrap(),
    );
    events.existing_created(location, &header).unwrap();
    assert!(events.prepare_occurrence(fact).is_err());
    let mut ack = send;
    ack.turn = Some(AgentTurnId::mint());
    ledger.acknowledge_bot_initial_send(a.run, &ack).unwrap();
    let prepared = events.prepare_occurrence(fact).unwrap().unwrap();
    events.ready(prepared).unwrap();
    ledger
        .advance(a.run, a.generation, AttemptPhase::Interrupted)
        .unwrap();
    ledger.retire(a.run, a.generation).unwrap();
    ledger.settle_bot(a.run, BotSettlement::Settled).unwrap();
    let mut newer = proof.global;
    newer.key.request = BotRequestId::mint().into();
    newer.config_revision += 1;
    newer.effects_allowed = false;
    ledger.install_native_policy(&newer).unwrap();
    events.validate_occurrence_authority(fact).unwrap();
    for publication in ledger.ready_schedule_publications(16).unwrap() {
        events.validate_publication_authority(&publication).unwrap();
    }
    let mut changed = fact;
    changed.run = RunId::mint();
    assert!(events.validate_occurrence_authority(changed).is_err());
}
