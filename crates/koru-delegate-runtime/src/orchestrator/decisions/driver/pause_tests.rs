// SPDX-License-Identifier: MPL-2.0

//! M8 piece C: a Koru decision under a pause (contract 3.5, D2). Under a
//! pause, of the Bot or of its team, no successor is reserved for an answer;
//! nothing is stored and the decision's clock keeps running, so it ends at
//! its usual deadline through the existing expiry (O157).
//!
//! Driven through the public decision-chain builder over the real in-memory
//! ledger, the scripted session host and the builder's shared fake clock.
//! These cases stop where the runtime's own expiry cases stop: the cleanup
//! is requested and owned. The cleanup to the Run's end is driven on the
//! real actor tick in `koru-delegate-host`.

use super::*;
use crate::DecisionResumeStep as Step;
use crate::test_support::{
    DecisionChain, FIXTURE_NOW_MS, SessionCall, decision_chain, private_decision_chain,
    publish_owed,
};
use koru_delegate_core::decisions::{DecisionAuthorization, DecisionClosureReason, DecisionPhase};
use koru_delegate_core::{
    AgentTurnId, BotAdmissionBarrier, BotRequestId, BotRunKey, BotTeamBarrier, SessionId,
};
use koru_delegate_sqlite::{DecisionCleanupOrigin, DecisionRecord};
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    Bot,
    Team,
}

const SCOPES: [Scope; 2] = [Scope::Bot, Scope::Team];

fn advance(chain: &mut DecisionChain) -> Step {
    let (owner, run) = (chain.prepared.owner, chain.prepared.run);
    let mut bridge = chain.prepared.enrollment_bridge();
    chain
        .orchestrator
        .bot_advance_decision_resume(owner, run, &mut bridge)
        .unwrap()
}

fn decision(chain: &mut DecisionChain) -> DecisionRecord {
    let (owner, run, id) = (
        chain.prepared.owner,
        chain.prepared.run,
        chain.prepared.decision,
    );
    chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_decision(owner, run, id)
        .unwrap()
        .unwrap()
}

fn cleanup_cause(chain: &mut DecisionChain) -> Option<DecisionClosureReason> {
    let (owner, run) = (chain.prepared.owner, chain.prepared.run);
    match chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .decision_cleanup_ownership(owner, run)
        .unwrap()?
        .origin
    {
        DecisionCleanupOrigin::Requested { cause, .. } => Some(cause),
        _ => None,
    }
}

/// A decision a person may answer: the origin is stopped, every projection
/// is published, and the driver waits for the answer.
fn held(mut chain: DecisionChain) -> DecisionChain {
    chain.prepared.script_resume(
        &mut chain.sessions.lock().unwrap(),
        SessionId::mint(),
        AgentTurnId::mint(),
    );
    for expected in [
        Step::EnrollmentRequested,
        Step::Enrolled,
        Step::OriginStopped,
        Step::Waiting("bot_answer_pending"),
    ] {
        assert_eq!(advance(&mut chain), expected);
    }
    let run = chain.prepared.run;
    publish_owed(&mut chain.orchestrator, run).unwrap();
    chain
}

/// Pause (`paused`) or resume at `scope`. At Bot scope each change moves the
/// Bot's revision, as the AI owner's Pause and Resume do; at team scope no
/// revision of any Bot moves.
fn set_pause(chain: &mut DecisionChain, scope: Scope, epoch: u64, paused: bool) {
    let device = chain.prepared.owner.device;
    match scope {
        Scope::Bot => {
            let barrier = BotAdmissionBarrier {
                owner: chain.prepared.owner,
                bot: chain.prepared.fence.bot,
                request: BotRequestId::mint(),
                epoch,
                paused,
            };
            chain.orchestrator.bot_barrier(&barrier, device).unwrap();
        }
        Scope::Team => {
            let barrier = BotTeamBarrier {
                owner: chain.prepared.owner,
                epoch,
                paused,
                request: Some(BotRequestId::mint()),
            };
            chain
                .orchestrator
                .bot_team_barrier(&barrier, device)
                .unwrap();
        }
    }
}

