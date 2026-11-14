// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn routine_composition_live_claim_releases_before_http_and_never_launches() {
    let mut c = Composition::new();
    let report = c.run().unwrap();
    assert!(report.started.is_empty());
    let RoutineClaimObservation::Claimed { record } = c.observation() else {
        panic!("retained winner")
    };
    assert_eq!(record.claim.ids, c.prepared.ids);
    assert_eq!(record.link_state, RoutineLinkState::Linked {});
    assert_eq!(
        c.probe.lock().unwrap().calls,
        [
            "read_preparation",
            "preclaim",
            "begin",
            "release",
            "read_link",
            "finalize"
        ]
    );
    assert!(c.docs.read().firings().unwrap().is_empty());
    c.no_launch();
    let calls = c.probe.lock().unwrap().calls.clone();
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    for _ in 0..2 {
        c.probe.lock().unwrap().gate = true;
        let report = c
            .coordinator
            .evaluate(&mut c.effects, 61_000, &zone)
            .unwrap();
        c.probe.lock().unwrap().gate = false;
        assert!(report.routine_work.is_none());
    }
    c.probe.lock().unwrap().gate = true;
    let healthy = c
        .coordinator
        .evaluate(&mut c.effects, 61_000, &zone)
        .unwrap();
    c.probe.lock().unwrap().gate = false;
    assert_eq!(healthy.next_deadline_ms, Some(120_000));
    assert_eq!(c.probe.lock().unwrap().calls, calls);
}

#[test]
fn routine_composition_work_serial_is_consumed_and_old_selection_refused() {
    let mut c = Composition::new();
    let work = c.select();
    let newer = c.select();
    assert!(
        c.coordinator
            .evaluate_routine_work(
                work,
                &mut c.effects,
                Instant::now() + Duration::from_secs(5)
            )
            .is_err()
    );
    // Refusing stale work consumes neither a physical operation nor authority.
    assert!(c.probe.lock().unwrap().calls.is_empty());
    assert!(
        c.coordinator
            .evaluate_routine_work(
                newer,
                &mut c.effects,
                Instant::now() + Duration::from_secs(5)
            )
            .is_err()
    );
}

#[test]
fn routine_composition_missing_preparation_unknown_retries_read_without_new_prepare() {
    let mut c = Composition::new();
    {
        let mut p = c.probe.lock().unwrap();
        p.missing = true;
        p.unknown_prepare = true;
    }
    assert!(c.run().is_err());
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
    c.run().unwrap();
    let p = c.probe.lock().unwrap();
    assert_eq!(p.calls.iter().filter(|call| **call == "prepare").count(), 1);
    assert_eq!(
        p.calls
            .iter()
            .filter(|call| **call == "read_preparation")
            .count(),
        2
    );
    drop(p);
    c.no_launch();
}

#[test]
fn routine_composition_preclaim_refusal_cannot_begin_or_insert() {
    let mut c = Composition::new();
    c.probe.lock().unwrap().refuse_preclaim = true;
    assert!(c.run().is_err());
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
    assert!(!c.probe.lock().unwrap().calls.contains(&"begin"));
    c.no_launch();
}

#[test]
fn routine_composition_bad_immutable_preparation_is_refused_before_preclaim() {
    let mut c = Composition::new();
    c.probe.lock().unwrap().bad_prepared = true;
    assert!(c.run().is_err());
    assert_eq!(c.probe.lock().unwrap().calls, ["read_preparation"]);
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
}

#[test]
fn routine_composition_late_reopen_checks_stop_before_gate_or_begin() {
    let mut c = Composition::new();
    c.probe.lock().unwrap().late_open = true;
    assert!(c.run().is_err());
    let p = c.probe.lock().unwrap();
    assert_eq!(p.handles, 0);
    assert!(!p.gate);
    assert!(p.lease.is_none());
    assert!(!p.calls.contains(&"begin"));
    assert!(!p.calls.contains(&"finalize"));
}

#[test]
fn routine_composition_absorb_error_and_stop_release_exact_lease_before_return() {
    for stop in [false, true] {
        let mut c = Composition::new();
        {
            let mut p = c.probe.lock().unwrap();
            p.fail_absorb = !stop;
            p.stop_absorb = stop;
        }
        assert!(c.run().is_err());
        let p = c.probe.lock().unwrap();
        assert!(p.lease.is_none());
        assert_eq!(p.calls.last(), Some(&"release"));
        drop(p);
        assert!(matches!(
            c.observation(),
            RoutineClaimObservation::Absent { .. }
        ));
    }
}

