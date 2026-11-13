// SPDX-License-Identifier: MPL-2.0

use super::*;

pub(super) fn policy(s: &BotSubmission, revision: u64) -> BotPolicyBarrier {
    BotPolicyBarrier {
        key: BotPolicyBarrierKey {
            owner: s.provenance.owner,
            bot: s.provenance.bot,
            request: BotRequestId::from_uuid(v4(89 + revision as u8)).into(),
        },
        config_revision: revision,
        policy_revision: s.provenance.policy_revision,
        policy: s.policy.clone(),
        effects_allowed: true,
    }
}

fn barrier(s: &BotSubmission) -> BotAdmissionBarrier {
    BotAdmissionBarrier {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        request: BotRequestId::from_uuid(v4(0xef)),
        epoch: s.provenance.config_revision,
        paused: false,
    }
}

pub(super) fn replay(orchestrator: &mut Orchestrator, s: &BotSubmission) {
    replay_pair(
        orchestrator,
        barrier(s),
        policy(s, s.provenance.config_revision),
    );
}

pub(super) fn replay_pair(
    orchestrator: &mut Orchestrator,
    barrier: BotAdmissionBarrier,
    policy: BotPolicyBarrier,
) {
    let boot = orchestrator
        .bot_hello(barrier.owner, BotBootScan::Probe {}, barrier.owner.device)
        .unwrap()
        .boot;
    let device = barrier.owner.device;
    let receipt = orchestrator
        .synchronize_boot_bot(boot, barrier, policy, device)
        .unwrap();
    assert_eq!(receipt.state, BotBootState::Ready);
}

#[test]
fn new_submission_requires_exact_current_pair_before_reserving_or_launching() {
    let mut h = bot_harness();
    let s = submission(60);
    let owner = s.provenance.owner;
    h.orchestrator.bot_submit(s.clone(), owner.device).unwrap();
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_none()
    );
    assert!(
        h.schedule_store
            .active_bot_executions(owner.device)
            .unwrap()
            .is_empty()
    );
    assert!(h.calls().is_empty());
    // Ordinary policy and admission operations never acknowledge this boot.
    h.orchestrator
        .bot_barrier(&barrier(&s), owner.device)
        .unwrap();
    h.orchestrator
        .synchronize_native_policy(policy(&s, 1), owner.device)
        .unwrap();
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_none()
    );
    replay(&mut h.orchestrator, &s);
    replay(&mut h.orchestrator, &s);
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_some()
    );
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        h.calls()
            .iter()
            .filter(|c| matches!(c, Call::Create { .. }))
            .count(),
        1
    );
}

#[test]
fn reboot_changes_identity_and_rejects_old_replay_without_new_launch() {
    let mut h = bot_harness();
    let s = submission(61);
    let owner = s.provenance.owner;
    h.orchestrator.bot_submit(s.clone(), owner.device).unwrap();
    let old = h
        .orchestrator
        .bot_hello(owner, BotBootScan::Start {}, owner.device)
        .unwrap();
    replay(&mut h.orchestrator, &s);
    let mut h = restarted(h, (host(1), CreationLookup::NoEvidence));
    let new = h
        .orchestrator
        .bot_hello(owner, BotBootScan::Start {}, owner.device)
        .unwrap();
    assert_ne!(old.boot, new.boot);
    assert_eq!(new.entries.len(), 1);
    assert!(
        h.orchestrator
            .synchronize_boot_bot(old.boot, barrier(&s), policy(&s, 1), owner.device)
            .is_err()
    );
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_none()
    );
    replay(&mut h.orchestrator, &s);
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_some()
    );
}

#[test]
fn historical_effective_policy_cannot_acknowledge_newer_current_policy() {
    let mut h = bot_harness();
    let s = submission(62);
    let owner = s.provenance.owner;
    h.orchestrator.bot_submit(s.clone(), owner.device).unwrap();
    replay(&mut h.orchestrator, &s);
    let boot = h
        .orchestrator
        .bot_hello(owner, BotBootScan::Probe {}, owner.device)
        .unwrap()
        .boot;
    // Policy advanced before its corresponding admission replay. The historical
    // install still returns Effective, but boot replay must read current state.
    h.orchestrator
        .synchronize_native_policy(policy(&s, 2), owner.device)
        .unwrap();
    assert!(
        h.orchestrator
            .synchronize_boot_bot(boot, barrier(&s), policy(&s, 1), owner.device)
            .is_err()
    );
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_none()
    );
    let mut next = barrier(&s);
    next.epoch = 2;
    replay_pair(&mut h.orchestrator, next.clone(), policy(&s, 2));
    next.epoch = 3;
    next.paused = true;
    h.orchestrator.bot_barrier(&next, owner.device).unwrap();
    assert!(
        h.orchestrator
            .synchronize_boot_bot(boot, barrier(&s), policy(&s, 1), owner.device)
            .is_err()
    );
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_none()
    );
}

