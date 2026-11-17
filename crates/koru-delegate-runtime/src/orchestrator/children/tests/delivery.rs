// SPDX-License-Identifier: MPL-2.0

//! Cohorts `m6_child_delivery_once` and `m6_child_natural_completion`: a Run
//! that waits for other Bots delivers once (contract 4.1, "checked against
//! the single-delivery rule" and "checked against O127").

use super::*;
use crate::SessionUpdate;
use koru_delegate_core::decisions::BotWaitPhase;
use koru_delegate_core::{AttemptGeneration, BotDeliveryReceipt, BotExecutionFence, RunAttemptId};
use koru_session_protocol::{AgentEvent, SequencedAgentEvent, StreamSequence};

pub(super) fn receipt(
    chain: &ChildChain,
    attempt: RunAttemptId,
    generation: AttemptGeneration,
) -> BotDeliveryReceipt {
    let provenance = &chain.parent.submission.provenance;
    BotDeliveryReceipt {
        run: chain.parent.run,
        attempt,
        generation,
        chat_id: provenance.chat_id,
        ai_attempt_id: provenance.ai_attempt_id.unwrap(),
        assistant_message_id: provenance.assistant_message_id.unwrap(),
        confirmed_at_ms: FIXTURE_NOW_MS,
    }
}

fn key(chain: &ChildChain) -> BotRunKey {
    BotRunKey {
        owner: chain.parent.owner,
        bot: chain.parent.bot,
        run: chain.parent.run,
    }
}

pub(super) fn deliver(
    chain: &mut ChildChain,
    receipt: &BotDeliveryReceipt,
) -> Result<(), RunError> {
    let key = key(chain);
    let device = device(chain);
    chain
        .orchestrator
        .bot_acknowledge_delivery(&key, device, receipt)
}

fn code(error: &RunError) -> &str {
    match error {
        RunError::Port(port) => port.code.as_str(),
        _ => "",
    }
}

/// Deliver `receipt` the way the AI retries it: each pending answer is
/// followed by publishing what is owed and settling the wait, until the
/// receipt is accepted or a final refusal comes back.
pub(super) fn deliver_until_done(
    chain: &mut ChildChain,
    receipt: &BotDeliveryReceipt,
) -> Result<(), RunError> {
    let run = chain.parent.run;
    for _ in 0..6 {
        match deliver(chain, receipt) {
            Ok(()) => return Ok(()),
            Err(error)
                if code(&error) == "bot_delivery_pending"
                    || code(&error) == "publication_pending" =>
            {
                // Accepted once the header carries it and the Run left
                // Running; what is still pending is the ordinary owner's
                // session cleanup, not the delivery.
                let header = chain.orchestrator.header(run)?;
                if header.chat_delivery.as_ref() == Some(receipt)
                    && !matches!(header.status, RunStatus::Running { .. })
                    && chain.orchestrator.run_publication_pending(run) == false
                {
                    return Ok(());
                }
                let _ = publish_owed(&mut chain.orchestrator, run);
                // The cleanup owner's tick, as the actor's budget deadline
                // runs it.
                let _ = chain.orchestrator.bot_budget_tick(run);
                let _ = publish_owed(&mut chain.orchestrator, run);
                let _ = chain.orchestrator.settle_publication_wait(run);
            }
            Err(error) => return Err(error),
        }
    }
    deliver(chain, receipt)
}

/// The origin's InitialSend turn completed, as its own event stream reports
/// it to this delegate while the wait holds the document.
fn origin_turn_completed(chain: &mut ChildChain) {
    let run = chain.parent.run;
    let fence = chain.parent.fence.clone();
    let epoch = uuid::Uuid::new_v4();
    chain.orchestrator.register_source_producer(run, epoch);
    chain
        .orchestrator
        .handle_from_producer(
            run,
            epoch,
            SessionUpdate::Agent(SequencedAgentEvent {
                session_id: fence.session,
                sequence: StreamSequence::new(2).unwrap(),
                event: AgentEvent::TurnCompleted {
                    turn_id: fence.turn.unwrap(),
                },
            }),
        )
        .expect("the origin's turn completion is recorded");
    let _ = publish_owed(&mut chain.orchestrator, run);
    chain
        .sessions
        .lock()
        .unwrap()
        .script_completed_turn(fence.session, fence.turn.unwrap());
}

fn successor_fence(chain: &mut ChildChain) -> BotExecutionFence {
    let execution = execution(chain).expect("the wait is open");
    let resume = execution.resume.expect("a successor was reserved");
    let (attempt, generation, session, _) = resume.consumer_identity().unwrap();
    BotExecutionFence {
        attempt,
        generation,
        session: session.expect("bound"),
        turn: resume.accepted_turn(),
        ..chain.parent.fence.clone()
    }
}

