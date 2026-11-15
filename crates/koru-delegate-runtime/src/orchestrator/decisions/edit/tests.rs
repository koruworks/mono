// SPDX-License-Identifier: MPL-2.0

//! Exact retained replay of a Koru edit decision before fresh checks, over
//! the real in-memory ledger through the public decision-chain builder.
//! Scripted runtime composition, not provider qualification. No disk, Store
//! or provider.

use super::*;
use crate::DecisionResumeStep as Step;
use crate::test_support::{DecisionChain, PreparedDecision, decision_chain};
use koru_delegate_core::decisions::{DecisionAuthorization, DecisionPhase};
use koru_delegate_core::{AgentTurnId, SessionId};

/// The exact edit the prepared decision protects, read back from its binding.
fn retained_edit(
    orchestrator: &mut Orchestrator,
    prepared: &PreparedDecision,
) -> DecisionEditRequest {
    let record = orchestrator
        .bot_ledger()
        .unwrap()
        .bot_decision(prepared.owner, prepared.run, prepared.decision)
        .unwrap()
        .unwrap();
    let DecisionBindingV1::KoruEdit(binding) = &record.receipt.binding else {
        panic!("a Koru edit decision")
    };
    DecisionEditRequest {
        target: binding.target,
        action: binding.action.clone(),
        operation_id: binding.operation_id,
        edit_fingerprint: binding.edit_fingerprint,
        expected_revision: binding.expected_revision.as_slice().to_vec(),
        payload: binding.payload.as_slice().to_vec(),
    }
}

