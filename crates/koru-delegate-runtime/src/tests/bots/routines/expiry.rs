// SPDX-License-Identifier: MPL-2.0
//! Real live document -> production adapter -> SQLite; no AI draft or port.
use super::*;
use koru_delegate_document::{RoutineExpiryAuthority, RoutineExpiryPlan};

fn expiry_token(
    doc: &ScheduleDocument,
    at: i64,
    run: RunId,
    miss: Option<RoutineRepresentativeMiss>,
) -> RoutineExpiryAuthority {
    let association = doc.header().unwrap().definition.routine.unwrap();
    doc.expired_routine_authority(
        association.home,
        association.owner.device,
        RoutineExpiryPlan {
            occurrence_ms: at,
            planned_run: run,
            representative_miss: miss,
        },
    )
    .unwrap()
    .unwrap()
}

#[test]
fn routine_expiry_adapter_records_no_draft_miss_without_policies_or_executable_rows() {
    let h = bot_harness();
    h.schedule_clock.store(60_900, Ordering::SeqCst);
    let subject = submission(1);
    let association = RoutineAssociation::new(
        subject.provenance.owner,
        subject.provenance.bot,
        subject.workspace,
        BotRequestId::mint(),
    )
    .unwrap();
    let mut header = ScheduleHeader {
        schedule: ScheduleId::mint(),
        created_at_ms: 0,
        initial_owner: association.owner.device,
        definition: ScheduleDefinition {
            revision: 1,
            effective_at_ms: 0,
            template: DelegationTemplate {
                requested_by: subject.request.requested_by,
                objective: subject.request.objective,
                agent: subject.request.agent,
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
    header.definition.revision = 2;
    header.definition.effective_at_ms = 1;
    header.definition.lifecycle = ScheduleLifecycle::Active;
    doc.update_definition(association.owner.device, &header.definition)
        .unwrap();
    let mut db = h.schedule_store.inner.lock().unwrap();
    db.register_routine_route(&RoutineRouteIdentity {
        owner: association.owner,
        bot: association.bot,
        schedule: header.schedule,
        workspace: association.home,
        creation_request: association.creation_request,
        seed_hash: RoutineDigest32::from_bytes([1; 32]),
        created_at_ms: 0,
    })
    .unwrap();
    let run = RunId::mint();
    let (outcome, miss) =
        crate::RoutineLedger::record_expired_routine(&mut *db, expiry_token(&doc, 900, run, None))
            .unwrap();
    assert!(
        matches!(outcome,RoutineClaimObservation::Skipped{record} if record.planned_run==run && matches!(record.outcome,RoutineSkipOutcome::SkippedMissed{}))
    );
    assert!(miss.is_none());
    assert!(db.intents().unwrap().is_empty());
    // No admission row exists. The skip retains the planned run id as a
    // Routine marker, so `bot_admission` denies it at this tool-authority
    // boundary instead of reading absence as an ordinary Run (63c7e124).
    assert_eq!(
        db.bot_admission(run),
        Err(BotLedgerError::Ledger(
            LedgerError::InvalidScheduleAuthority
        ))
    );
    assert!(
        db.current_routine_policy(association.owner, association.bot, header.schedule)
            .unwrap()
            .is_none()
    );
}

#[test]
fn routine_expiry_adapter_consumes_current_authority_before_any_ledger_write() {
    let (h, doc, p, _) = fixture();
    let authority = expiry_token(&doc, 900, RunId::mint(), None);
    doc.set_lifecycle(
        p.slot.owner.device,
        2,
        ScheduleLifecycle::Paused {
            reason: SchedulePauseReason::Manual,
        },
        2000,
    )
    .unwrap();
    h.schedule_clock.store(60_900, Ordering::SeqCst);
    let mut db = h.schedule_store.inner.lock().unwrap();
    assert!(crate::RoutineLedger::record_expired_routine(&mut *db, authority).is_err());
    assert!(db.intents().unwrap().is_empty());
    assert!(matches!(
        db.read_routine_claim(&p.slot).unwrap(),
        RoutineClaimObservation::Absent { .. }
    ));
}

#[test]
fn routine_expiry_adapter_preserves_actual_claim_and_busy_winners_before_early_clock() {
    for busy in [false, true] {
        let (h, doc, p, fresh) = fixture();
        if busy {
            h.schedule_store
                .inner
                .lock()
                .unwrap()
                .admit_bot(&submission(66))
                .unwrap();
        }
        let winner = crate::RoutineLedger::claim_bot_routine(
            &mut *h.schedule_store.inner.lock().unwrap(),
            token(&doc, &p, false),
            &admission(&p),
            &fresh,
        )
        .unwrap()
        .0;
        assert!(matches!(
            (&winner, busy),
            (RoutineClaimObservation::Claimed { .. }, false)
                | (RoutineClaimObservation::Skipped { .. }, true)
        ));
        h.schedule_clock.store(-1, Ordering::SeqCst);
        let (actual, miss) = crate::RoutineLedger::record_expired_routine(
            &mut *h.schedule_store.inner.lock().unwrap(),
            expiry_token(&doc, 900, RunId::mint(), None),
        )
        .unwrap();
        assert_eq!(actual, winner);
        assert!(miss.is_none());
    }
}

#[test]
fn routine_expiry_adapter_claimed_representative_rolls_back_primary_skip() {
    let (h, doc, mut p, fresh) = fixture();
    p.slot.occurrence_ms = 100;
    p.submission
        .provenance
        .routine
        .as_mut()
        .unwrap()
        .occurrence_ms = 100;
    p.submission.request.origin = RunOrigin::Scheduled {
        schedule: p.slot.schedule,
        occurrence_ms: 100,
        revision: p.definition.revision,
    };
    let winner = claim(&h, &doc, &p, &fresh, false);
    h.schedule_clock.store(60_900, Ordering::SeqCst);
    let authority = expiry_token(
        &doc,
        900,
        RunId::mint(),
        Some(RoutineRepresentativeMiss {
            occurrence_ms: 100,
            planned_run: RunId::mint(),
        }),
    );
    let mut db = h.schedule_store.inner.lock().unwrap();
    assert!(crate::RoutineLedger::record_expired_routine(&mut *db, authority).is_err());
    assert_eq!(
        db.read_routine_claim(&p.slot).unwrap(),
        RoutineClaimObservation::Claimed { record: winner }
    );
    assert!(matches!(
        db.read_routine_claim(&RoutineSlot {
            occurrence_ms: 900,
            ..p.slot
        })
        .unwrap(),
        RoutineClaimObservation::Absent { .. }
    ));
    assert_eq!(db.intents().unwrap().len(), 1);
}