#[test]
fn inventory_cursor_is_owner_boot_bound_and_acknowledgements_do_not_starve_later_bots() {
    let mut h = bot_harness();
    let first = submission(1);
    let owner = first.provenance.owner;
    let mut submissions = Vec::new();
    for n in 1..=70 {
        let mut s = submission(n);
        s.provenance.bot = BotId::from_uuid(v4(n));
        h.orchestrator.bot_submit(s.clone(), owner.device).unwrap();
        submissions.push(s);
    }
    let page = h
        .orchestrator
        .bot_hello(owner, BotBootScan::Start {}, owner.device)
        .unwrap();
    assert_eq!(page.entries.len(), 16);
    let cursor = page.next.unwrap();
    let mut invalid = cursor.clone();
    invalid.boot = DelegateBootId::mint();
    assert!(
        h.orchestrator
            .bot_hello(
                owner,
                BotBootScan::Continue { cursor: invalid },
                owner.device
            )
            .is_err()
    );
    let mut foreign = owner;
    foreign.user_scope = serde_json::from_value(serde_json::json!(v4(0xfe))).unwrap();
    assert!(
        h.orchestrator
            .bot_hello(foreign, BotBootScan::Continue { cursor }, owner.device)
            .is_err()
    );
    for s in &submissions {
        replay(&mut h.orchestrator, s);
    }
    // More than the separate native payload cache's 64 entries remain ready.
    let mut scan = BotBootScan::Start {};
    let mut pages = 0;
    loop {
        let page = h.orchestrator.bot_hello(owner, scan, owner.device).unwrap();
        assert!(page.entries.is_empty());
        pages += 1;
        match page.next {
            Some(cursor) => scan = BotBootScan::Continue { cursor },
            None => break,
        }
    }
    assert_eq!(pages, 5);
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&submissions[0]), owner.device)
            .unwrap()
            .is_some()
    );
}

#[test]
fn no_unsettled_identity_cannot_populate_ack_and_settlement_requires_new_replay() {
    let mut h = bot_harness();
    let s = submission(63);
    let owner = s.provenance.owner;
    let boot = h
        .orchestrator
        .bot_hello(owner, BotBootScan::Probe {}, owner.device)
        .unwrap()
        .boot;
    assert_eq!(
        h.orchestrator
            .synchronize_boot_bot(boot, barrier(&s), policy(&s, 1), owner.device)
            .unwrap()
            .state,
        BotBootState::NoUnsettledWork
    );
    h.orchestrator.bot_submit(s.clone(), owner.device).unwrap();
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_none()
    );
    replay(&mut h.orchestrator, &s);
    h.orchestrator
        .bot_stop(&key(&s), owner.device, None)
        .unwrap();
    let mut next = submission(64);
    next.provenance.bot = s.provenance.bot;
    h.orchestrator
        .bot_submit(next.clone(), owner.device)
        .unwrap();
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&next), owner.device)
            .unwrap()
            .is_none()
    );
    replay(&mut h.orchestrator, &next);
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&next), owner.device)
            .unwrap()
            .is_some()
    );
}

#[test]
fn safe_reservations_requeue_but_existing_attempt_retains_capacity() {
    let mut h = bot_harness();
    let live = submission(65);
    let _ = prepared(&mut h, &live);
    let mut unused = submission(66);
    unused.provenance.bot = BotId::mint();
    h.orchestrator
        .bot_submit(unused.clone(), unused.provenance.owner.device)
        .unwrap();
    h.schedule_store.reserve_bot(unused.request.run).unwrap();
    h.orchestrator
        .recover_unused_bot_reservations(live.provenance.owner.device)
        .unwrap();
    assert_eq!(
        h.schedule_store
            .bot_execution(unused.request.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Queued
    );
    assert_eq!(
        h.schedule_store
            .bot_execution(live.request.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Executing
    );
    assert_eq!(
        h.schedule_store
            .active_bot_executions(live.provenance.owner.device)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn failed_current_read_and_newer_submission_stay_held_until_exact_replay() {
    let mut h = bot_harness();
    let s = submission(67);
    let owner = s.provenance.owner;
    h.orchestrator.bot_submit(s.clone(), owner.device).unwrap();
    replay(&mut h.orchestrator, &s);
    h.ledger.lock().unwrap().fail = Some("bot_current_policy");
    assert!(h.orchestrator.bot_dispatch(&key(&s), owner.device).is_err());
    assert!(
        h.schedule_store
            .active_bot_executions(owner.device)
            .unwrap()
            .is_empty()
    );
    h.ledger.lock().unwrap().fail = None;
    let mut newer = submission(68);
    newer.provenance.config_revision = 2;
    newer.provenance.policy_revision = 2;
    h.orchestrator
        .bot_submit(newer.clone(), owner.device)
        .unwrap();
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_none()
    );
    replay(&mut h.orchestrator, &newer);
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_some()
    );
}

#[test]
fn direct_start_cannot_bypass_boot_gate_with_retained_reservation() {
    let mut h = bot_harness();
    let s = submission(69);
    h.orchestrator
        .bot_submit(s.clone(), s.provenance.owner.device)
        .unwrap();
    h.schedule_store.reserve_bot(s.request.run).unwrap();
    assert!(
        h.orchestrator
            .start(
                s.request.run,
                StartOrigin::Bot {
                    bot: s.provenance.bot
                }
            )
            .is_err()
    );
    assert!(
        h.schedule_store
            .current_attempt(s.request.run)
            .unwrap()
            .is_none()
    );
    assert!(h.calls().is_empty());
    replay(&mut h.orchestrator, &s);
    assert!(
        h.orchestrator
            .start(
                s.request.run,
                StartOrigin::Bot {
                    bot: s.provenance.bot
                }
            )
            .is_ok()
    );
}

#[test]
fn ordinary_run_starts_while_bot_waits_for_replay() {
    let mut h = bot_harness();
    let s = submission(70);
    h.orchestrator
        .bot_submit(s.clone(), s.provenance.owner.device)
        .unwrap();
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), s.provenance.owner.device)
            .unwrap()
            .is_none()
    );
    let run = run_id(71);
    h.orchestrator
        .create(request(run, h.scope()), workspace())
        .unwrap();
    assert!(h.orchestrator.start(run, StartOrigin::Manual).is_ok());
    assert!(
        h.schedule_store
            .current_attempt(s.request.run)
            .unwrap()
            .is_none()
    );
}