fn waiting_code(scope: Scope) -> &'static str {
    match scope {
        Scope::Bot => "bot_paused",
        Scope::Team => "bot_team_paused",
    }
}

fn creations(chain: &DecisionChain) -> usize {
    chain
        .sessions
        .lock()
        .unwrap()
        .calls()
        .iter()
        .filter(|call| matches!(call, SessionCall::Create { .. }))
        .count()
}

fn rebuilt(chain: DecisionChain) -> DecisionChain {
    let DecisionChain {
        orchestrator,
        sessions,
        clock,
        documents,
        waits,
        prepared,
    } = chain;
    DecisionChain {
        orchestrator: Orchestrator::new(
            orchestrator.into_ports(),
            crate::Options {
                reask_attempts: 3,
                ..crate::Options::default()
            },
        ),
        sessions,
        clock,
        documents,
        waits,
        prepared,
    }
}

/// `m8_decision_not_reserved_under_pause`: an answered Koru edit decision
/// under a pause, at each scope, is not reserved; no row changes; Stop still
/// closes it.
#[test]
fn m8_decision_not_reserved_under_pause_at_each_scope_and_stop_still_closes_it() {
    for scope in SCOPES {
        let mut chain = held(decision_chain());
        chain.prepared.allow_once(&mut chain.orchestrator).unwrap();
        set_pause(&mut chain, scope, 2, true);
        let before = decision(&mut chain);
        assert_eq!(
            before.receipt.state.authorization,
            DecisionAuthorization::Granted
        );
        let created = creations(&chain);
        for _ in 0..3 {
            assert_eq!(advance(&mut chain), Step::Waiting(waiting_code(scope)));
        }
        let after = decision(&mut chain);
        assert_eq!(after, before, "{scope:?}: no row changed");
        assert!(after.resume.is_none(), "{scope:?}: nothing was reserved");
        assert_eq!(creations(&chain), created, "{scope:?}: nothing launched");
        assert_eq!(cleanup_cause(&mut chain), None);

        // A person's Stop is not a start: it closes the decision as before.
        let key = BotRunKey {
            owner: chain.prepared.owner,
            bot: chain.prepared.fence.bot,
            run: chain.prepared.run,
        };
        let device = chain.prepared.owner.device;
        let _ = chain.orchestrator.bot_stop(&key, device, None);
        let stopped = decision(&mut chain);
        assert_eq!(
            (stopped.receipt.state.phase, stopped.wait.phase),
            (DecisionPhase::StopPending, BotWaitPhase::CleanupPending),
            "{scope:?}: Stop owns the cleanup"
        );
        assert_eq!(
            cleanup_cause(&mut chain),
            Some(DecisionClosureReason::Canceled)
        );
        assert!(stopped.resume.is_none());
        assert_eq!(creations(&chain), created);
    }
}

/// `m8_decision_not_reserved_under_pause`, row 17 of the contract's 3.3: a
/// private operation's decision resumes by successor too, and is held the
/// same way.
#[test]
fn m8_decision_not_reserved_under_pause_for_a_private_operation_decision() {
    for scope in SCOPES {
        let mut chain = held(private_decision_chain());
        chain.prepared.allow_once(&mut chain.orchestrator).unwrap();
        set_pause(&mut chain, scope, 2, true);
        let before = decision(&mut chain);
        assert_eq!(advance(&mut chain), Step::Waiting(waiting_code(scope)));
        let after = decision(&mut chain);
        assert_eq!(after, before);
        assert!(after.resume.is_none());
    }
}

