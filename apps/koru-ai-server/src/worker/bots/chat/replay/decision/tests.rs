// SPDX-License-Identifier: MPL-2.0
//! Pure interception and held-wait verdicts over scripted delegate reads.
//! Scripted composition, not provider, actor or transport qualification.
use super::*;
use crate::worker::bots::decision_wait::fixture;
use koru_delegate_core::decisions::{DecisionAuthorization, DecisionResumeState};

fn read(observed: &BotDecisionWaitObservation) -> WaitRead {
    Ok(Some(observed.clone()))
}

#[test]
fn origin_terminal_before_and_after_the_internal_stop_receipt() {
    let fence = fixture::fence();
    let holding = fixture::holding(&fence);
    let key = holding.key.clone();
    // Before the delegate confirmed the internal stop: keep it staged.
    assert_eq!(
        classify_origin_terminal(&key, &fence, false, false, &read(&holding)),
        Interception::Uncertain
    );
    // After: only the exact matching receipt suppresses the terminal.
    let stopped = fixture::origin_stopped(&holding);
    assert_eq!(
        classify_origin_terminal(&key, &fence, false, false, &read(&stopped)),
        Interception::Suppress
    );
}

#[test]
fn missing_versus_unavailable_proof_never_interprets_the_terminal() {
    let fence = fixture::fence();
    let holding = fixture::holding(&fence);
    let key = holding.key.clone();
    let stopped = fixture::origin_stopped(&holding);
    let other = fixture::origin_stopped(&fixture::holding(&fence));
    for (case, proof) in [
        Err(DecisionWaitPortError::Unavailable),
        Err(DecisionWaitPortError::Refused(
            koru_delegate_core::decisions::BotWaitRefusal::Stale,
        )),
        // The delegate does not know a wait the AI enrolled: contradictory.
        Ok(None),
        // A receipt for another wait on the same origin is not this one.
        read(&other),
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            classify_origin_terminal(&key, &fence, false, false, proof),
            Interception::Uncertain,
            "case {case}"
        );
    }
    assert_eq!(
        classify_origin_terminal(&key, &fence, false, false, &read(&stopped)),
        Interception::Suppress
    );
}

#[test]
fn ordinary_stop_completion_and_superseded_wait_keep_existing_mapping() {
    let fence = fixture::fence();
    let holding = fixture::holding(&fence);
    let key = holding.key.clone();
    let stopped = fixture::origin_stopped(&holding);
    // Ordinary Stop supersedes even a matching receipt.
    assert_eq!(
        classify_origin_terminal(&key, &fence, true, false, &read(&stopped)),
        Interception::Ordinary
    );
    // A natural completion is never an internal-stop outcome.
    assert_eq!(
        classify_origin_terminal(&key, &fence, false, true, &read(&stopped)),
        Interception::Ordinary
    );
    let mut superseded = holding.clone();
    superseded.fresh_stop_eligibility = FreshWaitStopEligibility::Superseded;
    assert_eq!(
        classify_origin_terminal(&key, &fence, false, false, &read(&superseded)),
        Interception::Ordinary
    );
}

#[test]
fn delayed_old_batch_and_consumer_cannot_borrow_the_origin_enrollment() {
    let fence = fixture::fence();
    let holding = fixture::holding(&fence);
    let key = holding.key.clone();
    let stopped = fixture::origin_stopped(&holding);
    let mut later_turn = fence.clone();
    later_turn.turn = Some(koru_delegate_core::AgentTurnId::mint());
    let mut consumer = fence.clone();
    consumer.attempt = koru_delegate_core::RunAttemptId::mint();
    consumer.generation =
        koru_delegate_core::AttemptGeneration::from_raw(fence.generation.get() + 1).unwrap();
    for other in [later_turn, consumer] {
        assert_eq!(
            classify_origin_terminal(&key, &other, false, false, &read(&stopped)),
            Interception::Ordinary
        );
    }
}

