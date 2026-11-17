// SPDX-License-Identifier: MPL-2.0
//! Preclaim uses the same current-policy caches without fabricating admission.
use super::*;

fn ready() -> (Harness, RoutinePrepared, FreshBotPolicyProof) {
    let (mut h, _, p, fresh) = fixture();
    let device = p.slot.owner.device;
    h.orchestrator
        .synchronize_native_policy(fresh.global.clone(), device)
        .unwrap();
    h.orchestrator
        .synchronize_routine_policy(fresh.routine.clone().unwrap(), device)
        .unwrap();
    (h, p, fresh)
}

#[test]
fn routine_preclaim_cache_needs_no_run_and_binds_complete_admission_and_preparation() {
    let (mut h, p, fresh) = ready();
    assert_eq!(
        h.orchestrator
            .routine_policy_for_prepared(&p, &admission(&p), &fresh)
            .unwrap(),
        fresh
    );
    assert!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .bot_admission(p.ids.run)
            .unwrap()
            .is_none()
    );
    for field in 0..8 {
        let mut offered = admission(&p);
        let mut candidate = p.clone();
        match field {
            0 => offered.request = BotRequestId::mint(),
            1 => offered.paused = true,
            2 => offered.epoch += 1,
            3 => offered.owner.device = DeviceId::new(),
            4 => offered.bot = BotId::mint(),
            5 => candidate.ids.run = RunId::mint(),
            6 => candidate.slot.schedule = ScheduleId::mint(),
            _ => candidate.definition.revision += 1,
        }
        assert!(
            h.orchestrator
                .routine_policy_for_prepared(&candidate, &offered, &fresh)
                .is_err(),
            "field{field}"
        );
    }
    assert!(h.sessions.lock().unwrap().calls.is_empty());
}

#[test]
fn routine_preclaim_cache_compares_full_offered_proof_and_exact_captured_revisions() {
    let (mut h, p, fresh) = ready();
    for field in 0..9 {
        let mut offered = fresh.clone();
        match field {
            0 => offered.global.key.request = BotRequestId::mint().into(),
            1 => offered.global.config_revision += 1,
            2 => offered.global.policy_revision += 1,
            3 => offered.routine.as_mut().unwrap().grant_revision += 1,
            4 => offered.routine.as_mut().unwrap().request = BotRequestId::mint().into(),
            5 => offered.routine.as_mut().unwrap().effects_allowed = false,
            6 => offered.global.effects_allowed = false,
            7 => offered.routine.as_mut().unwrap().policy.workspace = WorkspaceId::new(),
            _ => offered.routine = None,
        }
        assert!(
            h.orchestrator
                .routine_policy_for_prepared(&p, &admission(&p), &offered)
                .is_err(),
            "field{field}"
        );
    }
}

#[test]
fn routine_preclaim_cache_rechecks_durable_admission_digest_and_both_policy_owners() {
    let (mut h, p, fresh) = ready();
    for failed in ["bot_current_policy", "bot_current_routine_policy"] {
        h.ledger.lock().unwrap().fail = Some(failed);
        assert!(
            h.orchestrator
                .routine_policy_for_prepared(&p, &admission(&p), &fresh)
                .is_err()
        );
        h.ledger.lock().unwrap().fail = None;
    }
    h.ledger.lock().unwrap().corrupt_native_policy_digest = true;
    assert!(
        h.orchestrator
            .routine_policy_for_prepared(&p, &admission(&p), &fresh)
            .is_err()
    );
    h.ledger.lock().unwrap().corrupt_native_policy_digest = false;
    let mut paused = admission(&p);
    paused.epoch += 1;
    paused.request = BotRequestId::mint();
    paused.paused = true;
    h.schedule_store
        .inner
        .lock()
        .unwrap()
        .apply_bot_barrier(&paused)
        .unwrap();
    assert!(
        h.orchestrator
            .routine_policy_for_prepared(&p, &admission(&p), &fresh)
            .is_err()
    );
}

