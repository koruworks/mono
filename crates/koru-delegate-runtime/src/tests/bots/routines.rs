// SPDX-License-Identifier: MPL-2.0

//! Real live document authority crosses the production runtime adapter into
//! SQLite memory. No executable Routine, scheduler or HTTP path is enabled.
use super::*;
use koru_delegate_document::{ScheduleClaimAuthority, ScheduleClaimPlan, ScheduleDocument};
use koru_delegate_schedule::*;
use koru_recurrence::{CronExpression, RecurrenceRule};

fn fixture() -> (
    Harness,
    ScheduleDocument,
    RoutinePrepared,
    FreshBotPolicyProof,
) {
    let h = bot_harness();
    h.schedule_clock.store(1000, Ordering::SeqCst);
    let mut s = submission(1);
    s.input.text = s.request.objective.as_str().to_owned();
    let association = RoutineAssociation::new(
        s.provenance.owner,
        s.provenance.bot,
        s.workspace,
        BotRequestId::mint(),
    )
    .unwrap();
    let header = ScheduleHeader {
        schedule: ScheduleId::from_uuid(v4(50)),
        created_at_ms: 0,
        initial_owner: association.owner.device,
        definition: ScheduleDefinition {
            revision: 1,
            effective_at_ms: 0,
            template: DelegationTemplate {
                requested_by: s.request.requested_by.clone(),
                objective: s.request.objective.clone(),
                agent: s.request.agent.clone(),
                requested_model: None,
                scope: WorkspaceScope::Temporary,
                grants: Default::default(),
                context: vec![],
            },
            recurrence: RecurrenceRule::floating(CronExpression::parse("* * * * *").unwrap()),
            lifecycle: ScheduleLifecycle::Paused {
                reason: SchedulePauseReason::Manual,
            },
            routine: Some(association.clone()),
        },
    };
    let doc = ScheduleDocument::create(&header).unwrap();
    let mut definition = header.definition;
    definition.revision = 2;
    definition.effective_at_ms = 1;
    definition.lifecycle = ScheduleLifecycle::Active;
    doc.update_definition(association.owner.device, &definition)
        .unwrap();
    let identity = RoutineDefinitionIdentity {
        revision: 2,
        digest: routine_definition_digest(&definition, s.workspace, None).unwrap(),
        owner_handoff: None,
    };
    let ids = RoutineExecutionIds {
        submission: BotRequestId::mint(),
        run: s.request.run,
        chat: uuid::Uuid::new_v4(),
        attempt: uuid::Uuid::new_v4(),
        user_message: uuid::Uuid::new_v4(),
        assistant_message: uuid::Uuid::new_v4(),
    };
    s.provenance.submission = ids.submission;
    s.provenance.chat_id = ids.chat;
    s.provenance.ai_attempt_id = Some(ids.attempt);
    s.provenance.assistant_message_id = Some(ids.assistant_message);
    s.provenance.routine = Some(RoutineProvenance {
        schedule: header.schedule,
        occurrence_ms: 900,
        definition: identity,
        grant_revision: 1,
    });
    s.request.origin = RunOrigin::Scheduled {
        schedule: header.schedule,
        occurrence_ms: 900,
        revision: 2,
    };
    let prepared = RoutinePrepared {
        slot: RoutineSlot {
            owner: association.owner,
            bot: association.bot,
            schedule: header.schedule,
            occurrence_ms: 900,
        },
        definition: identity,
        ids,
        semantic_hash: RoutineDigest32::from_bytes([7; 32]),
        preparation_hash: RoutineDigest32::from_bytes([8; 32]),
        submission_version: 2,
        submission: s,
    };
    assert!(prepared.valid());
    let fresh = FreshBotPolicyProof {
        global: BotPolicyBarrier {
            key: BotPolicyBarrierKey {
                owner: association.owner,
                bot: association.bot,
                request: BotRequestId::mint().into(),
            },
            config_revision: 1,
            policy_revision: 1,
            policy: prepared.submission.policy.clone(),
            effects_allowed: true,
        },
        routine: Some(RoutinePolicyBarrier {
            owner: association.owner,
            bot: association.bot,
            schedule: header.schedule,
            request: BotRequestId::mint().into(),
            grant_revision: 1,
            policy: prepared.submission.policy.clone(),
            effects_allowed: true,
        }),
    };
    {
        let mut db = h.schedule_store.inner.lock().unwrap();
        db.register_routine_route(&RoutineRouteIdentity {
            owner: association.owner,
            bot: association.bot,
            schedule: header.schedule,
            workspace: association.home,
            creation_request: association.creation_request,
            seed_hash: RoutineDigest32::from_bytes([9; 32]),
            created_at_ms: 0,
        })
        .unwrap();
        db.install_native_policy(&fresh.global).unwrap();
        db.install_routine_policy(fresh.routine.as_ref().unwrap())
            .unwrap();
    }
    (h, doc, prepared, fresh)
}
fn token(
    doc: &ScheduleDocument,
    prepared: &RoutinePrepared,
    missed: bool,
) -> ScheduleClaimAuthority {
    doc.claim_authority(
        prepared.submission.workspace,
        prepared.slot.owner.device,
        ScheduleClaimPlan::BotRoutine {
            prepared: prepared.clone(),
            representative_miss: missed.then_some(RoutineRepresentativeMiss {
                occurrence_ms: 100,
                planned_run: RunId::from_uuid(v4(60)),
            }),
        },
    )
    .unwrap()
    .unwrap()
}
fn claim(
    h: &Harness,
    doc: &ScheduleDocument,
    p: &RoutinePrepared,
    fresh: &FreshBotPolicyProof,
    missed: bool,
) -> RoutineClaimRecord {
    let (outcome, skip) = crate::RoutineLedger::claim_bot_routine(
        &mut *h.schedule_store.inner.lock().unwrap(),
        token(doc, p, missed),
        fresh,
    )
    .unwrap();
    assert_eq!(skip.is_some(), missed);
    let RoutineClaimObservation::Claimed { record } = outcome else {
        panic!("expected claim")
    };
    record
}
fn replace_candidate(p: &mut RoutinePrepared) {
    p.ids = RoutineExecutionIds {
        submission: BotRequestId::mint(),
        run: RunId::from_uuid(uuid::Uuid::new_v4()),
        chat: uuid::Uuid::new_v4(),
        attempt: uuid::Uuid::new_v4(),
        user_message: uuid::Uuid::new_v4(),
        assistant_message: uuid::Uuid::new_v4(),
    };
    p.submission.request.run = p.ids.run;
    p.submission.provenance.submission = p.ids.submission;
    p.submission.provenance.chat_id = p.ids.chat;
    p.submission.provenance.ai_attempt_id = Some(p.ids.attempt);
    p.submission.provenance.assistant_message_id = Some(p.ids.assistant_message);
    p.preparation_hash = RoutineDigest32::from_bytes([10; 32]);
}

