// SPDX-License-Identifier: MPL-2.0
//! Scripted delegate observations for AI-side decision tests. They stand in
//! for the authenticated IPC16 read; scripted composition is not provider,
//! actor or transport qualification.
use koru_ai_core::bots::replay::BotExecutionFence;
use koru_delegate_core as d;
use koru_delegate_core::decisions::*;

pub(in crate::worker) fn fence() -> BotExecutionFence {
    BotExecutionFence {
        owner: d::BotOwner {
            user_scope: koru_ai_core::bots::UserScopeId::new(),
            device: d::DeviceId::new(),
        },
        bot: d::BotId::mint(),
        run: d::RunId::mint(),
        attempt: d::RunAttemptId::mint(),
        generation: d::AttemptGeneration::from_raw(1).unwrap(),
        session: d::SessionId::mint(),
        turn: Some(d::AgentTurnId::mint()),
    }
}

/// The enrollment-time read: eligible, Open/Pending/Holding, applied card.
pub(in crate::worker) fn holding(fence: &BotExecutionFence) -> BotDecisionWaitObservation {
    let cleanup_intent = BotWaitCleanupIntentV1 {
        version: DecisionVersionV1,
        owner: fence.owner,
        bot: fence.bot,
        run: fence.run,
        wait_id: BotWaitId::mint(),
        decision_id: DecisionId::mint(),
        decision_revision: DecisionRevision::new(2).unwrap(),
        wait_revision: DecisionRevision::new(2).unwrap(),
        origin_attempt: fence.attempt,
        origin_generation: fence.generation,
        origin_session: fence.session,
        origin_turn: fence.turn.unwrap(),
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
    };
    let key = cleanup_intent.key().unwrap();
    let observed = BotDecisionWaitObservation {
        key: key.clone(),
        cleanup_intent,
        current_revision: key.wait_revision,
        fresh_stop_eligibility: FreshWaitStopEligibility::EligibleOriginWaitStop,
        origin_stop: OriginWaitStop::Pending {},
        consumer_cleanup: ConsumerCleanup::NotRequested {},
        disposition: DecisionDisposition::Open {
            state: DecisionState {
                kind: DecisionKind::KoruEdit,
                authorization: DecisionAuthorization::Pending,
                phase: DecisionPhase::Holding,
                revision: key.decision_revision,
            },
        },
        publication: DecisionPublication::Applied {
            transition: DecisionTransitionId::mint(),
            sequence: key.wait_revision,
            target_digest: DecisionDigest::from_bytes([1; 32]),
            observed_version: DecisionVersionVector::new(vec![1]).unwrap(),
        },
    };
    assert!(observed.validate().is_ok());
    observed
}

fn retirement(origin: &DecisionFence) -> ObservedCreationRetirement {
    ObservedCreationRetirement {
        creation_key: d::IdempotencyKey::mint(),
        host: d::HostInstanceId::from_uuid(uuid::Uuid::new_v4()),
        result: ObservedRetirementResult::KnownEnded {
            session: origin.session,
        },
        observed_at_ms: 5,
    }
}
fn drain(origin: &DecisionFence) -> DecisionNativeDrain {
    DecisionNativeDrain {
        run: origin.run,
        attempt: origin.attempt,
        generation: origin.generation,
        observed_at_ms: 5,
        effects: DecisionList::default(),
    }
}

/// The internal origin wait-stop happened with this exact enrollment.
pub(in crate::worker) fn origin_stopped(
    holding: &BotDecisionWaitObservation,
) -> BotDecisionWaitObservation {
    let origin = holding.cleanup_intent.origin();
    let mut observed = holding.clone();
    observed.current_revision = DecisionRevision::new(3).unwrap();
    observed.origin_stop = OriginWaitStop::Confirmed {
        origin_fence: origin,
        wait_id: holding.key.wait_id,
        intent_digest: holding.key.receipt_digest,
        enrollment_digest: holding.key.receipt_digest,
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
        creation_retirement: retirement(&origin),
        native_drain: drain(&origin),
    };
    observed.disposition = DecisionDisposition::Open {
        state: DecisionState {
            kind: DecisionKind::KoruEdit,
            authorization: DecisionAuthorization::Pending,
            phase: DecisionPhase::Held,
            revision: DecisionRevision::new(3).unwrap(),
        },
    };
    assert!(observed.validate().is_ok());
    observed
}

/// Final authoritative read: settled, cleanup confirmed, publication applied.
pub(in crate::worker) fn settled(
    stopped: &BotDecisionWaitObservation,
    reason: DecisionClosureReason,
    authorization: DecisionAuthorization,
) -> BotDecisionWaitObservation {
    let origin = stopped.cleanup_intent.origin();
    let retired = retirement(&origin);
    let revision = DecisionRevision::new(4).unwrap();
    let mut observed = stopped.clone();
    observed.current_revision = revision;
    observed.fresh_stop_eligibility = FreshWaitStopEligibility::Terminal;
    observed.consumer_cleanup = ConsumerCleanup::Confirmed {
        cleanup_revision: revision,
        target: DecisionCleanupTarget::Origin {
            fence: origin,
            creation_key: retired.creation_key,
        },
        creation_retirement: retired,
        attempt_retirement: DecisionAttemptRetirement {
            run: origin.run,
            attempt: origin.attempt,
            generation: origin.generation,
            retired_at_ms: 5,
        },
        native_drain: drain(&origin),
        retained_effect_outcome: None,
    };
    observed.disposition = DecisionDisposition::Settled {
        state: DecisionState {
            kind: DecisionKind::KoruEdit,
            authorization,
            phase: DecisionPhase::Settled,
            revision,
        },
        reason,
        outcome_digest: DecisionDigest::from_bytes([3; 32]),
    };
    assert!(observed.validate().is_ok());
    observed
}

/// A successor whose exact input the delegate acknowledged.
pub(in crate::worker) fn acknowledged(key: &BotWaitEnrollmentKey) -> DecisionResume {
    let origin = DecisionFence {
        owner: key.owner,
        bot: key.bot,
        run: key.run,
        attempt: key.origin_attempt,
        generation: key.origin_generation,
        session: key.origin_session,
        turn: key.origin_turn,
    };
    let resume = DecisionResume {
        decision: key.decision_id,
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