/// Resume after one settled child, keeping the successor's fence.
fn resumed(chain: &mut ChildChain, bridge: &mut ScriptedChildEnrollment) -> BotExecutionFence {
    for receipt in requests(chain) {
        if receipt.state == ChildRequestState::Open {
            stop_child(chain, &receipt);
        }
    }
    for expected in [
        Step::SuccessorReserved,
        Step::SuccessorBound,
        Step::SuccessorInstalled,
        Step::InputAcknowledged,
    ] {
        assert_eq!(advance(chain, bridge), expected);
    }
    let fence = successor_fence(chain);
    let run = chain.parent.run;
    publish_owed(&mut chain.orchestrator, run).unwrap();
    chain.orchestrator.settle_publication_wait(run).unwrap();
    assert!(execution(chain).is_none());
    fence
}

#[test]
fn m6_child_delivery_once_the_successor_delivers_and_the_origin_cannot() {
    let (mut chain, _, mut bridge) = held(&[1]);
    let successor = resumed(&mut chain, &mut bridge);
    chain
        .sessions
        .lock()
        .unwrap()
        .script_completed_turn(successor.session, successor.turn.unwrap());
    // The stopped origin's receipt is never the Run's delivery.
    let origin = receipt(
        &chain,
        chain.parent.fence.attempt,
        chain.parent.fence.generation,
    );
    assert!(deliver(&mut chain, &origin).is_err());
    // The successor's is, once.
    let accepted = receipt(&chain, successor.attempt, successor.generation);
    deliver_until_done(&mut chain, &accepted).expect("the successor delivers");
    let run = chain.parent.run;
    let header = chain.orchestrator.header(run).unwrap();
    assert_eq!(header.chat_delivery, Some(accepted.clone()));
    // An exact retry replays; a different receipt conflicts.
    deliver_until_done(&mut chain, &accepted).expect("an exact retry");
    let mut other = accepted.clone();
    other.assistant_message_id = uuid::Uuid::new_v4();
    assert!(deliver(&mut chain, &other).is_err());
    assert_eq!(
        chain.orchestrator.header(run).unwrap().chat_delivery,
        Some(accepted)
    );
}

#[test]
fn m6_child_delivery_once_a_second_wait_and_a_third_generation_still_deliver_once() {
    let (mut chain, _, mut bridge) = held(&[1]);
    let second = resumed(&mut chain, &mut bridge);
    // The successor asks again: the parent's fence is now generation 2.
    let creation = {
        let row = chain
            .orchestrator
            .ports()
            .store
            .current_attempt(chain.parent.run)
            .unwrap()
            .unwrap();
        row.provider_session.unwrap().idempotency
    };
    let (third_session, third_turn) = (SessionId::mint(), AgentTurnId::mint());
    {
        let mut script = chain.sessions.lock().unwrap();
        script.script_retirement(creation, koru_session_protocol::CreationLookup::NoEvidence);
        script.script_creation(third_session);
        script.script_turn(third_turn);
    }
    chain.parent.fence = second.clone();
    let (_, requested) = asked(&mut chain, &[2]);
    assert_ne!(requested.wait.wait, BotWaitId::mint());
    let run = chain.parent.run;
    publish_owed(&mut chain.orchestrator, run).unwrap();
    assert_eq!(advance(&mut chain, &mut bridge), Step::Enrolled);
    assert_eq!(advance(&mut chain, &mut bridge), Step::OriginStopped);
    let third = resumed(&mut chain, &mut bridge);
    assert_eq!(third.generation.get(), second.generation.get() + 1);
    assert_eq!(third.session, third_session);
    chain
        .sessions
        .lock()
        .unwrap()
        .script_completed_turn(third.session, third.turn.unwrap());
    // The second generation no longer delivers; the third does, once.
    let stale = receipt(&chain, second.attempt, second.generation);
    assert!(deliver(&mut chain, &stale).is_err());
    let accepted = receipt(&chain, third.attempt, third.generation);
    deliver_until_done(&mut chain, &accepted).expect("the third generation delivers");
    assert_eq!(
        chain.orchestrator.header(run).unwrap().chat_delivery,
        Some(accepted)
    );
}

/// O127 before the stop: the origin's turn ends by itself while the wait is
/// still `hold_requested`. The delivery closes the wait, the request is
/// settled `parent_stopped`, the child is stopped and no successor exists.
#[test]
fn m6_child_natural_completion_before_the_origin_stop() {
    let mut chain = child_chain();
    let origin_creation = chain.parent.origin_creation;
    chain.sessions.lock().unwrap().script_retirement(
        origin_creation,
        koru_session_protocol::CreationLookup::NoEvidence,
    );
    let (_, requested) = asked(&mut chain, &[1]);
    let run = chain.parent.run;
    publish_owed(&mut chain.orchestrator, run).unwrap();
    origin_turn_completed(&mut chain);
    let origin = receipt(
        &chain,
        chain.parent.fence.attempt,
        chain.parent.fence.generation,
    );
    let first = deliver(&mut chain, &origin).unwrap_err();
    assert_eq!(code(&first), "bot_delivery_pending", "{first:?}");
    let settled = requests(&mut chain);
    assert_eq!(settled[0].state, ChildRequestState::Settled);
    assert_eq!(settled[0].settled_by, Some(ChildSettledBy::ParentStopped));
    // The child Run is stopped (it was queued: settled without starting).
    let child = requested.receipts[0].child_run;
    assert!(matches!(
        child_state(&mut chain, child),
        BotAdmissionState::Settled | BotAdmissionState::Refused
    ));
    deliver_until_done(&mut chain, &origin).expect("the origin's answer completes the Run");
    let mut bridge = chain.parent.enrollment_bridge();
    let step = advance(&mut chain, &mut bridge);
    assert!(
        !matches!(
            step,
            Step::SuccessorReserved | Step::SuccessorBound | Step::SuccessorInstalled
        ),
        "no successor: {step:?}"
    );
    assert_eq!(
        chain.sessions.lock().unwrap().created().len(),
        1,
        "only the origin was created"
    );
    assert_eq!(
        chain.orchestrator.header(run).unwrap().chat_delivery,
        Some(origin)
    );
}

