// SPDX-License-Identifier: MPL-2.0

//! M8 piece B: the delegate's team value. Cohorts `m8_team_value`,
//! `m8_gate_team`, `m8_delegate_restart_closed` and
//! `m8_native_approval_under_pause`.
use super::*;

fn team(owner: BotOwner, epoch: u64, paused: bool) -> BotTeamBarrier {
    BotTeamBarrier {
        owner,
        epoch,
        paused,
        request: (epoch > 0).then(BotRequestId::mint),
    }
}

/// A harness whose delegate run was told nothing about the team: it is
/// closed until it is told (contract 3.2), as every delegate run starts.
fn closed_harness() -> Harness {
    let h = bot_harness();
    assert!(h.orchestrator.team_value_required());
    h
}

fn tell(h: &mut Harness, barrier: &BotTeamBarrier) -> BotTeamBarrier {
    h.orchestrator
        .bot_team_barrier(barrier, barrier.owner.device)
        .unwrap()
}

fn start_refusal(h: &mut Harness, s: &BotSubmission) -> String {
    code(
        h.orchestrator
            .start(
                s.request.run,
                StartOrigin::Bot {
                    bot: s.provenance.bot,
                },
            )
            .unwrap_err(),
    )
}

/// The switch itself. It is on since the AI worker sends `TeamBarrier`
/// each time its boot `Hello` shows a delegate run it has not told (piece
/// W; proved there by the `m8_team_coordination` cases in
/// `apps/koru-ai-server/src/worker/bots/team_pause/tests.rs`). A delegate
/// run that was told nothing holds every Bot of that owner, and the
/// worker's "never paused" opens it. This case fails if the constant is
/// switched off again, which would let a delegate that restarted under a
/// team pause dispatch what it had queued.
#[test]
fn m8_team_value_is_required_and_the_workers_never_paused_opens_the_run() {
    assert!(
        crate::TEAM_VALUE_REQUIRED,
        "a delegate run that was not told the team value must hold its Bots"
    );
    let mut h = bot_harness();
    assert!(h.orchestrator.team_value_required());
    let s = submission(80);
    let owner = s.provenance.owner;
    assert!(h.orchestrator.bot_team_value(owner).is_none());
    assert_eq!(
        h.orchestrator.bot_held(owner, s.provenance.bot).unwrap(),
        Held::NotTold
    );
    // A worker that said nothing about the team: submit and the boot pair.
    h.orchestrator.bot_submit(s.clone(), owner.device).unwrap();
    boot::replay_untold(&mut h.orchestrator, &s);
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_none(),
        "nothing of a Bot is dispatched before the worker speaks"
    );
    // The worker of this tree: "never paused" for a team with no row.
    tell(&mut h, &BotTeamBarrier::never_paused(owner));
    assert_eq!(
        h.orchestrator.bot_held(owner, s.provenance.bot).unwrap(),
        Held::No
    );
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_some(),
        "the queued task dispatches once the run is told"
    );
    // A later pause is enforced.
    tell(&mut h, &team(owner, 1, true));
    assert_eq!(
        h.orchestrator.bot_held(owner, s.provenance.bot).unwrap(),
        Held::Team
    );
    // The builder every dispatching test uses tells the run the same way.
    let mut h = bot_harness();
    let s = submission(79);
    h.orchestrator.bot_submit(s.clone(), owner.device).unwrap();
    boot::replay(&mut h.orchestrator, &s);
    assert_eq!(
        h.orchestrator.bot_team_value(owner),
        Some(&BotTeamBarrier::never_paused(owner))
    );
}