#[test]
fn routine_live_adapter_commits_held_and_miss_without_enabling_execution() {
    let (h, doc, p, fresh) = fixture();
    assert!(token(&doc, &p, false).into_run().is_err());
    assert!(token(&doc, &p, false).into_skip().is_err());
    let record = claim(&h, &doc, &p, &fresh, true);
    assert_eq!(record.link_state, RoutineLinkState::Held {});
    let mut db = h.schedule_store.inner.lock().unwrap();
    assert_eq!(
        db.admit_bot(&p.submission).unwrap().submission,
        p.submission
    );
    assert!(db.pending_intents().unwrap().is_empty());
    assert!(db.queued_bots(0, 16).unwrap().is_empty());
    assert!(db.reserve_bot(p.ids.run).is_err());
    let linked = db
        .acknowledge_routine_link(&RoutineLinkAcknowledgement {
            claim: record.claim.clone(),
            outcome: RoutineLinkOutcome::Linked {},
        })
        .unwrap();
    assert_eq!(linked.link_state, RoutineLinkState::Linked {});
    assert!(db.reserve_bot(p.ids.run).is_err());
    assert!(db.pending_intents().unwrap().is_empty());
    assert!(db.queued_bots(0, 16).unwrap().is_empty());
    assert!(h.sessions.lock().unwrap().calls.is_empty());
}

#[test]
fn routine_live_adapter_revalidates_pause_retirement_schema_and_definition() {
    for change in 0..4 {
        let (h, doc, p, fresh) = fixture();
        let authority = token(&doc, &p, false);
        match change {
            0 | 3 => {
                let mut definition = doc.header().unwrap().definition;
                definition.revision += 1;
                definition.effective_at_ms += 1;
                if change == 0 {
                    definition.lifecycle = ScheduleLifecycle::Paused {
                        reason: SchedulePauseReason::Manual,
                    };
                } else {
                    definition.template.objective = Objective::new("changed task").unwrap();
                }
                doc.update_definition(p.slot.owner.device, &definition)
                    .unwrap();
            }
            1 => {
                doc.retire(p.slot.owner.device, 1001).unwrap();
            }
            _ => {
                doc.doc()
                    .get_map("schedule")
                    .insert("schema", 3i64)
                    .unwrap();
                doc.doc().commit();
            }
        }
        let mut db = h.schedule_store.inner.lock().unwrap();
        assert!(crate::RoutineLedger::claim_bot_routine(&mut *db, authority, &fresh).is_err());
        assert!(matches!(
            db.read_routine_claim(&p.slot).unwrap(),
            RoutineClaimObservation::Absent { .. }
        ));
        assert!(db.bot_admission(p.ids.run).unwrap().is_none());
    }
}

