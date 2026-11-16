// SPDX-License-Identifier: MPL-2.0

//! A Routine Run asks and waits for a page-edit decision (P4.3 section
//! 4.10), through the public Routine decision-chain builder: a claimed and
//! Linked Routine occurrence over the real in-memory ledger, the resume
//! driver and the scripted session host. Scripted runtime composition, not
//! provider qualification. No disk, Store or provider.

use super::*;
use crate::DecisionResumeStep as Step;
use crate::test_support::{
    DecisionChain, FIXTURE_NOW_MS, decision_chain, publish_owed, routine_decision_chain,
};
use koru_delegate_core::decisions::{
    DecisionAuthorization, DecisionClosureReason, DecisionPhase, DecisionResumeState,
};
use koru_delegate_core::{
    AgentTurnId, BotAdmissionState, ConfirmNativeResult, NativeResultKind, NativeResultObservation,
    NativeResultReference, NativeResultStage, SessionId,
};
use koru_delegate_sqlite::DecisionRecord;
use koru_delegate_sqlite::test_support as build;
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

/// The Routine decision with its origin stopped: a person may answer it.
fn held(successor: SessionId, turn: AgentTurnId) -> DecisionChain {
    let mut chain = routine_decision_chain();
    chain
        .prepared
        .script_resume(&mut chain.sessions.lock().unwrap(), successor, turn);
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

#[test]
fn routine_decision_edit_outside_its_grant_is_bound_to_the_routine_and_its_current_grant() {
    let mut chain = routine_decision_chain();
    let routine = chain
        .prepared
        .submission
        .provenance
        .routine
        .expect("a Routine Run");
    let grant = chain.prepared.proof.routine.clone().expect("its grant");
    let record = decision(&mut chain);
    assert_eq!(
        (
            record.receipt.state.authorization,
            record.receipt.state.phase
        ),
        (DecisionAuthorization::Pending, DecisionPhase::Holding)
    );
    let DecisionBindingV1::KoruEdit(binding) = &record.receipt.binding else {
        panic!("a Koru edit decision")
    };
    let subject = binding.routine.as_ref().expect("the Routine subject");
    assert_eq!(subject.provenance, routine);
    assert_eq!(subject.claim.ids.run, chain.prepared.run);
    assert_eq!(subject.route.schedule, routine.schedule);
    let policy = binding.routine_policy.as_ref().expect("the Routine policy");
    assert_eq!(
        (
            policy.schedule,
            policy.request,
            policy.grant_revision.get(),
            policy.digest
        ),
        (
            grant.schedule,
            grant.request,
            grant.grant_revision,
            grant.digest().unwrap()
        )
    );
    // The Routine's grant is what was captured, not a wider Bot policy.
    assert_eq!(binding.captured_policy, grant.policy);

    // An exact retry replays the same decision; nothing new is written.
    let device = chain.prepared.owner.device;
    let DecisionBindingV1::KoruEdit(bound) = record.receipt.binding.clone() else {
        unreachable!()
    };
    let replayed = chain
        .orchestrator
        .bot_prepare_edit_decision(
            &chain.prepared.fence,
            device,
            &DecisionEditRequest {
                target: bound.target,
                action: bound.action,
                operation_id: bound.operation_id,
                edit_fingerprint: bound.edit_fingerprint,
                expected_revision: bound.expected_revision.as_slice().to_vec(),
                payload: bound.payload.as_slice().to_vec(),
            },
            &chain.prepared.proof,
            chain.prepared.boot,
        )
        .unwrap();
    assert_eq!(replayed.receipt.decision, record.receipt.decision);
}

#[test]
fn routine_decision_answer_needs_the_routines_own_proof_and_a_manual_run_is_unchanged() {
    // A Routine decision answered under the Bot's ordinary proof (no Routine
    // grant) is refused: nothing is granted through the Bot's chat access.
    let mut routine = held(SessionId::mint(), AgentTurnId::mint());
    let device = routine.prepared.owner.device;
    let answer = routine
        .prepared
        .allow_once_answer(&mut routine.orchestrator)
        .unwrap();
    let interactive = FreshBotPolicyProof {
        global: routine.prepared.proof.global.clone(),
        routine: None,
    };
    assert!(
        routine
            .orchestrator
            .bot_answer_decision(&answer, device, &interactive, routine.prepared.boot)
            .is_err()
    );
    assert_eq!(
        decision(&mut routine).receipt.state.authorization,
        DecisionAuthorization::Pending
    );

    // A manual Bot Run's decision carries no Routine, and a Routine proof
    // offered for it is refused; its own proof still answers it.
    let mut manual = decision_chain();
    let device = manual.prepared.owner.device;
    let before = decision(&mut manual);
    assert!(
        matches!(&before.receipt.binding, DecisionBindingV1::KoruEdit(v) if v.routine.is_none() && v.routine_policy.is_none())
    );
    manual.prepared.script_resume(
        &mut manual.sessions.lock().unwrap(),
        SessionId::mint(),
        AgentTurnId::mint(),
    );
    for expected in [
        Step::EnrollmentRequested,
        Step::Enrolled,
        Step::OriginStopped,
    ] {
        assert_eq!(advance(&mut manual), expected);
    }
    let answer = manual
        .prepared
        .allow_once_answer(&mut manual.orchestrator)
        .unwrap();
    let mixed = FreshBotPolicyProof {
        global: manual.prepared.proof.global.clone(),
        routine: routine.prepared.proof.routine.clone(),
    };
    assert!(
        manual
            .orchestrator
            .bot_answer_decision(&answer, device, &mixed, manual.prepared.boot)
            .is_err()
    );
    let granted = manual
        .orchestrator
        .bot_answer_decision(
            &answer,
            device,
            &manual.prepared.proof,
            manual.prepared.boot,
        )
        .unwrap();
    assert_eq!(
        granted.receipt.state.authorization,
        DecisionAuthorization::Granted
    );
    assert_eq!(advance(&mut manual), Step::SuccessorReserved);
}

/// Rows 2 to 5: the resume input, the fresh proof, the successor launch and
/// the driver all serve a Routine Run, and the approved edit runs once.
#[test]
fn routine_decision_successor_keeps_routine_provenance() {
    let successor = SessionId::mint();
    let turn = AgentTurnId::mint();
    let mut chain = held(successor, turn);
    let (owner, run) = (chain.prepared.owner, chain.prepared.run);
    let device = owner.device;
    let routine = chain.prepared.submission.provenance.routine.unwrap();

    // The fresh proof the resume uses carries the current Routine policy.
    let proof = chain.orchestrator.decision_policy_proof(run).unwrap();
    assert_eq!(proof, chain.prepared.proof);
    assert!(proof.routine.is_some());

    chain.prepared.allow_once(&mut chain.orchestrator).unwrap();
    for expected in [
        Step::SuccessorReserved,
        Step::SuccessorBound,
        Step::SuccessorInstalled,
        Step::InputAcknowledged,
    ] {
        assert_eq!(advance(&mut chain), expected);
    }
    let keys = chain
        .prepared
        .successor_keys(&mut chain.orchestrator)
        .unwrap()
        .unwrap();
    assert_eq!(keys.session, Some(successor));
    assert_eq!(
        keys.generation.get(),
        chain.prepared.fence.generation.get() + 1
    );
    {
        let script = chain.sessions.lock().unwrap();
        assert_eq!(script.creations_under(keys.creation), 1);
        assert_eq!(script.inputs_with_key(keys.input), 1);
        assert_eq!(script.acknowledged_turn(keys.input), Some(turn));
    }
    // The successor is a new attempt of the same Run: the same admission,
    // the same Routine provenance, still the claimed occurrence.
    let admitted = chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_admission(run)
        .unwrap()
        .unwrap();
    assert_eq!(admitted.submission, chain.prepared.submission);
    assert_eq!(admitted.submission.provenance.routine, Some(routine));
    assert_eq!(admitted.state, BotAdmissionState::Executing);
    let subject = chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .decision_routine_subject(owner, run)
        .unwrap()
        .expect("still the Routine's Run");
    assert_eq!(subject.provenance, routine);

    // The approved edit is consumed once, under the Routine's own grant,
    // and its confirmed save settles the decision.
    let record = decision(&mut chain);
    let consumer = BotExecutionFence {
        attempt: keys.attempt,
        generation: keys.generation,
        session: successor,
        turn: Some(turn),
        ..chain.prepared.fence.clone()
    };
    let request = build::consume_request(&record, &consumer).unwrap();
    let DecisionBindingV1::KoruEdit(binding) = &record.receipt.binding else {
        panic!("a Koru edit decision")
    };
    let payload = binding.payload.as_slice().to_vec();
    // Without the Routine in the proof nothing is consumed.
    let interactive = FreshBotPolicyProof {
        global: chain.prepared.proof.global.clone(),
        routine: None,
    };
    assert!(
        chain
            .orchestrator
            .bot_consume_decision_edit(
                &consumer,
                device,
                &request,
                &payload,
                &interactive,
                chain.prepared.boot
            )
            .is_err()
    );
    let first = chain
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
    let lease = first.execute.expect("the one execution license");
    assert_eq!(
        lease.routine.as_ref().map(|grant| grant.schedule),
        Some(routine.schedule)
    );
    let replay = chain
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
    assert!(replay.execute.is_none(), "never a second license");
    let stage = chain
        .orchestrator
        .confirm_native_result(
            &ConfirmNativeResult {
                key: request.key.clone(),
                request_fingerprint: request.request_fingerprint,
                observation: NativeResultObservation::Durable {
                    evidence: NativeResultReference {
                        operation_id: request.key.operation_id,
                        workspace: request.destination,
                        document: request.target.unwrap(),
                        version: vec![7, 7, 7],
                        title: request.title.clone(),
                        confirmed_at_ms: FIXTURE_NOW_MS,
                        kind: NativeResultKind::EditedPage,
                    },
                },
            },
            device,
        )
        .unwrap()
        .stage;
    assert_eq!(stage, NativeResultStage::Verified);
    let settled = decision(&mut chain);
    assert_eq!(
        (
            settled.receipt.state.authorization,
            settled.receipt.state.phase,
            settled.receipt.closure.as_ref().map(|c| c.reason)
        ),
        (
            DecisionAuthorization::Consumed,
            DecisionPhase::Settled,
            Some(DecisionClosureReason::Completed)
        )
    );
}

#[test]
fn routine_decision_declined_or_unanswered_closes_and_nothing_is_launched() {
    // Declined: the answer owns the cleanup and no successor is reserved.
    let mut chain = held(SessionId::mint(), AgentTurnId::mint());
    let device = chain.prepared.owner.device;
    let mut deny = chain
        .prepared
        .allow_once_answer(&mut chain.orchestrator)
        .unwrap();
    deny.choice = koru_delegate_core::decisions::DecisionAnswerChoice::Deny {};
    let denied = chain
        .orchestrator
        .bot_answer_decision(&deny, device, &chain.prepared.proof, chain.prepared.boot)
        .unwrap();
    assert_eq!(
        denied.receipt.state.authorization,
        DecisionAuthorization::Denied
    );
    assert_eq!(advance(&mut chain), Step::Waiting("bot_cleanup_owned"));
    assert!(
        chain
            .prepared
            .successor_keys(&mut chain.orchestrator)
            .unwrap()
            .is_none()
    );

    // Unanswered (nobody may be there): it expires at its deadline.
    let mut chain = held(SessionId::mint(), AgentTurnId::mint());
    let deadline = decision(&mut chain).receipt.deadline.deadline_at_ms;
    chain.clock.store(deadline - 1, Ordering::SeqCst);
    assert_eq!(advance(&mut chain), Step::Waiting("bot_answer_pending"));
    chain.clock.store(deadline, Ordering::SeqCst);
    assert_eq!(advance(&mut chain), Step::Expired);
    let expired = decision(&mut chain);
    assert_eq!(
        expired.receipt.state.authorization,
        DecisionAuthorization::Expired
    );
    // A late answer is refused; the successor is never reserved.
    assert!(chain.prepared.allow_once(&mut chain.orchestrator).is_err());
    assert!(
        chain
            .prepared
            .successor_keys(&mut chain.orchestrator)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        decision(&mut chain).resume.map(|resume| resume.state),
        None | Some(DecisionResumeState::Closed { .. })
    ));
}