/// `m8_team_value`: not told is held for dispatch (rows 7 and 8) and for the
/// attempt start (row 9, `bot_boot_pending`), while admit queues (row 6).
#[test]
fn m8_team_value_not_told_holds_dispatch_and_start_while_admit_queues() {
    let mut h = closed_harness();
    let s = submission(81);
    let owner = s.provenance.owner;
    assert_eq!(
        h.orchestrator.bot_held(owner, s.provenance.bot).unwrap(),
        Held::NotTold
    );
    // Row 6: admitted and queued, as today.
    h.orchestrator.bot_submit(s.clone(), owner.device).unwrap();
    assert_eq!(state(&h, &s), BotAdmissionState::Queued);
    boot::replay_untold(&mut h.orchestrator, &s);
    // Row 7: not dispatched; the task stays queued.
    for _ in 0..3 {
        assert!(
            h.orchestrator
                .bot_dispatch(&key(&s), owner.device)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(state(&h, &s), BotAdmissionState::Queued);
    assert!(no_session_was_created(&h));

    // Rows 8 and 9: a reservation is kept and its start refuses.
    let mut h = closed_harness();
    let kept = submission(82);
    reserved_untold(&mut h, &kept);
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&kept), owner.device)
            .unwrap()
            .is_none()
    );
    assert_eq!(state(&h, &kept), BotAdmissionState::Reserved);
    assert_eq!(start_refusal(&mut h, &kept), "bot_boot_pending");
    assert_eq!(state(&h, &kept), BotAdmissionState::Reserved);
    assert!(no_session_was_created(&h));

    // "Never paused" opens: the same reservation now starts.
    tell(&mut h, &BotTeamBarrier::never_paused(owner));
    assert_eq!(
        h.orchestrator.bot_held(owner, kept.provenance.bot).unwrap(),
        Held::No
    );
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&kept), owner.device)
            .unwrap()
            .is_some()
    );
}

/// `m8_team_value`: the apply rule is a Bot barrier's, in memory. An older
/// epoch is `bot_team_stale` and the held epoch can be read; the re-send at
/// that epoch plus one is accepted; the same epoch with other content is
/// `bot_team_conflict`; the same content is answered again.
#[test]
fn m8_team_value_orders_by_epoch_and_refuses_stale_and_conflicting_values() {
    let mut h = closed_harness();
    let owner = submission(83).provenance.owner;
    let never = BotTeamBarrier::never_paused(owner);
    assert_eq!(tell(&mut h, &never), never);
    assert_eq!(tell(&mut h, &never), never, "idempotent");
    let paused = team(owner, 5, true);
    assert_eq!(tell(&mut h, &paused), paused);
    assert_eq!(tell(&mut h, &paused), paused, "a repeat is answered");
    assert_eq!(h.orchestrator.bot_team_value(owner), Some(&paused));

    // Older: stale, and nothing changes. The held epoch is 5.
    for older in [never.clone(), team(owner, 4, false)] {
        assert_eq!(
            code(
                h.orchestrator
                    .bot_team_barrier(&older, owner.device)
                    .unwrap_err()
            ),
            "bot_team_stale"
        );
    }
    assert_eq!(h.orchestrator.bot_team_value(owner).unwrap().epoch, 5);
    // Same epoch, other content: a conflict, and nothing changes.
    for other in [team(owner, 5, false), team(owner, 5, true)] {
        assert_eq!(
            code(
                h.orchestrator
                    .bot_team_barrier(&other, owner.device)
                    .unwrap_err()
            ),
            "bot_team_conflict"
        );
    }
    assert_eq!(h.orchestrator.bot_team_value(owner), Some(&paused));
    // A restored AI owner writes its row at the held epoch plus one, keeping
    // its own `paused`, and sends again: accepted.
    let rewritten = team(owner, 6, false);
    assert_eq!(tell(&mut h, &rewritten), rewritten);
    assert_eq!(
        h.orchestrator
            .bot_held(owner, submission(83).provenance.bot)
            .unwrap(),
        Held::No
    );
}

