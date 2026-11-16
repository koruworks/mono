// SPDX-License-Identifier: MPL-2.0

//! O157: a Koru edit decision is closed at its deadline. Driven through the
//! public decision-chain builder over the real in-memory ledger and the
//! resume driver, with the builder's shared fake clock. The whole cleanup to
//! the Run's end is driven on the real actor tick in `koru-delegate-host`
//! (`actor::tests::decision_composition`). No disk, Store or provider.

use super::*;
use crate::DecisionResumeStep as Step;
use crate::test_support::{DecisionChain, FIXTURE_NOW_MS, decision_chain, publish_owed};
use koru_delegate_core::decisions::{DecisionBindingV1, DecisionResumeState};
use koru_delegate_core::{
    AgentTurnId, ConfirmNativeResult, NativeResultKind, NativeResultObservation,
    NativeResultReference, PrepareNativeResult, SessionId,
};
use koru_delegate_sqlite::test_support as build;
use koru_delegate_sqlite::{DecisionCleanupOrigin, DecisionRecord};
use std::sync::atomic::Ordering;

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

fn projections(chain: &mut DecisionChain) -> u64 {
    let (owner, run) = (chain.prepared.owner, chain.prepared.run);
    chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .latest_decision_projection(owner, run)
        .unwrap()
        .map_or(0, |record| record.identity.sequence.get())
}