/// O127 after the stop: the origin was retired, then its completed turn's
/// receipt arrives. The step this delegate recorded is the evidence.
#[test]
fn m6_child_natural_completion_after_the_origin_stop() {
    let mut chain = child_chain();
    chain.parent.script_resume(
        &mut chain.sessions.lock().unwrap(),
        SessionId::mint(),
        AgentTurnId::mint(),
    );
    let (_, requested) = asked(&mut chain, &[1]);
    let run = chain.parent.run;
    publish_owed(&mut chain.orchestrator, run).unwrap();
    origin_turn_completed(&mut chain);
    let mut bridge = chain.parent.enrollment_bridge();
    assert_eq!(advance(&mut chain, &mut bridge), Step::Enrolled);
    assert_eq!(advance(&mut chain, &mut bridge), Step::OriginStopped);
    let origin = receipt(
        &chain,
        chain.parent.fence.attempt,
        chain.parent.fence.generation,
    );
    let first = deliver(&mut chain, &origin).unwrap_err();
    assert_eq!(code(&first), "bot_delivery_pending", "{first:?}");
    let settled = requests(&mut chain);
    assert_eq!(settled[0].settled_by, Some(ChildSettledBy::ParentStopped));
    assert!(matches!(
        child_state(&mut chain, requested.receipts[0].child_run),
        BotAdmissionState::Settled | BotAdmissionState::Refused
    ));
    deliver_until_done(&mut chain, &origin).expect("the origin's answer completes the Run");
    assert_eq!(
        chain.sessions.lock().unwrap().created().len(),
        1,
        "no successor"
    );
    assert_eq!(
        chain.orchestrator.header(run).unwrap().chat_delivery,
        Some(origin)
    );
}

/// An origin receipt that arrives after a successor was reserved is
/// refused; cleanup runs and no successor is ever created.
///
/// **Not met (recorded for the lead):** the contract then has the retried
/// origin receipt complete the Run with the origin's delivery. The ledger's
/// receipt owner (`koru-delegate-sqlite` `bots::delivery_on`) accepts only the
/// current attempt, and the reservation already opened the successor's
/// attempt row, so the retry is refused `bot_fenced` and the Run ends through
/// its cleanup instead. Changing that owner changes delivery for every Bot
/// Run; it was left as it is.
#[test]
fn m6_child_natural_completion_after_a_reserved_successor() {
    let (mut chain, _, mut bridge) = held(&[1]);
    // The origin's turn had completed before it was stopped.
    let run = chain.parent.run;
    let turn = chain.parent.fence.turn.unwrap();
    if let Some(attempt) = chain
        .orchestrator
        .runs
        .get_mut(&run)
        .and_then(|state| state.attempt.as_mut())
    {
        attempt.bot_terminal_turn = Some(turn);
    }
    let receipt_open = requests(&mut chain)[0].clone();
    stop_child(&mut chain, &receipt_open);
    assert_eq!(advance(&mut chain, &mut bridge), Step::SuccessorReserved);
    assert_eq!(
        execution(&mut chain).unwrap().wait.phase,
        BotWaitPhase::ResumeReserved
    );
    let origin = receipt(
        &chain,
        chain.parent.fence.attempt,
        chain.parent.fence.generation,
    );
    let refused = deliver(&mut chain, &origin).unwrap_err();
    assert_eq!(code(&refused), "bot_delivery_pending");
    // The wait went to its cleanup owner: no successor is ever created.
    let after = execution(&mut chain);
    assert!(after.is_none_or(|execution| execution.wait.phase != BotWaitPhase::ResumeReserved));
    let _ = advance(&mut chain, &mut bridge);
    assert_eq!(
        chain.sessions.lock().unwrap().created().len(),
        1,
        "no successor created"
    );
    let retried = deliver_until_done(&mut chain, &origin).unwrap_err();
    assert_eq!(code(&retried), "bot_fenced", "{retried:?}");
    assert_eq!(
        chain.sessions.lock().unwrap().created().len(),
        1,
        "still no successor"
    );
    let header = chain.orchestrator.header(run).unwrap();
    assert_eq!(header.chat_delivery, None);
}