/// `m8_team_value`: the owner must be this device's, and the value well
/// formed. A refused value stores nothing.
#[test]
fn m8_team_value_refuses_a_wrong_owner_and_a_malformed_value() {
    let mut h = closed_harness();
    let owner = submission(84).provenance.owner;
    let other_device = DeviceId::from_uuid(v4(0xfd));
    assert_eq!(
        code(
            h.orchestrator
                .bot_team_barrier(&BotTeamBarrier::never_paused(owner), other_device)
                .unwrap_err()
        ),
        "bot_owner_mismatch"
    );
    let mut foreign = BotTeamBarrier::never_paused(owner);
    foreign.owner.device = other_device;
    assert_eq!(
        code(
            h.orchestrator
                .bot_team_barrier(&foreign, owner.device)
                .unwrap_err()
        ),
        "bot_owner_mismatch"
    );
    for malformed in [
        BotTeamBarrier {
            owner,
            epoch: 0,
            paused: true,
            request: None,
        },
        BotTeamBarrier {
            owner,
            epoch: 1,
            paused: true,
            request: None,
        },
        BotTeamBarrier {
            owner,
            epoch: 0,
            paused: false,
            request: Some(BotRequestId::mint()),
        },
    ] {
        assert_eq!(
            code(
                h.orchestrator
                    .bot_team_barrier(&malformed, owner.device)
                    .unwrap_err()
            ),
            "bot_invalid"
        );
    }
    assert!(h.orchestrator.bot_team_value(owner).is_none());
    // Another person's team on this device is its own value.
    let mut second = owner;
    second.user_scope = serde_json::from_value(serde_json::json!(v4(0xf1))).unwrap();
    tell(&mut h, &team(second, 1, true));
    assert!(h.orchestrator.bot_team_value(owner).is_none());
    assert_eq!(
        h.orchestrator
            .bot_held(owner, submission(84).provenance.bot)
            .unwrap(),
        Held::NotTold
    );
}

/// `m8_gate_team`: with the team paused and no Bot paused, admit, both
/// dispatch boundaries and the attempt start answer as the contract's 3.3
/// says, and no Bot's barrier moved.
#[test]
fn m8_gate_team_a_paused_team_refuses_admit_and_holds_dispatch_and_start() {
    let mut h = closed_harness();
    let queued = submission(85);
    let owner = queued.provenance.owner;
    tell(&mut h, &BotTeamBarrier::never_paused(owner));
    h.orchestrator
        .bot_submit(queued.clone(), owner.device)
        .unwrap();
    boot::replay(&mut h.orchestrator, &queued);
    let barrier_before = h
        .schedule_store
        .bot_barrier(owner, queued.provenance.bot)
        .unwrap();

    tell(&mut h, &team(owner, 1, true));
    assert_eq!(
        h.orchestrator
            .bot_held(owner, queued.provenance.bot)
            .unwrap(),
        Held::Team
    );
    // Row 6: a new submission is refused with the team's code and nothing is
    // written; the exact replay of an admitted one is still answered.
    let mut fresh = submission(86);
    fresh.provenance.bot = queued.provenance.bot;
    assert_eq!(
        code(
            h.orchestrator
                .bot_submit(fresh.clone(), owner.device)
                .unwrap_err()
        ),
        "bot_team_paused"
    );
    assert!(
        h.schedule_store
            .bot_execution(fresh.request.run)
            .unwrap()
            .is_none()
    );
    assert!(
        h.orchestrator
            .bot_submit(queued.clone(), owner.device)
            .is_ok(),
        "a replay is answered from the ledger"
    );
    // Row 7: not dispatched; the task stays queued.
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&queued), owner.device)
            .unwrap()
            .is_none()
    );
    assert_eq!(state(&h, &queued), BotAdmissionState::Queued);
    assert!(no_session_was_created(&h));
    // No Bot's barrier or revision moved.
    assert_eq!(
        h.schedule_store
            .bot_barrier(owner, queued.provenance.bot)
            .unwrap(),
        barrier_before
    );

    // Resume: the queued task dispatches, and the refused one may be sent.
    tell(&mut h, &team(owner, 2, false));
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&queued), owner.device)
            .unwrap()
            .is_some()
    );
    assert!(h.orchestrator.bot_submit(fresh, owner.device).is_ok());

    // Rows 8 and 9: a reservation is kept and its start refuses.
    let mut h = closed_harness();
    let kept = submission(87);
    tell(&mut h, &BotTeamBarrier::never_paused(owner));
    reserved(&mut h, &kept);
    tell(&mut h, &team(owner, 1, true));
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&kept), owner.device)
            .unwrap()
            .is_none()
    );
    assert_eq!(start_refusal(&mut h, &kept), "bot_boot_pending");
    assert_eq!(state(&h, &kept), BotAdmissionState::Reserved);
    assert!(no_session_was_created(&h));
}