#[test]
fn routine_composition_unknown_release_keeps_winner_but_sends_no_finalize() {
    let mut c = Composition::new();
    c.probe.lock().unwrap().fail_release = true;
    assert!(c.run().is_err());
    let p = c.probe.lock().unwrap();
    assert_eq!(p.lease, Some(c.prepared.ids.run));
    assert!(!p.calls.contains(&"read_link"));
    drop(p);
    let RoutineClaimObservation::Claimed { record } = c.observation() else {
        panic!("committed")
    };
    assert_eq!(record.link_state, RoutineLinkState::Held {});
    c.no_launch();
}

#[test]
fn routine_composition_positive_ai_link_with_native_held_replays_identical_finalize() {
    let mut c = Composition::new();
    c.probe.lock().unwrap().lose_ack = true;
    assert!(c.run().is_err());
    let RoutineClaimObservation::Claimed { record: first } = c.observation() else {
        panic!("held")
    };
    assert_eq!(first.link_state, RoutineLinkState::Held {});
    c.probe.lock().unwrap().lose_ack = false;
    c.run().unwrap();
    let RoutineClaimObservation::Claimed { record: second } = c.observation() else {
        panic!("linked")
    };
    assert_eq!(first.claim, second.claim);
    assert_eq!(second.link_state, RoutineLinkState::Linked {});
    assert_eq!(
        c.probe
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|call| **call == "begin")
            .count(),
        1
    );
    c.no_launch();
}

#[test]
fn routine_composition_retained_winner_recovers_after_due_window_and_pause() {
    let mut c = Composition::new();
    c.probe.lock().unwrap().lose_ack = true;
    assert!(c.run().is_err());
    let doc = c.docs.read();
    let h = doc.header().unwrap();
    doc.set_lifecycle(
        c.prepared.slot.owner.device,
        h.definition.revision,
        ScheduleLifecycle::Paused {
            reason: SchedulePauseReason::Manual,
        },
        120_001,
    )
    .unwrap();
    *c.docs.snapshot.lock().unwrap() = doc.export_snapshot().unwrap();
    c.h.schedule_clock.store(180_001, Ordering::SeqCst);
    c.probe.lock().unwrap().lose_ack = false;
    c.run().unwrap();
    assert_eq!(
        c.probe
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|call| **call == "preclaim")
            .count(),
        1
    );
    c.no_launch();
}

#[test]
fn routine_composition_unavailable_bridge_never_creates_a_fresh_claim() {
    let mut c = Composition::new();
    c.probe.lock().unwrap().unavailable = true;
    assert!(c.run().is_err());
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
    assert!(!c.probe.lock().unwrap().calls.contains(&"begin"));
}

#[test]
fn routine_composition_selected_due_expires_without_draft_or_http() {
    let mut c = Composition::new();
    c.probe.lock().unwrap().unavailable = true;
    let work = c.select();
    c.h.schedule_clock.store(120_000, Ordering::SeqCst);
    c.coordinator
        .evaluate_routine_work(
            work,
            &mut c.effects,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    assert!(c.probe.lock().unwrap().calls.is_empty());
    let RoutineClaimObservation::Skipped { record } = c.observation() else {
        panic!("missed")
    };
    assert_eq!(record.outcome, RoutineSkipOutcome::SkippedMissed {});
    assert_eq!(
        c.docs.read().firings().unwrap()[0].outcome,
        ScheduleFiringOutcome::SkippedMissed
    );
    c.no_launch();
}

#[test]
fn routine_composition_foreign_scope_busy_does_not_fabricate_sql_busy() {
    let mut c = Composition::new();
    c.probe.lock().unwrap().busy = true;
    c.run().unwrap();
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
    assert!(!c.probe.lock().unwrap().calls.contains(&"release"));
    let work = c.select();
    c.h.schedule_clock.store(120_000, Ordering::SeqCst);
    c.coordinator
        .evaluate_routine_work(
            work,
            &mut c.effects,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    assert!(
        matches!(c.observation(),RoutineClaimObservation::Skipped{record} if record.outcome==RoutineSkipOutcome::SkippedMissed{})
    );
}

#[test]
fn routine_composition_clock_rollback_never_claims_or_records_early_miss() {
    let mut c = Composition::new();
    let work = c.select();
    c.h.schedule_clock.store(59_999, Ordering::SeqCst);
    c.coordinator
        .evaluate_routine_work(
            work,
            &mut c.effects,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    assert!(c.probe.lock().unwrap().calls.is_empty());
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
}

#[test]
fn routine_composition_original_expired_deadline_performs_no_physical_phase() {
    let mut c = Composition::new();
    let work = c.select();
    assert!(
        c.coordinator
            .evaluate_routine_work(
                work,
                &mut c.effects,
                Instant::now() - Duration::from_millis(1)
            )
            .is_err()
    );
    assert!(c.probe.lock().unwrap().calls.is_empty());
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Absent { .. }
    ));
}

#[test]
fn routine_composition_physical_candidate_wins_before_header_alias_canonicalization() {
    let mut c = Composition::configured(|docs, _| docs.alias = true);
    c.run().unwrap();
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Claimed { .. }
    ));
    c.no_launch();
}

