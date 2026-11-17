// SPDX-License-Identifier: MPL-2.0
//! Cohort `m6_child_interceptor`: the child wait's origin terminal verdict
//! and held-wait verdict over scripted delegate observations. Scripted
//! composition, not provider, actor or transport qualification.
use super::*;
use crate::worker::bots::decision_wait::fixture;
use koru_delegate_core as d;
use koru_delegate_core::children::BotChildWaitCleanupIntentV1;
use koru_delegate_core::decisions::*;

fn holding(fence: &BotExecutionFence) -> BotChildWaitObservation {
    let origin = DecisionFence {
        owner: fence.owner,
        bot: fence.bot,
        run: fence.run,
        attempt: fence.attempt,
        generation: fence.generation,
        session: fence.session,
        turn: fence.turn.unwrap(),
    };
    let cleanup_intent = BotChildWaitCleanupIntentV1::frozen(BotWaitId::mint(), &origin).unwrap();
    let key = cleanup_intent.key().unwrap();
    let observed = BotChildWaitObservation {
        current_revision: key.wait_revision,
        key,
        cleanup_intent,
        fresh_stop_eligibility: FreshWaitStopEligibility::EligibleOriginWaitStop,
        origin_stop: OriginWaitStop::Pending {},
        consumer_cleanup: ConsumerCleanup::NotRequested {},
        disposition: ChildWaitDisposition::Open {},
        resume: None,
    };
    assert!(observed.validate().is_ok());
    observed
}

fn stopped(holding: &BotChildWaitObservation) -> BotChildWaitObservation {
    let origin = holding.cleanup_intent.origin();
    let mut observed = holding.clone();
    observed.current_revision = DecisionRevision::new(2).unwrap();
    observed.origin_stop = OriginWaitStop::Confirmed {
        origin_fence: origin,
        wait_id: holding.key.wait_id,
        intent_digest: holding.key.receipt_digest,
        enrollment_digest: holding.key.receipt_digest,
        cause: BotWaitCleanupCause::ChildWaitCleanup,
        creation_retirement: ObservedCreationRetirement {
            creation_key: d::IdempotencyKey::mint(),
            host: d::HostInstanceId::from_uuid(uuid::Uuid::new_v4()),
            result: ObservedRetirementResult::KnownEnded {
                session: origin.session,
            },
            observed_at_ms: 5,
        },
        native_drain: DecisionNativeDrain {
            run: origin.run,
            attempt: origin.attempt,
            generation: origin.generation,
            observed_at_ms: 5,
            effects: DecisionList::default(),
        },
    };
    assert!(observed.validate().is_ok());
    observed
}

fn acknowledged(observed: &BotChildWaitObservation) -> ChildResume {
    let origin = observed.key.origin();
    let resume = ChildResume {
        wait: observed.key.wait_id,
        origin,
        consumer: DecisionConsumer::RecordedSuccessor {
            attempt: d::RunAttemptId::mint(),
            generation: d::AttemptGeneration::from_raw(origin.generation.get() + 1).unwrap(),
            creation_key: d::IdempotencyKey::mint(),
            session: Some(d::SessionId::mint()),
        },
        input_key: d::IdempotencyKey::mint(),
        input_fingerprint: [7; 32],
        state: DecisionResumeState::InputAcknowledged {
            acknowledgment: DecisionInputAcknowledgment {
                turn: d::AgentTurnId::mint(),
                receipt_digest: DecisionDigest::from_bytes([9; 32]),
            },
        },
    };
    assert!(resume.validate().is_ok());
    resume
}

fn read(observed: &BotChildWaitObservation) -> ChildWaitRead {
    Ok(observed.clone())
}

#[test]
fn m6_child_interceptor_a_stop_caused_terminal_is_suppressed_only_on_the_matching_confirmed_proof()
{
    let fence = fixture::fence();
    let holding = holding(&fence);
    let key = holding.key.clone();
    // The internal stop is still owed: the terminal stays staged.
    assert_eq!(
        classify_child_origin_terminal(&key, &fence, false, false, &read(&holding)),
        Interception::Uncertain
    );
    let stopped = stopped(&holding);
    assert_eq!(
        classify_child_origin_terminal(&key, &fence, false, false, &read(&stopped)),
        Interception::Suppress
    );
    // A confirmed stop of another wait on the same origin is not this one.
    let other = stopped_of_other(&fence);
    assert_eq!(
        classify_child_origin_terminal(&key, &fence, false, false, &read(&other)),
        Interception::Uncertain
    );
}