/// `m8_gate_team`: paused both ways, a refusal names the Bot's own pause.
/// A Bot's own Resume under a paused team is accepted and leaves it held by
/// the team; a team resume leaves a Bot paused on its own paused.
#[test]
fn m8_gate_team_paused_both_ways_names_the_bots_own_pause() {
    let mut h = closed_harness();
    let s = submission(88);
    let owner = s.provenance.owner;
    tell(&mut h, &team(owner, 1, true));
    h.orchestrator
        .bot_barrier(&paused_barrier(&s, 1), owner.device)
        .unwrap();
    assert_eq!(
        h.orchestrator.bot_held(owner, s.provenance.bot).unwrap(),
        Held::Both
    );
    assert_eq!(
        code(
            h.orchestrator
                .bot_submit(s.clone(), owner.device)
                .unwrap_err()
        ),
        "bot_fenced",
        "the ledger's refusal of the Bot's own pause"
    );
    // The Bot's own Resume is independent of the team.
    let mut resumed = paused_barrier(&s, 2);
    resumed.paused = false;
    h.orchestrator.bot_barrier(&resumed, owner.device).unwrap();
    assert_eq!(
        h.orchestrator.bot_held(owner, s.provenance.bot).unwrap(),
        Held::Team
    );
    // And the team's resume leaves a Bot's own pause alone.
    h.orchestrator
        .bot_barrier(&paused_barrier(&s, 3), owner.device)
        .unwrap();
    tell(&mut h, &team(owner, 2, false));
    assert_eq!(
        h.orchestrator.bot_held(owner, s.provenance.bot).unwrap(),
        Held::Bot
    );
}

/// `m8_delegate_restart_closed`: the orchestrator is rebuilt over the same
/// ledger with a queued task. Nothing dispatches until `TeamBarrier`
/// arrives, for a paused and for an unpaused team.
#[test]
fn m8_delegate_restart_closed_until_the_team_value_arrives() {
    for paused in [false, true] {
        let mut h = closed_harness();
        let s = submission(89);
        let owner = s.provenance.owner;
        h.orchestrator.bot_submit(s.clone(), owner.device).unwrap();
        tell(&mut h, &team(owner, 3, paused));
        assert_eq!(state(&h, &s), BotAdmissionState::Queued);

        // The delegate restarts: a new run, the same ledger, no memory.
        let mut h = crate::tests::restarted(h, (host(1), CreationLookup::NoEvidence));
        assert!(h.orchestrator.bot_team_value(owner).is_none());
        assert_eq!(
            h.orchestrator.bot_held(owner, s.provenance.bot).unwrap(),
            Held::NotTold
        );
        // Even with the Bot's own boot pair replayed, nothing dispatches.
        boot::replay_untold(&mut h.orchestrator, &s);
        for _ in 0..3 {
            assert!(
                h.orchestrator
                    .bot_dispatch(&key(&s), owner.device)
                    .unwrap()
                    .is_none(),
                "paused={paused}: closed until told"
            );
        }
        assert_eq!(state(&h, &s), BotAdmissionState::Queued);
        assert!(no_session_was_created(&h));
        // A new submission is still admitted and queued meanwhile.
        let mut second = submission(90);
        second.provenance.bot = BotId::from_uuid(v4(0x5a));
        h.orchestrator
            .bot_submit(second.clone(), owner.device)
            .unwrap();
        assert_eq!(state(&h, &second), BotAdmissionState::Queued);

        // The AI worker sees the new run and sends its row.
        tell(&mut h, &team(owner, 3, paused));
        let dispatched = h
            .orchestrator
            .bot_dispatch(&key(&s), owner.device)
            .unwrap()
            .is_some();
        assert_eq!(dispatched, !paused, "paused={paused}");
        if paused {
            assert_eq!(state(&h, &s), BotAdmissionState::Queued);
            tell(&mut h, &team(owner, 4, false));
            assert!(
                h.orchestrator
                    .bot_dispatch(&key(&s), owner.device)
                    .unwrap()
                    .is_some()
            );
        }
    }
}

