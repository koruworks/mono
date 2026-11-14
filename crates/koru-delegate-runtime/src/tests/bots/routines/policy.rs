// SPDX-License-Identifier: MPL-2.0

//! Real admission/claim SQL backs the cache tests. No executing Routine is
//! enabled: composing a snapshot does not open reservation or native gates.
use super::*;

fn key(p: &RoutinePrepared) -> BotRunKey {
    BotRunKey {
        owner: p.slot.owner,
        bot: p.slot.bot,
        run: p.ids.run,
    }
}

fn linked() -> (Harness, RoutinePrepared, FreshBotPolicyProof) {
    let (h, doc, p, fresh) = fixture();
    let claimed = claim(&h, &doc, &p, &fresh, false);
    h.schedule_store
        .inner
        .lock()
        .unwrap()
        .acknowledge_routine_link(&RoutineLinkAcknowledgement {
            claim: claimed.claim,
            outcome: RoutineLinkOutcome::Linked {},
        })
        .unwrap();
    (h, p, fresh)
}

fn synchronize(h: &mut Harness, fresh: &FreshBotPolicyProof) {
    let device = fresh.global.key.owner.device;
    assert_eq!(
        h.orchestrator
            .synchronize_native_policy(fresh.global.clone(), device)
            .unwrap()
            .state,
        BotPolicyBarrierState::Effective
    );
    assert_eq!(
        h.orchestrator
            .synchronize_routine_policy(fresh.routine.clone().unwrap(), device)
            .unwrap(),
        BotPolicyBarrierState::Effective
    );
}

#[test]
fn routine_policy_cache_requires_both_subjects_and_keeps_execution_closed() {
    let (mut h, p, fresh) = linked();
    assert!(
        h.orchestrator
            .native_policy_for_run(&key(&p), 1, 1)
            .is_err()
    );
    h.orchestrator
        .synchronize_native_policy(fresh.global.clone(), p.slot.owner.device)
        .unwrap();
    assert!(
        h.orchestrator
            .native_policy_for_run(&key(&p), 1, 1)
            .is_err()
    );
    synchronize(&mut h, &fresh);
    assert_eq!(
        h.orchestrator
            .native_policy_for_run(&key(&p), 1, 1)
            .unwrap(),
        fresh
    );
    let mut wrong = key(&p);
    wrong.bot = BotId::mint();
    assert!(h.orchestrator.native_policy_for_run(&wrong, 1, 1).is_err());
    assert!(
        h.orchestrator
            .native_policy_for_run(&key(&p), 2, 1)
            .is_err()
    );
    let mut db = h.schedule_store.inner.lock().unwrap();
    assert!(db.reserve_bot(p.ids.run).is_err());
    assert!(db.queued_bots(0, 16).unwrap().is_empty());
    assert!(h.sessions.lock().unwrap().calls.is_empty());
}

#[test]
fn routine_policy_cache_historical_and_conflicting_sync_preserve_current_proof() {
    let (mut h, p, first) = linked();
    synchronize(&mut h, &first);
    let mut newer = first.clone();
    let routine = newer.routine.as_mut().unwrap();
    routine.request = BotRequestId::mint().into();
    routine.grant_revision = 2;
    synchronize(&mut h, &newer);
    assert_eq!(
        h.orchestrator
            .synchronize_routine_policy(first.routine.unwrap(), p.slot.owner.device)
            .unwrap(),
        BotPolicyBarrierState::Effective
    );
    let mut conflict = newer.routine.clone().unwrap();
    conflict.effects_allowed = false;
    assert!(
        h.orchestrator
            .synchronize_routine_policy(conflict, p.slot.owner.device)
            .is_err()
    );
    assert_eq!(
        h.orchestrator
            .native_policy_for_run(&key(&p), 1, 1)
            .unwrap(),
        newer
    );
}

#[test]
fn routine_policy_cache_global_historical_sync_cannot_replace_newer_epoch() {
    let (mut h, p, first) = linked();
    synchronize(&mut h, &first);
    let mut newer = first.clone();
    newer.global.key.request = BotRequestId::mint().into();
    newer.global.config_revision = 2;
    newer.global.policy_revision = 2;
    synchronize(&mut h, &newer);
    assert_eq!(
        h.orchestrator
            .synchronize_native_policy(first.global.clone(), p.slot.owner.device)
            .unwrap(),
        BotPolicyBarrierReceipt {
            barrier: first.global.clone(),
            state: BotPolicyBarrierState::Effective
        }
    );
    assert!(
        h.orchestrator
            .reconcile_native_policy(&first.global.key, p.slot.owner.device)
            .is_err()
    );
    assert_eq!(
        h.orchestrator
            .native_policy_for_run(&key(&p), 2, 2)
            .unwrap(),
        newer
    );
}