#[test]
fn held_wait_rolls_over_only_on_exact_acknowledged_input_and_closes_when_settled() {
    let fence = fixture::fence();
    let holding = fixture::holding(&fence);
    let key = holding.key.clone();
    let stopped = fixture::origin_stopped(&holding);
    assert_eq!(classify_held(&key, &read(&stopped), None), HeldNext::Wait);
    let acknowledged = fixture::acknowledged(&key);
    assert_eq!(
        classify_held(&key, &read(&stopped), Some(&Ok(Some(acknowledged.clone())))),
        HeldNext::Rollover(acknowledged.clone())
    );
    let mut unconfirmed = acknowledged.clone();
    unconfirmed.state = DecisionResumeState::InputUnconfirmed {};
    assert_eq!(
        classify_held(&key, &read(&stopped), Some(&Ok(Some(unconfirmed)))),
        HeldNext::Wait
    );
    let mut foreign = acknowledged.clone();
    foreign.decision = koru_delegate_core::decisions::DecisionId::mint();
    assert_eq!(
        classify_held(&key, &read(&stopped), Some(&Ok(Some(foreign)))),
        HeldNext::Wait
    );
    // Final AI closure needs no further provider event: the settled read.
    let denied = fixture::settled(
        &stopped,
        DecisionClosureReason::Denied,
        DecisionAuthorization::Denied,
    );
    assert_eq!(classify_held(&key, &read(&denied), None), HeldNext::Close);
    let expired = fixture::settled(
        &stopped,
        DecisionClosureReason::Expired,
        DecisionAuthorization::Expired,
    );
    assert_eq!(classify_held(&key, &read(&expired), None), HeldNext::Close);
    let completed = fixture::settled(
        &stopped,
        DecisionClosureReason::Completed,
        DecisionAuthorization::Consumed,
    );
    assert_eq!(classify_held(&key, &read(&completed), None), HeldNext::Wait);
    // An unavailable read never closes or rolls anything over.
    assert_eq!(
        classify_held(
            &key,
            &Err(DecisionWaitPortError::Unavailable),
            Some(&Ok(Some(acknowledged)))
        ),
        HeldNext::Wait
    );
}

#[test]
fn suppressed_batch_keeps_every_frame_and_drops_only_the_terminal_effect() {
    let fence = fixture::fence();
    let attempt = InferenceAttemptId::new();
    let batch = BotReplayBatch {
        ai_attempt: attempt,
        fence: fence.clone(),
        expected_after: Some(4),
        events: vec![
            BotReplayEvent {
                sequence: 5,
                turn: fence.turn,
                effects: vec![BotReplayEffect::Mode(None)],
            },
            BotReplayEvent {
                sequence: 6,
                turn: fence.turn,
                effects: vec![
                    BotReplayEffect::Mode(None),
                    BotReplayEffect::Event {
                        event: AiEvent::Cancelled {
                            attempt_id: attempt,
                            at: Timestamp::now(),
                        },
                        finalized_blocks: Vec::new(),
                        terminal_cleanup: Some(BotTerminalCleanup {
                            retained_question_ids: Vec::new(),
                        }),
                    },
                ],
            },
        ],
    };
    let stripped = suppressed_batch(&batch);
    assert_eq!(
        stripped
            .events
            .iter()
            .map(|e| e.sequence)
            .collect::<Vec<_>>(),
        vec![5, 6]
    );
    assert_eq!(stripped.expected_after, Some(4));
    assert!(stripped.events.iter().all(|event| {
        event.effects.iter().all(|effect| {
            !matches!(
                effect,
                BotReplayEffect::Event {
                    terminal_cleanup: Some(_),
                    ..
                }
            )
        })
    }));
    assert_eq!(stripped.events[1].effects.len(), 1);
    // The original candidate is untouched for an uncertain retry.
    assert_eq!(batch.events[1].effects.len(), 2);
}

#[test]
fn needed_you_closure_publishes_exactly_one_closing_event_and_no_failure() {
    let attempt = InferenceAttemptId::new();
    let at = Timestamp::from_millis(5);
    for reason in [
        Some(koru_ai_core::NeededYouReason::Declined),
        Some(koru_ai_core::NeededYouReason::NotAnswered),
        None,
    ] {
        let event = needed_you_event(attempt, BotDecisionClosure::Closed { reason }, at)
            .expect("the closure that wrote it publishes one event");
        assert_eq!(
            event,
            PublicAiEvent::NeededYou {
                attempt_id: attempt,
                reason,
                at
            }
        );
        // Terminal, and never a failure.
        assert!(event.is_terminal());
        assert!(!matches!(event, PublicAiEvent::Failed { .. }));
    }
    // An exact replay, another cause's ending or an owed closure: nothing.
    for closure in [
        BotDecisionClosure::ClosedBefore,
        BotDecisionClosure::AlreadyEnded,
        BotDecisionClosure::Owed,
    ] {
        assert_eq!(needed_you_event(attempt, closure, at), None, "{closure:?}");
    }
}
