// SPDX-License-Identifier: MPL-2.0

//! M8: M6's two admission points behind the gate (contract 3.3, rows 19 and
//! 20). Cohort `m8_gate_team`. A Bot's ask of another Bot is refused while
//! the team is paused, and a waiting parent's resume is held, with no time
//! limit, by a paused team and by a delegate run that was not told.

use super::*;
use koru_delegate_core::{BotRequestId, BotTeamBarrier};

fn team(chain: &ChildChain, epoch: u64, paused: bool) -> BotTeamBarrier {
    BotTeamBarrier {
        owner: chain.parent.owner,
        epoch,
        paused,
        request: (epoch > 0).then(BotRequestId::mint),
    }
}

fn tell(chain: &mut ChildChain, epoch: u64, paused: bool) {
    let barrier = team(chain, epoch, paused);
    let device = device(chain);
    chain
        .orchestrator
        .bot_team_barrier(&barrier, device)
        .unwrap();
}

/// Row 19: the parent is running (a team pause stops nothing that runs),
/// but every recipient is held, so the ask is refused whole and commits
/// nothing. After the resume the same ask is admitted.
#[test]
fn m8_gate_team_a_child_request_is_refused_while_the_team_is_paused() {
    let mut chain = child_chain();
    let successor = SessionId::mint();
    let turn = AgentTurnId::mint();
    chain
        .parent
        .script_resume(&mut chain.sessions.lock().unwrap(), successor, turn);
    tell(&mut chain, 1, true);
    let request = chain.parent.ask(&mut chain.orchestrator, &[1, 2]);
    match send(&mut chain, &request) {
        ChildRequestOutcome::Refused(refused) => {
            assert_eq!(refused.code, ChildRefusal::RecipientPaused);
            assert_eq!(refused.ordinal, Some(0));
        }
        other => panic!("the ask was not refused: {other:?}"),
    }
    assert!(execution(&mut chain).is_none(), "no wait was opened");
    for item in &request.requests {
        let run = item.binding.child_run;
        assert!(
            chain
                .orchestrator
                .bot_ledger()
                .unwrap()
                .bot_execution(run)
                .unwrap()
                .is_none(),
            "no child was admitted"
        );
    }
    assert_eq!(parent_state(&mut chain), BotAdmissionState::Executing);

    tell(&mut chain, 2, false);
    assert!(matches!(
        send(&mut chain, &request),
        ChildRequestOutcome::Created(_)
    ));
}

/// Row 19, not told: the ask is admitted and its children are queued, as a
/// submission is. They are not dispatched until the run is told.
#[test]
fn m8_gate_team_a_child_request_is_admitted_and_queued_while_not_told() {
    let mut chain = child_chain();
    chain.orchestrator.require_team_value(true);
    let (_, requested) = asked(&mut chain, &[1]);
    let receipt = requested.receipts[0].clone();
    assert_eq!(receipt.child, BotAdmissionState::Queued);
    let key = BotRunKey {
        owner: chain.parent.owner,
        bot: receipt.recipient,
        run: receipt.child_run,
    };
    let device = device(&chain);
    assert!(
        chain
            .orchestrator
            .bot_dispatch(&key, device)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        child_state(&mut chain, receipt.child_run),
        BotAdmissionState::Queued
    );
}

/// Row 20: every request has settled, so the parent's successor is due. A
/// paused team holds the reservation, changes nothing, and never turns into
/// a cleanup; after the resume the parent continues.
#[test]
fn m8_gate_team_a_parents_resume_is_held_while_the_team_is_paused() {
    let (mut chain, requested, mut bridge) = held(&[1]);
    stop_child(&mut chain, &requested.receipts[0]);
    tell(&mut chain, 1, true);
    let before = execution(&mut chain).unwrap();
    assert_eq!(before.wait.phase, BotWaitPhase::Retired);
    for _ in 0..3 {
        assert_eq!(
            advance(&mut chain, &mut bridge),
            Step::Waiting("bot_team_paused")
        );
    }
    // Longer than the stall limit of counted time: still only waiting.
    super::clock::count(&mut chain, CHILD_STALL_LIMIT_MS + 20_000);
    assert_eq!(
        advance(&mut chain, &mut bridge),
        Step::Waiting("bot_team_paused")
    );
    let after = execution(&mut chain).unwrap();
    assert_eq!(after.wait, before.wait, "the wait is as it was");
    assert!(after.resume.is_none(), "nothing was reserved");
    assert_eq!(parent_state(&mut chain), BotAdmissionState::Executing);

    tell(&mut chain, 2, false);
    resume(&mut chain, &mut bridge);
    assert!(execution(&mut chain).is_none(), "the wait settled");
}

/// Row 20, not told: a delegate run that was not told the team value holds
/// the resume the same way, and "never paused" releases it.
#[test]
fn m8_gate_team_a_parents_resume_is_held_while_not_told() {
    let (mut chain, requested, mut bridge) = held(&[1]);
    stop_child(&mut chain, &requested.receipts[0]);
    chain.orchestrator.require_team_value(true);
    for _ in 0..3 {
        assert_eq!(
            advance(&mut chain, &mut bridge),
            Step::Waiting("bot_boot_pending")
        );
    }
    assert!(execution(&mut chain).unwrap().resume.is_none());
    tell(&mut chain, 0, false);
    resume(&mut chain, &mut bridge);
    assert!(execution(&mut chain).is_none());
}

/// What the code does today, stated so that it is not mistaken for a
/// promise: the 15-minute clock of an open request is paused by its parent
/// Bot's **own** barrier, in the ledger. The team value is memory of the
/// delegate and the ledger does not see it, so under a team pause the clock
/// keeps counting and the request still times out at its budget.
#[test]
fn m8_gate_team_a_request_clock_still_counts_under_a_team_pause() {
    let (mut chain, requested, _bridge) = held(&[1]);
    tell(&mut chain, 1, true);
    assert_eq!(super::clock::count(&mut chain, BOT_CHILD_WAIT - 10_000), 0);
    let open = requests(&mut chain);
    assert_eq!(open[0].state, ChildRequestState::Open);
    assert_eq!(open[0].clock, ChildClock::Running);
    assert!(open[0].elapsed_ms > 0);
    assert_eq!(super::clock::count(&mut chain, 10_000), 1);
    let settled = requests(&mut chain);
    assert_eq!(
        settled[0].child_request,
        requested.receipts[0].child_request
    );
    assert_eq!(settled[0].state, ChildRequestState::Settled);
}