fn stopped_of_other(fence: &BotExecutionFence) -> BotChildWaitObservation {
    stopped(&holding(fence))
}

#[test]
fn m6_child_interceptor_natural_completion_and_a_persons_stop_are_ordinary() {
    let fence = fixture::fence();
    let holding = holding(&fence);
    let key = holding.key.clone();
    let stopped = stopped(&holding);
    // O127: a completed turn is never the outcome of an internal stop.
    assert_eq!(
        classify_child_origin_terminal(&key, &fence, false, true, &read(&stopped)),
        Interception::Ordinary
    );
    // A person's Stop supersedes even a matching receipt.
    assert_eq!(
        classify_child_origin_terminal(&key, &fence, true, false, &read(&stopped)),
        Interception::Ordinary
    );
    // A wait closed before any internal stop: the terminal is ordinary.
    let mut closed = holding.clone();
    closed.fresh_stop_eligibility = FreshWaitStopEligibility::Terminal;
    closed.disposition = ChildWaitDisposition::Closed {
        cause: DecisionClosureReason::AiTerminal,
    };
    assert_eq!(
        classify_child_origin_terminal(&key, &fence, false, false, &read(&closed)),
        Interception::Ordinary
    );
    // A later turn or the successor never borrows the origin enrollment.
    let mut later = fence.clone();
    later.turn = Some(d::AgentTurnId::mint());
    assert_eq!(
        classify_child_origin_terminal(&key, &later, false, false, &read(&stopped)),
        Interception::Ordinary
    );
}

#[test]
fn m6_child_interceptor_an_uncertain_proof_interprets_nothing() {
    let fence = fixture::fence();
    let holding = holding(&fence);
    let key = holding.key.clone();
    for proof in [
        Err(ChildReadError::Unavailable),
        Err(ChildReadError::Unknown),
        Err(ChildReadError::Refused),
    ] {
        assert_eq!(
            classify_child_origin_terminal(&key, &fence, false, false, &proof),
            Interception::Uncertain
        );
        assert_eq!(classify_child_held(&key, &proof), ChildHeldNext::Wait);
    }
}

#[test]
fn m6_child_interceptor_the_held_wait_rolls_over_only_on_the_acknowledged_input_and_closes_without_one()
 {
    let fence = fixture::fence();
    let holding = holding(&fence);
    let key = holding.key.clone();
    let stopped = stopped(&holding);
    assert_eq!(
        classify_child_held(&key, &read(&stopped)),
        ChildHeldNext::Wait
    );
    let resume = acknowledged(&stopped);
    let mut resumed = stopped.clone();
    resumed.resume = Some(resume.clone());
    assert!(resumed.validate().is_ok());
    assert_eq!(
        classify_child_held(&key, &read(&resumed)),
        ChildHeldNext::Rollover(resume.clone())
    );
    // A reserved successor whose input is not yet acknowledged waits.
    let mut unconfirmed = resumed.clone();
    unconfirmed.resume.as_mut().unwrap().state = DecisionResumeState::InputUnconfirmed {};
    assert_eq!(
        classify_child_held(&key, &read(&unconfirmed)),
        ChildHeldNext::Wait
    );
    // A wait cleaned up while held (a Stop, the budget) ends the task.
    let mut canceled = stopped.clone();
    canceled.fresh_stop_eligibility = FreshWaitStopEligibility::Terminal;
    canceled.disposition = ChildWaitDisposition::Closed {
        cause: DecisionClosureReason::Canceled,
    };
    assert_eq!(
        classify_child_held(&key, &read(&canceled)),
        ChildHeldNext::Close
    );
    // An observation of another wait is never this one's.
    let other = stopped_of_other(&fence);
    assert_eq!(
        classify_child_held(&key, &read(&other)),
        ChildHeldNext::Wait
    );
}

#[test]
fn m6_child_interceptor_a_decision_enrollment_is_handled_as_before() {
    // The decision interceptor's own verdicts are unchanged by M6: the same
    // cases as its tests, through its own classifier.
    use super::super::decision::classify_origin_terminal;
    let fence = fixture::fence();
    let holding = fixture::holding(&fence);
    let stopped = fixture::origin_stopped(&holding);
    assert_eq!(
        classify_origin_terminal(&holding.key, &fence, false, false, &Ok(Some(stopped))),
        Interception::Suppress
    );
}