#[test]
fn routine_preclaim_cache_eviction_and_newer_effective_proof_require_resynchronization() {
    let (mut h, p, fresh) = ready();
    for _ in 0..64 {
        let mut other = fresh.routine.clone().unwrap();
        other.schedule = ScheduleId::mint();
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
            .routine_policy_for_prepared(&p, &admission(&p), &fresh)
            .is_err()
    );
    h.orchestrator
        .synchronize_routine_policy(fresh.routine.clone().unwrap(), p.slot.owner.device)
        .unwrap();
    assert_eq!(
        h.orchestrator
            .routine_policy_for_prepared(&p, &admission(&p), &fresh)
            .unwrap(),
        fresh
    );
    let mut newer = fresh.routine.clone().unwrap();
    newer.grant_revision += 1;
    newer.request = BotRequestId::mint().into();
    h.schedule_store
        .inner
        .lock()
        .unwrap()
        .install_routine_policy(&newer)
        .unwrap();
    assert!(
        h.orchestrator
            .routine_policy_for_prepared(&p, &admission(&p), &fresh)
            .is_err()
    );
}

/// `m8_gate_is_the_only_reader`, row 11: the delegate's Routine preclaim
/// asks the gate, and a Bot's own pause keeps the refusal it had.
#[test]
fn m8_gate_is_the_only_reader_and_a_paused_bot_keeps_the_preclaim_refusal() {
    let (mut h, p, fresh) = ready();
    assert_eq!(
        h.orchestrator.bot_held(p.slot.owner, p.slot.bot).unwrap(),
        Held::No
    );
    let mut paused = admission(&p);
    paused.epoch += 1;
    paused.request = BotRequestId::mint();
    paused.paused = true;
    h.schedule_store
        .inner
        .lock()
        .unwrap()
        .apply_bot_barrier(&paused)
        .unwrap();
    assert_eq!(
        h.orchestrator.bot_held(p.slot.owner, p.slot.bot).unwrap(),
        Held::Bot
    );
    let error = h
        .orchestrator
        .routine_policy_for_prepared(&p, &admission(&p), &fresh)
        .unwrap_err();
    assert!(matches!(error, RunError::Port(error) if error.code == "bot_admission_stale"));
}

/// `m8_team_value` and `m8_gate_team`, row 11: the delegate's Routine
/// preclaim is closed while this run was not told the team value
/// (`bot_boot_pending`) and while the team is paused (`bot_team_paused`).
/// Paused both ways, the Bot's own pause is named.
#[test]
fn m8_gate_team_and_m8_team_value_close_the_routine_preclaim() {
    let (mut h, p, fresh) = ready();
    let owner = p.slot.owner;
    let code = |h: &mut Harness| match h.orchestrator.routine_policy_for_prepared(
        &p,
        &admission(&p),
        &fresh,
    ) {
        Ok(_) => "admitted".to_owned(),
        Err(RunError::Port(error)) => error.code,
        Err(other) => panic!("not a coded refusal: {other:?}"),
    };
    assert_eq!(code(&mut h), "admitted");
    // Not told, once that closes.
    h.orchestrator.require_team_value(true);
    assert_eq!(code(&mut h), "bot_boot_pending");
    // Never paused opens.
    h.orchestrator
        .bot_team_barrier(&BotTeamBarrier::never_paused(owner), owner.device)
        .unwrap();
    assert_eq!(code(&mut h), "admitted");
    // The team alone paused.
    let paused_team = BotTeamBarrier {
        owner,
        epoch: 1,
        paused: true,
        request: Some(BotRequestId::mint()),
    };
    h.orchestrator
        .bot_team_barrier(&paused_team, owner.device)
        .unwrap();
    assert_eq!(code(&mut h), "bot_team_paused");
    // Paused both ways: the Bot's own refusal, as before M8.
    let mut paused = admission(&p);
    paused.epoch += 1;
    paused.request = BotRequestId::mint();
    paused.paused = true;
    h.schedule_store
        .inner
        .lock()
        .unwrap()
        .apply_bot_barrier(&paused)
        .unwrap();
    assert_eq!(code(&mut h), "bot_admission_stale");
    assert!(h.sessions.lock().unwrap().calls.is_empty());
}