/// A decision a person may answer: the origin is stopped, every projection
/// is published, and the driver waits for the answer.
fn held() -> DecisionChain {
    let mut chain = decision_chain();
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

/// The person allowed the edit and its successor acknowledged the
/// continuation: the one-use consume is next. Returns the consume request.
fn acknowledged() -> (
    DecisionChain,
    BotExecutionFence,
    PrepareNativeResult,
    Vec<u8>,
) {
    let mut chain = held();
    chain.prepared.allow_once(&mut chain.orchestrator).unwrap();
    for expected in [
        Step::SuccessorReserved,
        Step::SuccessorBound,
        Step::SuccessorInstalled,
        Step::InputAcknowledged,
    ] {
        assert_eq!(advance(&mut chain), expected);
    }
    let record = decision(&mut chain);
    let resume = record.resume.clone().unwrap();
    let (attempt, generation, session, _) = resume.consumer_identity();
    let DecisionResumeState::InputAcknowledged { acknowledgment } = &resume.state else {
        panic!("an acknowledged continuation")
    };
    let consumer = BotExecutionFence {
        attempt,
        generation,
        session: session.unwrap(),
        turn: Some(acknowledgment.turn),
        ..chain.prepared.fence.clone()
    };
    let request = build::consume_request(&record, &consumer).unwrap();
    let DecisionBindingV1::KoruEdit(binding) = &record.receipt.binding else {
        panic!("a Koru edit decision")
    };
    let payload = binding.payload.as_slice().to_vec();
    (chain, consumer, request, payload)
}

fn deadline_of(chain: &mut DecisionChain) -> i64 {
    decision(chain).receipt.deadline.deadline_at_ms
}

/// A runtime clock that reads ahead of the ledger's.
struct Ahead(i64);
impl crate::Clock for Ahead {
    fn now_ms(&mut self) -> i64 {
        self.0
    }
}

#[test]
fn decision_deadline_cause_follows_the_contracts_clock_rules() {
    let boot = DelegateBootId::mint();
    let deadline = DecisionDeadline {
        created_boot: boot,
        created_at_ms: 1_000,
        deadline_at_ms: 601_000,
        native_deadline_at_ms: None,
    };
    // Not yet, on both clocks.
    assert_eq!(deadline_cause(&deadline, boot, 600_999, false), None);
    // The wall clock reached it (sleep included: wall time kept advancing).
    assert_eq!(
        deadline_cause(&deadline, boot, 601_000, false),
        Some(DecisionClosureReason::Expired)
    );
    assert_eq!(
        deadline_cause(&deadline, boot, 601_000, true),
        Some(DecisionClosureReason::Expired)
    );
    // The whole wait elapsed on the monotonic clock while the wall clock
    // says otherwise (it was set back): the earlier expiry, as uncertain.
    assert_eq!(
        deadline_cause(&deadline, boot, 500, true),
        Some(DecisionClosureReason::ClockUncertain)
    );
    // Another boot created it: elapsed time is unproven even when the saved
    // deadline looks future, and also when it looks past.
    let other = DelegateBootId::mint();
    for now in [1_001, 601_000] {
        assert_eq!(
            deadline_cause(&deadline, other, now, false),
            Some(DecisionClosureReason::ClockUncertain)
        );
    }
}

#[test]
fn decision_expiry_unanswered_is_requested_once_at_the_deadline_and_not_before() {
    let mut chain = held();
    let deadline = deadline_of(&mut chain);
    assert_eq!(
        deadline,
        FIXTURE_NOW_MS + koru_delegate_core::decisions::DECISION_WAIT_MILLIS
    );
    chain.clock.store(deadline - 1, Ordering::SeqCst);
    assert_eq!(advance(&mut chain), Step::Waiting("bot_answer_pending"));
    assert_eq!(cleanup_cause(&mut chain), None);
    let before = projections(&mut chain);

    chain.clock.store(deadline, Ordering::SeqCst);
    assert_eq!(advance(&mut chain), Step::Expired);
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
    assert_eq!(projections(&mut chain), before + 1, "one cleanup request");
    // The cleanup owner has the Run now: its retry is armed, and the driver
    // asks for nothing more however often it is offered the Run.
    let run = chain.prepared.run;
    assert!(chain.orchestrator.runs[&run].decision_cleanup.is_some());
    for _ in 0..3 {
        assert_eq!(advance(&mut chain), Step::Waiting("bot_cleanup_owned"));
    }
    assert_eq!(projections(&mut chain), before + 1);
    // An answer after the expiry is refused and reopens nothing.
    assert!(chain.prepared.allow_once(&mut chain.orchestrator).is_err());
    assert_eq!(decision(&mut chain).receipt, expired.receipt);
}

#[test]
fn decision_expiry_refused_request_is_paced_and_an_answer_before_the_deadline_wins() {
    let mut chain = held();
    let run = chain.prepared.run;
    let deadline = deadline_of(&mut chain);
    // The ledger's clock is one millisecond short of the deadline while the
    // runtime's reads it as reached: the ledger refuses `Expired`.
    chain.clock.store(deadline - 1, Ordering::SeqCst);
    chain.orchestrator.ports_mut().clock = Box::new(Ahead(deadline));
    let before = projections(&mut chain);
    assert_eq!(
        advance(&mut chain),
        Step::Waiting("bot_decision_expiry_refused")
    );
    // Nothing stays retained for a later Stop to be made to retry.
    assert!(chain.orchestrator.runs[&run].decision_cleanup.is_none());
    // Woken again at once (a worker wake): not asked again before the tick.
    for _ in 0..3 {
        assert_eq!(
            advance(&mut chain),
            Step::Waiting("bot_decision_expiry_paced")
        );
    }
    assert_eq!(projections(&mut chain), before, "no request was written");
    let pending = decision(&mut chain);
    assert_eq!(
        (
            pending.receipt.state.authorization,
            pending.receipt.state.phase
        ),
        (DecisionAuthorization::Pending, DecisionPhase::Held)
    );

    // The person answers just before the deadline: accepted.
    let stale = chain
        .prepared
        .wait(&mut chain.orchestrator)
        .unwrap()
        .unwrap()
        .expected;
    let granted = chain.prepared.allow_once(&mut chain.orchestrator).unwrap();
    assert_eq!(
        granted.receipt.state.authorization,
        DecisionAuthorization::Granted
    );
    // An expiry that lost the race carries the pre-answer revisions: the
    // ledger refuses it even at the deadline, and the answer stands.
    chain.clock.store(deadline, Ordering::SeqCst);
    assert!(
        chain
            .orchestrator
            .bot_ledger()
            .unwrap()
            .request_decision_cleanup(
                &stale,
                DecisionTransitionId::mint(),
                DecisionClosureReason::Expired
            )
            .is_err()
    );
    assert_eq!(decision(&mut chain).receipt, granted.receipt);
    assert_eq!(cleanup_cause(&mut chain), None);
}

#[test]
fn decision_expiry_granted_but_unconsumed_closes_at_the_deadline() {
    // The ledger refuses a consume after the deadline, so an allowed edit
    // that was not consumed by then is closed rather than left waiting.
    let (mut chain, consumer, request, payload) = acknowledged();
    let device = chain.prepared.owner.device;
    let deadline = deadline_of(&mut chain);
    chain.clock.store(deadline, Ordering::SeqCst);
    assert!(
        chain
            .orchestrator
            .bot_consume_decision_edit(
                &consumer,
                device,
                &request,
                &payload,
                &chain.prepared.proof,
                chain.prepared.boot,
            )
            .is_err(),
        "the ledger's own clock refuses the late consume"
    );
    assert_eq!(advance(&mut chain), Step::Expired);
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
    // The successor's input is closed; its cleanup targets the successor.
    assert!(matches!(
        expired.resume.as_ref().map(|resume| &resume.state),
        Some(DecisionResumeState::Closed { .. })
    ));
    assert_eq!(advance(&mut chain), Step::Waiting("bot_cleanup_owned"));
}

#[test]
fn decision_expiry_never_touches_a_consumed_or_settled_decision() {
    let (mut chain, consumer, request, payload) = acknowledged();
    let device = chain.prepared.owner.device;
    let deadline = deadline_of(&mut chain);
    // Consumed one millisecond before the deadline: the save owns it now.
    chain.clock.store(deadline - 1, Ordering::SeqCst);
    let outcome = chain
        .orchestrator
        .bot_consume_decision_edit(
            &consumer,
            device,
            &request,
            &payload,
            &chain.prepared.proof,
            chain.prepared.boot,
        )
        .unwrap();
    assert!(outcome.execute.is_some());
    chain.clock.store(deadline + 60_000, Ordering::SeqCst);
    assert_eq!(advance(&mut chain), Step::Waiting("bot_consume_owned"));
    let consumed = decision(&mut chain);
    assert_eq!(
        (
            consumed.receipt.state.authorization,
            consumed.receipt.state.phase
        ),
        (DecisionAuthorization::Consumed, DecisionPhase::Effect)
    );
    assert_eq!(cleanup_cause(&mut chain), None);

    // The save is confirmed after the deadline: the decision settles as
    // Completed, and no later turn reopens or expires it (O150).
    chain
        .orchestrator
        .confirm_native_result(
            &ConfirmNativeResult {
                key: request.key.clone(),
                request_fingerprint: request.request_fingerprint,
                observation: NativeResultObservation::Durable {
                    evidence: NativeResultReference {
                        version_id: None,
                        operation_id: request.key.operation_id,
                        workspace: request.destination,
                        document: request.target.unwrap(),
                        version: vec![7, 7, 7],
                        title: request.title.clone(),
                        confirmed_at_ms: deadline + 60_000,
                        kind: NativeResultKind::EditedPage,
                    },
                },
            },
            device,
        )
        .unwrap();
    let settled = decision(&mut chain);
    assert_eq!(settled.receipt.state.phase, DecisionPhase::Settled);
    assert_eq!(
        settled.receipt.closure.as_ref().map(|c| c.reason),
        Some(DecisionClosureReason::Completed)
    );
    chain.clock.store(deadline + 3_600_000, Ordering::SeqCst);
    for _ in 0..3 {
        let step = advance(&mut chain);
        assert!(
            matches!(step, Step::Waiting(_) | Step::Idle),
            "a settled decision is never stepped by its deadline: {step:?}"
        );
    }
    assert_eq!(decision(&mut chain).receipt, settled.receipt);
    assert_eq!(cleanup_cause(&mut chain), None);
}

#[test]
fn decision_expiry_after_a_delegate_restart_is_clock_uncertain_at_once() {
    let mut chain = held();
    let (owner, run) = (chain.prepared.owner, chain.prepared.run);
    let created = decision(&mut chain).receipt.deadline;
    // The delegate restarts over the same ledger and ports: a new boot and
    // no memory. The saved deadline is still nine minutes away.
    chain.clock.store(FIXTURE_NOW_MS + 60_000, Ordering::SeqCst);
    let DecisionChain {
        orchestrator,
        sessions,
        clock,
        documents,
        waits,
        prepared,
    } = chain;
    let mut chain = DecisionChain {
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
    };
    assert_ne!(chain.orchestrator.delegate_boot(), created.created_boot);
    // The durable open-wait index still offers the Run to the driver.
    assert_eq!(
        chain.orchestrator.bot_decision_resume_runs(None).unwrap(),
        vec![(owner, run)]
    );
    // No answer is accepted for a decision another boot created ...
    assert!(chain.prepared.allow_once(&mut chain.orchestrator).is_err());
    // ... so its first turn closes it, as clock-uncertain.
    assert_eq!(advance(&mut chain), Step::Expired);
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
    assert!(chain.orchestrator.runs[&run].decision_cleanup.is_some());
    assert_eq!(advance(&mut chain), Step::Waiting("bot_cleanup_owned"));
}

/// M5: the deadline covers a private-operation decision exactly as it covers
/// an edit, and still leaves a native prompt to its own fallback.
#[test]
fn m5_decision_expiry_applies_to_a_private_operation_as_to_an_edit() {
    use koru_delegate_core::decisions::{DecisionKind, DecisionRevision, DecisionState};
    let authorizations = [
        DecisionAuthorization::Pending,
        DecisionAuthorization::Granted,
        DecisionAuthorization::Consumed,
        DecisionAuthorization::Denied,
        DecisionAuthorization::Canceled,
        DecisionAuthorization::Expired,
    ];
    let phases = [
        DecisionPhase::Publishing,
        DecisionPhase::Holding,
        DecisionPhase::Held,
        DecisionPhase::ResumePrepared,
        DecisionPhase::AwaitRetry,
        DecisionPhase::Effect,
        DecisionPhase::StopPending,
        DecisionPhase::Settled,
    ];
    let mut covered = 0;
    for authorization in authorizations {
        for phase in phases {
            let state = |kind| DecisionState {
                kind,
                authorization,
                phase,
                revision: DecisionRevision::new(1).unwrap(),
            };
            let applies =
                |kind, wait, cleanup| super::deadline_applies_to(Some(&state(kind)), wait, cleanup);
            let private = applies(DecisionKind::PrivateOperation, BotWaitPhase::Held, false);
            assert_eq!(
                private,
                applies(DecisionKind::KoruEdit, BotWaitPhase::Held, false),
                "{authorization:?}/{phase:?}"
            );
            assert_eq!(
                private,
                matches!(
                    authorization,
                    DecisionAuthorization::Pending | DecisionAuthorization::Granted
                ) && !matches!(phase, DecisionPhase::Settled | DecisionPhase::StopPending)
            );
            covered += usize::from(private);
            // A native prompt is never closed by this deadline.
            assert!(!applies(
                DecisionKind::NativeApproval,
                BotWaitPhase::Fenced,
                false
            ));
            // An owned or recorded cleanup is left to its owner.
            assert!(!applies(
                DecisionKind::PrivateOperation,
                BotWaitPhase::CleanupPending,
                false
            ));
            assert!(!applies(
                DecisionKind::PrivateOperation,
                BotWaitPhase::Held,
                true
            ));
        }
    }
    assert_eq!(covered, 12);
    assert!(!super::deadline_applies_to(None, BotWaitPhase::Held, false));
}