/// `m8_decision_expires_under_pause`: the deadline passes while paused, the
/// existing expiry (O157) closes the decision and hands its wait to the
/// cleanup owner, and nothing launches after the unpause.
#[test]
fn m8_decision_expires_under_pause_answered_or_not_and_nothing_launches_after_unpause() {
    for scope in SCOPES {
        for answered in [true, false] {
            let mut chain = held(decision_chain());
            if answered {
                chain.prepared.allow_once(&mut chain.orchestrator).unwrap();
            }
            set_pause(&mut chain, scope, 2, true);
            let deadline = decision(&mut chain).receipt.deadline.deadline_at_ms;
            assert_eq!(
                deadline,
                FIXTURE_NOW_MS + koru_delegate_core::decisions::DECISION_WAIT_MILLIS,
                "the pause did not move the deadline"
            );
            // One millisecond short: still only waiting, nothing closed.
            chain.clock.store(deadline - 1, Ordering::SeqCst);
            let waiting = if answered {
                waiting_code(scope)
            } else {
                "bot_answer_pending"
            };
            assert_eq!(advance(&mut chain), Step::Waiting(waiting));
            assert_eq!(cleanup_cause(&mut chain), None);
            let created = creations(&chain);

            chain.clock.store(deadline, Ordering::SeqCst);
            assert_eq!(advance(&mut chain), Step::Expired, "{scope:?}/{answered}");
            let expired = decision(&mut chain);
            assert_eq!(
                (
                    expired.receipt.state.authorization,
                    expired.receipt.state.phase,
                    expired.wait.phase
                ),
                (
                    DecisionAuthorization::Expired,
                    DecisionPhase::StopPending,
                    BotWaitPhase::CleanupPending
                )
            );
            assert_eq!(
                cleanup_cause(&mut chain),
                Some(DecisionClosureReason::Expired)
            );
            let run = chain.prepared.run;
            assert!(chain.orchestrator.runs[&run].decision_cleanup.is_some());

            // Unpause: the expired decision does not come back.
            set_pause(&mut chain, scope, 3, false);
            for _ in 0..3 {
                assert_eq!(advance(&mut chain), Step::Waiting("bot_cleanup_owned"));
            }
            assert!(chain.prepared.allow_once(&mut chain.orchestrator).is_err());
            let still = decision(&mut chain);
            assert_eq!(still.receipt, expired.receipt);
            assert!(still.resume.is_none());
            assert_eq!(creations(&chain), created, "nothing launched");
        }
    }
}

/// `m8_decision_expires_under_pause`, after the delegate is rebuilt over the
/// same ledger: the answered decision another boot created is closed on its
/// first turn, paused or not, and nothing launches after the unpause.
#[test]
fn m8_decision_expires_under_pause_after_the_delegate_is_rebuilt_over_the_same_ledger() {
    for scope in SCOPES {
        let mut chain = held(decision_chain());
        chain.prepared.allow_once(&mut chain.orchestrator).unwrap();
        set_pause(&mut chain, scope, 2, true);
        assert_eq!(advance(&mut chain), Step::Waiting(waiting_code(scope)));
        let created = creations(&chain);
        // The saved deadline is still nine minutes away.
        chain.clock.store(FIXTURE_NOW_MS + 60_000, Ordering::SeqCst);
        let mut chain = rebuilt(chain);
        // The durable open-wait index still offers the Run, and the deadline
        // comes before the gate.
        assert_eq!(advance(&mut chain), Step::Expired, "{scope:?}");
        let closed = decision(&mut chain);
        assert_eq!(
            (
                closed.receipt.state.authorization,
                closed.receipt.state.phase,
                closed.wait.phase
            ),
            (
                DecisionAuthorization::Expired,
                DecisionPhase::StopPending,
                BotWaitPhase::CleanupPending
            )
        );
        assert_eq!(
            cleanup_cause(&mut chain),
            Some(DecisionClosureReason::ClockUncertain)
        );
        // The AI worker then tells the new run and the pause ends.
        set_pause(&mut chain, scope, 3, false);
        assert_eq!(advance(&mut chain), Step::Waiting("bot_cleanup_owned"));
        assert!(decision(&mut chain).resume.is_none());
        assert_eq!(creations(&chain), created, "nothing launched");
    }
}