#[test]
fn routine_composition_sixteen_row_due_pass_reaches_later_active_schedule() {
    let mut c = Composition::configured(|docs, ledger| {
        let base = docs.read().header().unwrap();
        for n in 1..=20 {
            let mut header = base.clone();
            header.schedule = ScheduleId::from_uuid(v4(n));
            header.definition.revision = 1;
            header.definition.lifecycle = ScheduleLifecycle::Paused {
                reason: SchedulePauseReason::Manual,
            };
            let snapshot = ScheduleDocument::create(&header)
                .unwrap()
                .export_snapshot()
                .unwrap();
            docs.others
                .push((DocumentId::from_uuid(header.schedule.as_uuid()), snapshot));
            let association = header.definition.routine.as_ref().unwrap();
            ledger
                .inner
                .lock()
                .unwrap()
                .register_routine_route(&RoutineRouteIdentity {
                    owner: association.owner,
                    bot: association.bot,
                    schedule: header.schedule,
                    workspace: docs.workspace,
                    creation_request: association.creation_request,
                    seed_hash: RoutineDigest32::from_bytes([9; 32]),
                    created_at_ms: 0,
                })
                .unwrap();
        }
    });
    let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
    c.probe.lock().unwrap().gate = true;
    let first = c
        .coordinator
        .evaluate(&mut c.effects, 61_000, &zone)
        .unwrap();
    c.probe.lock().unwrap().gate = false;
    assert!(first.routine_work.is_none());
    assert_eq!(first.next_deadline_ms, Some(62_000));
    c.run().unwrap();
    assert!(matches!(
        c.observation(),
        RoutineClaimObservation::Claimed { .. }
    ));
    c.no_launch();
}

