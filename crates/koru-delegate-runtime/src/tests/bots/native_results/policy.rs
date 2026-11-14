// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn routine_policy_replay_preserves_old_result_but_never_leases_prepared_or_changed_request() {
    let (mut h, _, fence, mut policy, request) = active();
    let device = fence.owner.device;
    h.orchestrator
        .synchronize_native_policy(policy.clone(), device)
        .unwrap();
    let first = h
        .orchestrator
        .prepare_native_effect(&request, device)
        .unwrap();
    assert_eq!(first.disposition, NativeResultDisposition::Execute);
    let mut unleased = request.clone();
    unleased.key.operation_id = uuid::Uuid::new_v4();
    h.schedule_store.prepare_native_result(&unleased).unwrap();
    policy.key.request = BotRequestId::mint().into();
    policy.config_revision = 2;
    policy.policy_revision = 2;
    policy.effects_allowed = false;
    assert_eq!(
        h.orchestrator
            .synchronize_native_policy(policy, device)
            .unwrap()
            .state,
        BotPolicyBarrierState::Pending
    );
    let replay = h
        .orchestrator
        .prepare_native_effect(&request, device)
        .unwrap();
    assert_eq!(replay.target, first.target);
    assert_eq!(replay.disposition, NativeResultDisposition::ReconcileOnly);
    let mut changed = request.clone();
    changed.title.push_str(" changed");
    assert!(
        h.orchestrator
            .prepare_native_effect(&changed, device)
            .is_err()
    );
    unleased.config_revision = 2;
    unleased.policy_revision = 2;
    assert!(
        h.orchestrator
            .prepare_native_effect(&unleased, device)
            .is_err()
    );
    assert!(
        h.schedule_store
            .native_result(&unleased.key)
            .unwrap()
            .unwrap()
            .lease
            .is_none()
    );
    let mut fresh = unleased.clone();
    fresh.key.operation_id = uuid::Uuid::new_v4();
    assert!(
        h.orchestrator
            .prepare_native_effect(&fresh, device)
            .is_err()
    );
    assert!(
        h.schedule_store
            .native_result(&fresh.key)
            .unwrap()
            .is_none()
    );
}

#[test]
fn routine_policy_composition_preserves_interactive_subject_and_refuses_pending_global() {
    let (mut h, submission, fence, mut policy, request) = active();
    let key = BotRunKey {
        owner: fence.owner,
        bot: fence.bot,
        run: fence.run,
    };
    h.orchestrator
        .synchronize_native_policy(policy.clone(), fence.owner.device)
        .unwrap();
    // A same-Bot Routine cache entry must never appear in Interactive proof.
    let barrier = RoutinePolicyBarrier {
        owner: fence.owner,
        bot: fence.bot,
        schedule: ScheduleId::from_uuid(uuid::Uuid::new_v4()),
        request: BotRequestId::mint().into(),
        grant_revision: 1,
        policy: submission.policy,
        effects_allowed: true,
    };
    h.schedule_store
        .inner
        .lock()
        .unwrap()
        .register_routine_route(&RoutineRouteIdentity {
            owner: barrier.owner,
            bot: barrier.bot,
            schedule: barrier.schedule,
            workspace: barrier.policy.workspace,
            creation_request: BotRequestId::mint(),
            seed_hash: RoutineDigest32::from_bytes([7; 32]),
            created_at_ms: 0,
        })
        .unwrap();
    h.orchestrator
        .synchronize_routine_policy(barrier, fence.owner.device)
        .unwrap();
    assert!(
        h.orchestrator
            .native_policy_for_run(&key, 1, 1)
            .unwrap()
            .routine
            .is_none()
    );
    assert_eq!(
        h.orchestrator
            .prepare_native_effect(&request, fence.owner.device)
            .unwrap()
            .disposition,
        NativeResultDisposition::Execute
    );
    policy.key.request = BotRequestId::mint().into();
    policy.config_revision = 2;
    policy.policy_revision = 2;
    assert_eq!(
        h.orchestrator
            .synchronize_native_policy(policy, fence.owner.device)
            .unwrap()
            .state,
        BotPolicyBarrierState::Pending
    );
    assert!(h.orchestrator.native_policy_for_run(&key, 2, 2).is_err());
    // Retained reconciliation is independent of the fresh cache check.
    assert!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .native_result(&request.key)
            .unwrap()
            .is_some()
    );
}