/// `m8_decision_expires_under_pause`: a delegate run that was not told the
/// team value holds the reservation the same way, and the deadline still
/// ends the decision.
#[test]
fn m8_decision_expires_under_pause_while_the_delegate_run_was_not_told() {
    let mut chain = held(decision_chain());
    chain.prepared.allow_once(&mut chain.orchestrator).unwrap();
    chain.orchestrator.require_team_value(true);
    let before = decision(&mut chain);
    for _ in 0..3 {
        assert_eq!(advance(&mut chain), Step::Waiting("bot_boot_pending"));
    }
    assert_eq!(decision(&mut chain), before);
    let deadline = before.receipt.deadline.deadline_at_ms;
    chain.clock.store(deadline, Ordering::SeqCst);
    assert_eq!(advance(&mut chain), Step::Expired);
    assert_eq!(
        cleanup_cause(&mut chain),
        Some(DecisionClosureReason::Expired)
    );
}

/// `m8_decision_unpause_inside_deadline`, team scope: in the same delegate
/// run, an unpause before the deadline lets the unchanged driver reserve,
/// because no Bot's revision moved. This is what the code does, not a
/// promise (contract 3.5).
#[test]
fn m8_decision_unpause_inside_deadline_at_team_scope_the_unchanged_driver_reserves() {
    let mut chain = held(decision_chain());
    chain.prepared.allow_once(&mut chain.orchestrator).unwrap();
    set_pause(&mut chain, Scope::Team, 2, true);
    assert_eq!(advance(&mut chain), Step::Waiting("bot_team_paused"));
    chain.clock.store(FIXTURE_NOW_MS + 60_000, Ordering::SeqCst);
    set_pause(&mut chain, Scope::Team, 3, false);
    for expected in [
        Step::SuccessorReserved,
        Step::SuccessorBound,
        Step::SuccessorInstalled,
        Step::InputAcknowledged,
    ] {
        assert_eq!(advance(&mut chain), expected);
    }
    assert!(decision(&mut chain).resume.is_some());
}

/// `m8_decision_unpause_inside_deadline`, Bot scope. This is what the code
/// does, not a promise (contract 3.5), and it is not quite what the contract
/// expected. The binding rule compares the decision's bound policy with the
/// Bot's current **policy** barrier, not with its admission barrier:
///
/// - A Pause and a Resume move the admission barrier only. If nothing else
///   installed a policy barrier at the Bot's new revision, the unchanged
///   driver reserves after the unpause, exactly as at team scope.
/// - Once the policy barrier of the new revision is installed (the boot
///   pair, a lifecycle sync or a Routine preclaim does that), the binding
///   rule refuses the reservation for good and the decision ends at its
///   deadline.
#[test]
fn m8_decision_unpause_inside_deadline_at_bot_scope_follows_the_policy_barrier() {
    for policy_moved in [false, true] {
        let mut chain = held(decision_chain());
        chain.prepared.allow_once(&mut chain.orchestrator).unwrap();
        let device = chain.prepared.owner.device;
        for (epoch, paused) in [(2, true), (3, false)] {
            set_pause(&mut chain, Scope::Bot, epoch, paused);
            if policy_moved {
                let policy = crate::test_support::bot_policy(&chain.prepared.submission, epoch);
                chain
                    .orchestrator
                    .synchronize_native_policy(policy, device)
                    .unwrap();
            }
            if paused {
                assert_eq!(advance(&mut chain), Step::Waiting("bot_paused"));
            }
        }
        if !policy_moved {
            assert_eq!(advance(&mut chain), Step::SuccessorReserved);
            assert!(decision(&mut chain).resume.is_some());
            continue;
        }
        let (owner, run) = (chain.prepared.owner, chain.prepared.run);
        for _ in 0..3 {
            let mut bridge = chain.prepared.enrollment_bridge();
            let refused = chain
                .orchestrator
                .bot_advance_decision_resume(owner, run, &mut bridge)
                .unwrap_err();
            assert!(matches!(refused, RunError::Port(error) if error.code == "bot_conflict"));
        }
        assert!(decision(&mut chain).resume.is_none());
        let deadline = decision(&mut chain).receipt.deadline.deadline_at_ms;
        chain.clock.store(deadline, Ordering::SeqCst);
        assert_eq!(advance(&mut chain), Step::Expired);
    }
}