#[test]
fn routine_composition_failed_due_with_old_backlog_expires_exact_slot_after_suspend() {
    let mut c = Composition::new();
    c.probe.lock().unwrap().unavailable = true;
    c.h.schedule_clock.store(121_000, Ordering::SeqCst);
    // Current Due is 120000 while the planner's first unobserved Missed is
    // 60000. Unavailable read retains only that attempted Due identity.
    let work = c.select();
    assert!(
        matches!(&work.candidate,RoutineCandidate::Due{slot,..} if slot.occurrence_ms==120_000)
    );
    assert!(
        c.coordinator
            .evaluate_routine_work(
                work,
                &mut c.effects,
                Instant::now() + Duration::from_secs(5)
            )
            .is_err()
    );
    c.h.schedule_clock.store(3_600_001, Ordering::SeqCst);
    let work = c.select();
    assert!(
        matches!(&work.candidate,RoutineCandidate::Due{slot,representative_miss,..} if slot.occurrence_ms==120_000 && *representative_miss==Some(60_000))
    );
    c.coordinator
        .evaluate_routine_work(
            work,
            &mut c.effects,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    let db = c.h.schedule_store.inner.lock().unwrap();
    for instant in [60_000, 120_000] {
        let slot = RoutineSlot {
            occurrence_ms: instant,
            ..c.prepared.slot
        };
        assert!(
            matches!(db.read_routine_claim(&slot).unwrap(),RoutineClaimObservation::Skipped{record} if record.outcome==RoutineSkipOutcome::SkippedMissed{})
        );
    }
    assert_eq!(c.probe.lock().unwrap().calls, ["read_preparation"]);
}

#[test]
fn routine_composition_paused_attempt_hint_does_not_starve_second_route_expiry() {
    let second = ScheduleId::from_uuid(v4(51));
    let mut c = Composition::configured(|docs, ledger| {
        let mut header = docs.read().header().unwrap();
        header.schedule = second;
        header.definition.revision = 1;
        header.definition.effective_at_ms = 0;
        header.definition.lifecycle = ScheduleLifecycle::Paused {
            reason: SchedulePauseReason::Manual,
        };
        let doc = ScheduleDocument::create(&header).unwrap();
        doc.set_lifecycle(header.initial_owner, 1, ScheduleLifecycle::Active, 1)
            .unwrap();
        docs.others.push((
            DocumentId::from_uuid(second.as_uuid()),
            doc.export_snapshot().unwrap(),
        ));
        let a = header.definition.routine.unwrap();
        ledger
            .inner
            .lock()
            .unwrap()
            .register_routine_route(&RoutineRouteIdentity {
                owner: a.owner,
                bot: a.bot,
                schedule: second,
                workspace: docs.workspace,
                creation_request: a.creation_request,
                seed_hash: RoutineDigest32::from_bytes([9; 32]),
                created_at_ms: 0,
            })
            .unwrap();
    });
    c.probe.lock().unwrap().unavailable = true;
    assert!(c.run().is_err());
    let doc = c.docs.read();
    doc.set_lifecycle(
        c.prepared.slot.owner.device,
        2,
        ScheduleLifecycle::Paused {
            reason: SchedulePauseReason::Manual,
        },
        61_001,
    )
    .unwrap();
    *c.docs.snapshot.lock().unwrap() = doc.export_snapshot().unwrap();
    assert!(c.run().is_err());
    c.h.schedule_clock.store(120_001, Ordering::SeqCst);
    let work = c.select();
    assert!(
        matches!(&work.candidate,RoutineCandidate::Due{slot,..} if slot.schedule==second && slot.occurrence_ms==60_000)
    );
    c.coordinator
        .evaluate_routine_work(
            work,
            &mut c.effects,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
    let slot = RoutineSlot {
        schedule: second,
        ..c.prepared.slot
    };
    assert!(matches!(
        c.h.schedule_store
            .inner
            .lock()
            .unwrap()
            .read_routine_claim(&slot)
            .unwrap(),
        RoutineClaimObservation::Skipped { .. }
    ));
    assert_eq!(
        c.probe.lock().unwrap().calls,
        ["read_preparation", "read_preparation"]
    );
}

#[test]
fn routine_composition_recovery_cursor_commits_only_the_selected_lane_across_pages() {
    let mut c = Composition::new();
    let mut expected = std::collections::BTreeSet::new();
    for _ in 0..20 {
        let mut p = c.prepared.clone();
        replace_candidate(&mut p);
        p.slot.bot = BotId::mint();
        p.slot.schedule = ScheduleId::mint();
        p.submission.provenance.bot = p.slot.bot;
        let mut header = c.docs.read().header().unwrap();
        header.schedule = p.slot.schedule;
        header.definition.revision = 1;
        header.definition.effective_at_ms = 0;
        header.definition.lifecycle = ScheduleLifecycle::Paused {
            reason: SchedulePauseReason::Manual,
        };
        header.definition.routine.as_mut().unwrap().bot = p.slot.bot;
        let doc = ScheduleDocument::create(&header).unwrap();
        doc.set_lifecycle(p.slot.owner.device, 1, ScheduleLifecycle::Active, 1)
            .unwrap();
        p.definition.digest = routine_definition_digest(
            &doc.header().unwrap().definition,
            p.submission.workspace,
            None,
        )
        .unwrap();
        p.submission.provenance.routine = Some(RoutineProvenance {
            schedule: p.slot.schedule,
            occurrence_ms: p.slot.occurrence_ms,
            definition: p.definition,
            grant_revision: 1,
        });
        p.submission.request.origin = RunOrigin::Scheduled {
            schedule: p.slot.schedule,
            occurrence_ms: p.slot.occurrence_ms,
            revision: 2,
        };
        let proof = FreshBotPolicyProof {
            global: BotPolicyBarrier {
                key: BotPolicyBarrierKey {
                    owner: p.slot.owner,
                    bot: p.slot.bot,
                    request: BotRequestId::mint().into(),
                },
                config_revision: 1,
                policy_revision: 1,
                policy: p.submission.policy.clone(),
                effects_allowed: true,
            },
            routine: Some(RoutinePolicyBarrier {
                owner: p.slot.owner,
                bot: p.slot.bot,
                schedule: p.slot.schedule,
                request: BotRequestId::mint().into(),
                grant_revision: 1,
                policy: p.submission.policy.clone(),
                effects_allowed: true,
            }),
        };
        {
            let mut db = c.h.schedule_store.inner.lock().unwrap();
            let a = header.definition.routine.as_ref().unwrap();
            db.register_routine_route(&RoutineRouteIdentity {
                owner: p.slot.owner,
                bot: p.slot.bot,
                schedule: p.slot.schedule,
                workspace: p.submission.workspace,
                creation_request: a.creation_request,
                seed_hash: RoutineDigest32::from_bytes([8; 32]),
                created_at_ms: 0,
            })
            .unwrap();
            db.apply_bot_barrier(&admission(&p)).unwrap();
            db.install_native_policy(&proof.global).unwrap();
            db.install_routine_policy(proof.routine.as_ref().unwrap())
                .unwrap();
            RoutineLedger::claim_bot_routine(
                &mut *db,
                token(&doc, &p, false),
                &admission(&p),
                &proof,
            )
            .unwrap();
        }
        expected.insert(p.ids.run);
    }
    // Due wins first; neither unexecuted recovery lane may consume its first
    // valid row. Thereafter recovery alternates Held/Queued across 16-row pages.
    c.probe.lock().unwrap().unavailable = true;
    for _ in 0..82 {
        let _ = c.run();
    }
    let p = c.probe.lock().unwrap();
    let visited = p
        .link_runs
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(visited, expected);
    assert!(
        p.calls
            .iter()
            .filter(|call| **call == "read_preparation")
            .count()
            > 1
    );
}