#[test]
fn routine_live_adapter_ignores_unrelated_name_version_and_preserves_stored_winner() {
    let (h, doc, mut p, mut fresh) = fixture();
    let authority = token(&doc, &p, false);
    doc.rename(
        p.slot.owner.device,
        &doc.name().unwrap(),
        &ScheduleName::new("renamed").unwrap(),
    )
    .unwrap();
    let (winner, _) = crate::RoutineLedger::claim_bot_routine(
        &mut *h.schedule_store.inner.lock().unwrap(),
        authority,
        &fresh,
    )
    .unwrap();
    replace_candidate(&mut p);
    fresh.global.effects_allowed = false;
    h.schedule_clock.store(90_000, Ordering::SeqCst);
    let (replay, _) = crate::RoutineLedger::claim_bot_routine(
        &mut *h.schedule_store.inner.lock().unwrap(),
        token(&doc, &p, false),
        &fresh,
    )
    .unwrap();
    assert_eq!(replay, winner);
    let mut foreign = p.slot;
    foreign.bot = BotId::from_uuid(uuid::Uuid::new_v4());
    assert!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .read_routine_claim(&foreign)
            .is_err()
    );
}

#[test]
fn routine_stop_existing_orchestrator_path_wins_before_or_after_link() {
    for linked in [false, true] {
        let (mut h, doc, p, fresh) = fixture();
        let record = claim(&h, &doc, &p, &fresh, false);
        let ack = RoutineLinkAcknowledgement {
            claim: record.claim.clone(),
            outcome: RoutineLinkOutcome::Linked {},
        };
        if linked {
            h.schedule_store
                .inner
                .lock()
                .unwrap()
                .acknowledge_routine_link(&ack)
                .unwrap();
        }
        h.orchestrator
            .bot_stop(&key(&p.submission), p.slot.owner.device, None)
            .unwrap();
        h.orchestrator
            .bot_stop(&key(&p.submission), p.slot.owner.device, None)
            .unwrap();
        let mut db = h.schedule_store.inner.lock().unwrap();
        let RoutineClaimObservation::Claimed { record } = db.read_routine_claim(&p.slot).unwrap()
        else {
            panic!("claim")
        };
        assert_eq!(
            record.link_state,
            RoutineLinkState::Terminal {
                reason: RoutineTerminalReason::Stopped
            }
        );
        assert_eq!(
            db.bot_admission(p.ids.run).unwrap().unwrap().state,
            BotAdmissionState::Settled
        );
        assert_eq!(
            db.intent(p.ids.run).unwrap().unwrap().state,
            IntentState::Abandoned
        );
        assert!(db.acknowledge_routine_link(&ack).is_err());
        assert!(h.sessions.lock().unwrap().calls.is_empty());
        assert_eq!(*h.preambles.lock().unwrap(), 0);
    }
}

#[test]
fn routine_live_claim_requires_exact_current_policy_and_writer_deadline() {
    for change in 0..5 {
        let (h, doc, p, mut fresh) = fixture();
        match change {
            0 => fresh.global.effects_allowed = false,
            1 => fresh.routine.as_mut().unwrap().request = BotRequestId::mint().into(),
            2 => {
                let mut latest = fresh.routine.clone().unwrap();
                latest.grant_revision += 1;
                latest.request = BotRequestId::mint().into();
                h.schedule_store
                    .inner
                    .lock()
                    .unwrap()
                    .install_routine_policy(&latest)
                    .unwrap();
            }
            3 => h.schedule_clock.store(899, Ordering::SeqCst),
            _ => h.schedule_clock.store(60_900, Ordering::SeqCst),
        }
        let mut db = h.schedule_store.inner.lock().unwrap();
        let result =
            crate::RoutineLedger::claim_bot_routine(&mut *db, token(&doc, &p, false), &fresh);
        if change == 4 {
            assert!(matches!(
                result.unwrap().0,
                RoutineClaimObservation::Skipped {
                    record: RoutineSkippedClaim {
                        outcome: RoutineSkipOutcome::SkippedMissed {},
                        ..
                    }
                }
            ));
        } else {
            assert!(result.is_err());
        }
        assert!(db.bot_admission(p.ids.run).unwrap().is_none());
    }
}

#[test]
fn routine_second_slot_records_actual_held_busy_holder() {
    let (h, doc, mut p, fresh) = fixture();
    let winner = claim(&h, &doc, &p, &fresh, false);
    replace_candidate(&mut p);
    p.slot.occurrence_ms = 1000;
    p.submission
        .provenance
        .routine
        .as_mut()
        .unwrap()
        .occurrence_ms = 1000;
    p.submission.request.origin = RunOrigin::Scheduled {
        schedule: p.slot.schedule,
        occurrence_ms: 1000,
        revision: p.definition.revision,
    };
    let mut db = h.schedule_store.inner.lock().unwrap();
    let (outcome, _) =
        crate::RoutineLedger::claim_bot_routine(&mut *db, token(&doc, &p, false), &fresh).unwrap();
    let RoutineClaimObservation::Skipped { record } = outcome else {
        panic!("skip")
    };
    assert_eq!(
        record.outcome,
        RoutineSkipOutcome::SkippedBusy {
            holder: winner.claim.ids.run
        }
    );
    assert!(db.intent(p.ids.run).unwrap().is_none());
}