/// `m8_native_approval_under_pause`: a native approval is answered on the
/// prompt of work that is already running. Under a pause, of the team or of
/// the Bot, the answer still reaches the prompt and no successor exists. A
/// steer of the running turn is accepted too (D14).
#[test]
fn m8_native_approval_under_pause_reaches_the_prompt_and_starts_nothing() {
    for team_scope in [true, false] {
        let (mut h, s, fence, _, _) = native_results::active();
        let owner = fence.owner;
        let offered = vec![decisions::choice("allow"), decisions::choice("reject")];
        let approval = decisions::prompt(&mut h, &fence, offered.clone());
        if team_scope {
            tell(&mut h, &team(owner, 1, true));
            assert_eq!(
                h.orchestrator.bot_held(owner, fence.bot).unwrap(),
                Held::Team
            );
        } else {
            h.orchestrator
                .bot_barrier(&paused_barrier(&s, 2), owner.device)
                .unwrap();
            assert_eq!(
                h.orchestrator.bot_held(owner, fence.bot).unwrap(),
                Held::Bot
            );
        }
        let creations = |h: &Harness| {
            h.calls()
                .iter()
                .filter(|call| matches!(call, Call::Create { .. }))
                .count()
        };
        let before = creations(&h);
        h.orchestrator
            .bot_answer_approval(
                &fence,
                owner.device,
                approval,
                ApprovalDecision::Approve,
                Some(offered[0].id),
            )
            .unwrap();
        assert!(
            h.calls().iter().any(
                |call| matches!(call, Call::AnswerApproval { approval: answered, .. } if *answered == approval)
            ),
            "team_scope={team_scope}: the answer reached the prompt"
        );
        // No decision wait and no successor: the running work continues.
        assert!(
            h.schedule_store
                .inner
                .lock()
                .unwrap()
                .decision_wait_execution(owner, fence.run)
                .unwrap()
                .is_none()
        );
        assert_eq!(creations(&h), before);
        assert_eq!(state(&h, &s), BotAdmissionState::Executing);
    }
}

/// `m8_no_backlog`, queued tasks: tasks queued under a team pause are the
/// tasks the person sent, nothing more. None is dispatched while the team
/// is paused. After the resume they dispatch as they are offered, one per
/// Bot: the first of a Bot's two tasks starts and the second waits for it,
/// as after a Bot's own pause. (The order tasks are offered in is the
/// delegate's queue walk, which this piece does not change.)
#[test]
fn m8_no_backlog_queued_tasks_dispatch_in_order_after_the_team_resumes() {
    let mut h = closed_harness();
    let first = submission(92);
    let owner = first.provenance.owner;
    tell(&mut h, &BotTeamBarrier::never_paused(owner));
    let second = submission(93);
    assert_eq!(first.provenance.bot, second.provenance.bot);
    let mut other = submission(94);
    other.provenance.bot = BotId::from_uuid(v4(0x5b));
    for s in [&first, &second, &other] {
        h.orchestrator.bot_submit(s.clone(), owner.device).unwrap();
    }
    boot::replay(&mut h.orchestrator, &first);
    boot::replay(&mut h.orchestrator, &other);
    tell(&mut h, &team(owner, 1, true));
    for _ in 0..3 {
        for s in [&first, &second, &other] {
            assert!(
                h.orchestrator
                    .bot_dispatch(&key(s), owner.device)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(state(&h, s), BotAdmissionState::Queued);
        }
    }
    assert!(no_session_was_created(&h));

    tell(&mut h, &team(owner, 2, false));
    // Offered in queue order, as the delegate's queue walk offers them.
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&first), owner.device)
            .unwrap()
            .is_some()
    );
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&second), owner.device)
            .unwrap()
            .is_none(),
        "one task per Bot: the second waits for the first"
    );
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&other), owner.device)
            .unwrap()
            .is_some(),
        "another Bot's task dispatches too"
    );
    assert_eq!(state(&h, &second), BotAdmissionState::Queued);
    // Exactly the two sessions of the two tasks that started.
    assert_eq!(
        h.calls()
            .iter()
            .filter(|call| matches!(call, Call::Create { .. }))
            .count(),
        2
    );
}