#[test]
fn routine_policy_cache_rechecks_durable_global_and_independent_currentness() {
    for global in [false, true] {
        let (mut h, p, first) = linked();
        synchronize(&mut h, &first);
        let mut newer = first.clone();
        if global {
            newer.global.key.request = BotRequestId::mint().into();
            newer.global.config_revision = 2;
            newer.global.policy_revision = 2;
            h.schedule_store
                .inner
                .lock()
                .unwrap()
                .install_native_policy(&newer.global)
                .unwrap();
        } else {
            let barrier = newer.routine.as_mut().unwrap();
            barrier.request = BotRequestId::mint().into();
            barrier.grant_revision = 2;
            h.schedule_store
                .inner
                .lock()
                .unwrap()
                .install_routine_policy(barrier)
                .unwrap();
        }
        assert!(
            h.orchestrator
                .native_policy_for_run(&key(&p), 1, 1)
                .is_err()
        );
        synchronize(&mut h, &newer);
        assert_eq!(
            h.orchestrator
                .native_policy_for_run(
                    &key(&p),
                    newer.global.config_revision,
                    newer.global.policy_revision
                )
                .unwrap(),
            newer
        );
    }
}

#[test]
fn routine_policy_cache_eviction_and_foreign_subject_never_supply_missing_authority() {
    let (mut h, p, first) = linked();
    synchronize(&mut h, &first);
    for _ in 0..64 {
        let mut other = first.routine.clone().unwrap();
        other.schedule = ScheduleId::from_uuid(uuid::Uuid::new_v4());
        other.request = BotRequestId::mint().into();
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .register_routine_route(&RoutineRouteIdentity {
                owner: other.owner,
                bot: other.bot,
                schedule: other.schedule,
                workspace: other.policy.workspace,
                creation_request: BotRequestId::mint(),
                seed_hash: RoutineDigest32::from_bytes([9; 32]),
                created_at_ms: 0,
            })
            .unwrap();
        h.orchestrator
            .synchronize_routine_policy(other, p.slot.owner.device)
            .unwrap();
    }
    assert!(
        h.orchestrator
            .native_policy_for_run(&key(&p), 1, 1)
            .is_err()
    );
    synchronize(&mut h, &first);
    assert_eq!(
        h.orchestrator
            .native_policy_for_run(&key(&p), 1, 1)
            .unwrap(),
        first
    );
}

#[test]
fn routine_policy_cache_effects_off_and_foreign_device_refuse_without_widening() {
    for global in [false, true] {
        let (mut h, p, first) = linked();
        let foreign = DeviceId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
        assert!(
            h.orchestrator
                .synchronize_routine_policy(first.routine.clone().unwrap(), foreign)
                .is_err()
        );
        synchronize(&mut h, &first);
        let mut off = first.clone();
        if global {
            off.global.key.request = BotRequestId::mint().into();
            off.global.config_revision = 2;
            off.global.policy_revision = 2;
            off.global.effects_allowed = false;
            h.orchestrator
                .synchronize_native_policy(off.global, p.slot.owner.device)
                .unwrap();
        } else {
            let barrier = off.routine.as_mut().unwrap();
            barrier.request = BotRequestId::mint().into();
            barrier.grant_revision = 2;
            barrier.effects_allowed = false;
            h.orchestrator
                .synchronize_routine_policy(barrier.clone(), p.slot.owner.device)
                .unwrap();
        }
        assert!(
            h.orchestrator
                .native_policy_for_run(
                    &key(&p),
                    if global { 2 } else { 1 },
                    if global { 2 } else { 1 }
                )
                .is_err()
        );
    }
}

#[test]
fn routine_policy_cache_failed_current_read_never_falls_back_to_cached_proof() {
    let (mut h, p, fresh) = linked();
    synchronize(&mut h, &fresh);
    for failed in ["bot_current_policy", "bot_current_routine_policy"] {
        h.ledger.lock().unwrap().fail = Some(failed);
        assert!(
            h.orchestrator
                .native_policy_for_run(&key(&p), 1, 1)
                .is_err()
        );
        h.ledger.lock().unwrap().fail = None;
        assert_eq!(
            h.orchestrator
                .native_policy_for_run(&key(&p), 1, 1)
                .unwrap(),
            fresh
        );
    }
}

#[test]
fn routine_policy_cache_rejects_changed_digest_even_when_both_durable_reads_agree() {
    let (mut h, p, fresh) = linked();
    synchronize(&mut h, &fresh);
    // The returned retained/current rows both carry the same corrupted digest;
    // comparing those rows alone cannot prove the synchronized cache payload.
    h.ledger.lock().unwrap().corrupt_native_policy_digest = true;
    assert!(
        h.orchestrator
            .native_policy_for_run(&key(&p), 1, 1)
            .is_err()
    );
    h.ledger.lock().unwrap().corrupt_native_policy_digest = false;
    assert_eq!(
        h.orchestrator
            .native_policy_for_run(&key(&p), 1, 1)
            .unwrap(),
        fresh
    );
}