fn code(result: Result<DecisionRecord, RunError>) -> String {
    match result {
        Err(RunError::Port(error)) => error.code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn edit_decision_retry_replays_the_retained_receipt_before_publication_boot_and_fresh_policy() {
    let DecisionChain {
        mut orchestrator,
        prepared,
        ..
    } = decision_chain();
    let edit = retained_edit(&mut orchestrator, &prepared);
    let device = prepared.owner.device;
    // The decision owns the Run's publication: a fresh preparation would be
    // refused `bot_decision_owned` before the ledger.
    assert!(orchestrator.run_publication_pending(prepared.run));
    let before = orchestrator
        .bot_ledger()
        .unwrap()
        .bot_decision(prepared.owner, prepared.run, prepared.decision)
        .unwrap()
        .unwrap();
    // A stale boot and a proof whose policy no longer matches: replay needs
    // neither, because it records nothing and grants nothing.
    let mut stale = prepared.proof.clone();
    stale.global.policy_revision += 7;
    let replayed = orchestrator
        .bot_prepare_edit_decision(
            &prepared.fence,
            device,
            &edit,
            &stale,
            DelegateBootId::mint(),
        )
        .unwrap();
    assert_eq!(replayed, before, "the exact retained record, unchanged");
    assert_eq!(
        replayed.receipt.state.authorization,
        DecisionAuthorization::Pending
    );
    assert_eq!(replayed.receipt.state.phase, DecisionPhase::Holding);
    // Still the one open wait for the Run; no second question was minted.
    assert_eq!(
        orchestrator.bot_decision_resume_runs(None).unwrap(),
        vec![(prepared.owner, prepared.run)]
    );
}

#[test]
fn edit_decision_retry_with_a_changed_body_or_revision_is_a_conflict_not_a_second_question() {
    let DecisionChain {
        mut orchestrator,
        prepared,
        ..
    } = decision_chain();
    let edit = retained_edit(&mut orchestrator, &prepared);
    let device = prepared.owner.device;
    let mut body = edit.clone();
    body.payload = b"ANOTHER-EDIT".to_vec();
    let mut revision = edit.clone();
    revision.expected_revision = vec![9, 9, 9];
    let mut fingerprint = edit.clone();
    fingerprint.edit_fingerprint = [4; 32];
    for changed in [body, revision, fingerprint] {
        assert_eq!(
            code(orchestrator.bot_prepare_edit_decision(
                &prepared.fence,
                device,
                &changed,
                &prepared.proof,
                prepared.boot,
            )),
            "bot_conflict"
        );
    }
    // A different operation while this decision waits is not a new question.
    let mut other = edit;
    other.operation_id = uuid::Uuid::new_v4();
    assert_eq!(
        code(orchestrator.bot_prepare_edit_decision(
            &prepared.fence,
            device,
            &other,
            &prepared.proof,
            prepared.boot,
        )),
        "bot_decision_owned"
    );
    let open = prepared.wait(&mut orchestrator).unwrap().unwrap();
    assert_eq!(
        open.expected.decision.map(|d| d.decision),
        Some(prepared.decision)
    );
}

#[test]
fn edit_decision_retained_read_refuses_a_fence_that_is_neither_origin_nor_consumer() {
    let DecisionChain {
        mut orchestrator,
        prepared,
        ..
    } = decision_chain();
    let edit = retained_edit(&mut orchestrator, &prepared);
    let device = prepared.owner.device;
    let mut later_turn = prepared.fence.clone();
    later_turn.turn = Some(AgentTurnId::mint());
    let mut other_session = prepared.fence.clone();
    other_session.session = SessionId::mint();
    for fence in [later_turn, other_session] {
        assert_eq!(
            code(orchestrator.bot_prepare_edit_decision(
                &fence,
                device,
                &edit,
                &prepared.proof,
                prepared.boot,
            )),
            "bot_fenced"
        );
    }
    // Another device never reads it.
    let mut foreign = prepared.fence.clone();
    foreign.owner.device = DeviceId::new();
    assert!(
        orchestrator
            .bot_prepare_edit_decision(&foreign, device, &edit, &prepared.proof, prepared.boot)
            .is_err()
    );
}

#[test]
fn the_acknowledged_consumer_reads_its_decision_then_consumes_it_exactly_once() {
    let DecisionChain {
        mut orchestrator,
        sessions,
        prepared,
        ..
    } = decision_chain();
    let edit = retained_edit(&mut orchestrator, &prepared);
    let (owner, run, device) = (prepared.owner, prepared.run, prepared.owner.device);
    let successor = SessionId::mint();
    let turn = AgentTurnId::mint();
    prepared.script_resume(&mut sessions.lock().unwrap(), successor, turn);
    let mut bridge = prepared.enrollment_bridge();
    // Bounded: each step is asserted, so a stall fails at a named step.
    for expected in [
        Step::EnrollmentRequested,
        Step::Enrolled,
        Step::OriginStopped,
        Step::Waiting("bot_answer_pending"),
    ] {
        assert_eq!(
            orchestrator
                .bot_advance_decision_resume(owner, run, &mut bridge)
                .unwrap(),
            expected
        );
    }
    prepared.allow_once(&mut orchestrator).unwrap();
    for expected in [
        Step::SuccessorReserved,
        Step::SuccessorBound,
        Step::SuccessorInstalled,
        Step::InputAcknowledged,
    ] {
        assert_eq!(
            orchestrator
                .bot_advance_decision_resume(owner, run, &mut bridge)
                .unwrap(),
            expected
        );
    }
    let keys = prepared.successor_keys(&mut orchestrator).unwrap().unwrap();
    let consumer = BotExecutionFence {
        owner,
        bot: prepared.fence.bot,
        run,
        attempt: keys.attempt,
        generation: keys.generation,
        session: successor,
        turn: Some(turn),
    };
    // The consumer's retry of the same edit reads the retained decision,
    // with the acknowledged resume that names it.
    let record = orchestrator
        .bot_prepare_edit_decision(&consumer, device, &edit, &prepared.proof, prepared.boot)
        .unwrap();
    assert_eq!(record.receipt.decision, prepared.decision);
    assert_eq!(
        record.receipt.state.authorization,
        DecisionAuthorization::Granted
    );
    assert_eq!(record.receipt.state.phase, DecisionPhase::AwaitRetry);
    assert!(matches!(
        record.resume.as_ref().map(|r| &r.state),
        Some(DecisionResumeState::InputAcknowledged { acknowledgment }) if acknowledgment.turn == turn
    ));
    // The one-use consume licenses one execution; its retry never another.
    let request = koru_delegate_sqlite::test_support::consume_request(&record, &consumer).unwrap();
    let first = orchestrator
        .bot_consume_decision_edit(
            &consumer,
            device,
            &request,
            &edit.payload,
            &prepared.proof,
            prepared.boot,
        )
        .unwrap();
    assert!(first.execute.is_some(), "the first consume licenses once");
    let again = orchestrator
        .bot_consume_decision_edit(
            &consumer,
            device,
            &request,
            &edit.payload,
            &prepared.proof,
            prepared.boot,
        )
        .unwrap();
    assert!(again.execute.is_none(), "a replay never licenses again");
    assert_eq!(again.intent.key, first.intent.key);
    // After consumption the same retry still replays history only.
    let consumed = orchestrator
        .bot_prepare_edit_decision(&consumer, device, &edit, &prepared.proof, prepared.boot)
        .unwrap();
    assert_eq!(
        consumed.receipt.state.authorization,
        DecisionAuthorization::Consumed
    );
}
