// SPDX-License-Identifier: MPL-2.0
//! In-memory repository fixtures. A synthetic delegate observation stands in
//! for the authenticated IPC16 read; this is not transport qualification.
use super::*;
use koru_delegate_core as d;

fn at() -> Timestamp {
    Timestamp::from_millis(30)
}

fn enrolled() -> (
    Repository,
    BotOwner,
    BotWaitEnrollmentKey,
    BotDecisionWaitObservation,
    InferenceAttemptId,
) {
    let (mut repo, owner, request, qualified, attempt) = enrollment::tests::fixture();
    repo.enroll_bot_wait_cleanup(owner, &request, &qualified, at())
        .unwrap();
    (repo, owner, request.key, qualified, attempt)
}

/// The authoritative final read after a Koru Deny (O109): settled, origin
/// stop and final consumer cleanup confirmed, final publication applied.
fn settled(
    qualified: &BotDecisionWaitObservation,
    reason: DecisionClosureReason,
    authorization: DecisionAuthorization,
) -> BotDecisionWaitObservation {
    let origin = qualified.cleanup_intent.origin();
    let key = qualified.key.clone();
    let creation_key = d::IdempotencyKey::mint();
    let retirement = ObservedCreationRetirement {
        creation_key,
        host: d::HostInstanceId::from_uuid(Uuid::new_v4()),
        result: ObservedRetirementResult::KnownEnded {
            session: origin.session,
        },
        observed_at_ms: 5,
    };
    let drain = DecisionNativeDrain {
        run: origin.run,
        attempt: origin.attempt,
        generation: origin.generation,
        observed_at_ms: 5,
        effects: DecisionList::default(),
    };
    let revision = DecisionRevision::new(4).unwrap();
    let mut observed = qualified.clone();
    observed.current_revision = revision;
    observed.fresh_stop_eligibility = FreshWaitStopEligibility::Terminal;
    observed.origin_stop = OriginWaitStop::Confirmed {
        origin_fence: origin,
        wait_id: key.wait_id,
        intent_digest: key.receipt_digest,
        enrollment_digest: key.receipt_digest,
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
        creation_retirement: retirement.clone(),
        native_drain: drain.clone(),
    };
    observed.consumer_cleanup = ConsumerCleanup::Confirmed {
        cleanup_revision: revision,
        target: DecisionCleanupTarget::Origin {
            fence: origin,
            creation_key,
        },
        creation_retirement: retirement,
        attempt_retirement: DecisionAttemptRetirement {
            run: origin.run,
            attempt: origin.attempt,
            generation: origin.generation,
            retired_at_ms: 5,
        },
        native_drain: drain,
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
    observed.publication = DecisionPublication::Applied {
        transition: DecisionTransitionId::mint(),
        sequence: revision,
        target_digest: DecisionDigest::from_bytes([4; 32]),
        observed_version: DecisionVersionVector::new(vec![2]).unwrap(),
    };
    assert!(observed.validate().is_ok());
    observed
}

fn denied(qualified: &BotDecisionWaitObservation) -> BotDecisionWaitObservation {
    settled(
        qualified,
        DecisionClosureReason::Denied,
        DecisionAuthorization::Denied,
    )
}

/// (status, attempt stop_reason, assistant message state)
fn ending(repo: &Repository, attempt: InferenceAttemptId) -> (String, Option<String>, String) {
    repo.connection
        .query_row(
            "SELECT a.status,a.stop_reason,m.state FROM inference_attempts a
             JOIN messages m ON m.id=a.assistant_message_id WHERE a.id=?1",
            [attempt.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

fn live(repo: &Repository, attempt: InferenceAttemptId) -> bool {
    repo.connection
        .query_row(
            "SELECT status IN ('pending','streaming') AND stop_reason IS NULL FROM inference_attempts WHERE id=?1",
            [attempt.to_storage()],
            |r| r.get(0),
        )
        .unwrap()
}

fn open_tool(repo: &Repository, attempt: InferenceAttemptId) -> ToolInvocationId {
    let chat: String = repo
        .connection
        .query_row(
            "SELECT chat_id FROM bot_chat_runs WHERE ai_attempt_id=?1",
            [attempt.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    let invocation = ToolInvocationId::new();
    repo.connection.execute("INSERT INTO tool_invocations(id,attempt_id,chat_id,tool_name,read_only,status,approval,proposed_at_ms) VALUES(?1,?2,?3,'tool',0,'approval_required','pending',1)",
        params![invocation.to_storage(),attempt.to_storage(),chat]).unwrap();
    invocation
}

#[test]
fn decision_closure_ends_needed_you_without_another_provider_event() {
    let (mut repo, owner, key, qualified, attempt) = enrolled();
    let tool = open_tool(&repo, attempt);
    let before = repo.bot_replay_state(attempt).unwrap();
    let observed = denied(&qualified);
    // No native terminal event was applied: the closure alone ends the task.
    assert_eq!(
        repo.close_bot_decision_needed_you(owner, &key, &observed, at()),
        Ok(BotDecisionClosure::Closed)
    );
    assert_eq!(
        ending(&repo, attempt),
        (
            "interrupted".to_owned(),
            Some(BOT_DECISION_NEEDED_YOU.to_owned()),
            "interrupted".to_owned()
        )
    );
    // Replay fence, cursor and the closed replay stop vocabulary are untouched.
    assert_eq!(repo.bot_replay_state(attempt).unwrap(), before);
    let (status, approval): (String, String) = repo
        .connection
        .query_row(
            "SELECT status,approval FROM tool_invocations WHERE id=?1",
            [tool.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), approval.as_str()),
        ("cancelled", "denied")
    );
    let completed = |repo: &Repository| -> i64 {
        repo.connection
            .query_row(
                "SELECT completed_at_ms FROM inference_attempts WHERE id=?1",
                [attempt.to_storage()],
                |r| r.get(0),
            )
            .unwrap()
    };
    let first = completed(&repo);
    // Exact replay after a lost writer answer is the same closure, no rewrite.
    assert_eq!(
        repo.close_bot_decision_needed_you(
            owner,
            &key,
            &observed,
            Timestamp::from_millis(i64::MAX / 2)
        ),
        Ok(BotDecisionClosure::Closed)
    );
    assert_eq!(completed(&repo), first);
    assert_eq!(
        ending(&repo, attempt).1.as_deref(),
        Some(BOT_DECISION_NEEDED_YOU)
    );
}

#[test]
fn decision_closure_stays_owed_until_settled_cleanup_and_publication() {
    let (mut repo, owner, key, qualified, attempt) = enrolled();
    let final_read = denied(&qualified);
    let mut unpublished = final_read.clone();
    unpublished.publication = DecisionPublication::Pending {
        latest_sequence: DecisionRevision::new(4).unwrap(),
        target_digest: DecisionDigest::from_bytes([4; 32]),
    };
    let mut uncleaned = final_read.clone();
    uncleaned.consumer_cleanup = ConsumerCleanup::Owed {
        cleanup_revision: DecisionRevision::new(4).unwrap(),
        target: DecisionCleanupTarget::Origin {
            fence: qualified.cleanup_intent.origin(),
            creation_key: d::IdempotencyKey::mint(),
        },
    };
    let mut requested_nothing = final_read.clone();
    requested_nothing.consumer_cleanup = ConsumerCleanup::NotRequested {};
    // Origin stop confirmed is historical; it is never the final cleanup.
    for (case, observed) in [qualified, unpublished, uncleaned, requested_nothing]
        .iter()
        .enumerate()
    {
        assert_eq!(
            repo.close_bot_decision_needed_you(owner, &key, observed, at()),
            Ok(BotDecisionClosure::Owed),
            "case {case}"
        );
        assert!(live(&repo, attempt), "case {case}");
    }
}

#[test]
fn decision_closure_after_ordinary_stop_keeps_that_ending() {
    let (mut repo, owner, key, qualified, attempt) = enrolled();
    repo.cancel_bot_chat(attempt, at()).unwrap();
    let stopped = ending(&repo, attempt);
    assert_eq!(stopped.0, "cancelled");
    assert_eq!(
        repo.close_bot_decision_needed_you(owner, &key, &denied(&qualified), at()),
        Ok(BotDecisionClosure::AlreadyEnded)
    );
    assert_eq!(ending(&repo, attempt), stopped);
}

#[test]
fn decision_closure_refuses_completed_foreign_unknown_and_other_wait() {
    let (mut repo, owner, key, qualified, attempt) = enrolled();
    let completed = settled(
        &qualified,
        DecisionClosureReason::Completed,
        DecisionAuthorization::Consumed,
    );
    assert_eq!(
        repo.close_bot_decision_needed_you(owner, &key, &completed, at()),
        Err(BotError::Conflict)
    );
    let mut other = denied(&qualified);
    other.cleanup_intent.decision_id = DecisionId::mint();
    other.key = other.cleanup_intent.key().unwrap();
    other.origin_stop = OriginWaitStop::Pending {};
    assert!(other.validate().is_ok());
    assert_eq!(
        repo.close_bot_decision_needed_you(owner, &key, &other, at()),
        Err(BotError::Conflict)
    );
    let foreign = BotOwner {
        device: DeviceId::new(),
        ..owner
    };
    assert_eq!(
        repo.close_bot_decision_needed_you(foreign, &key, &denied(&qualified), at()),
        Err(BotError::Forbidden)
    );
    let mut unknown = key.clone();
    unknown.wait_id = BotWaitId::mint();
    unknown.decision_id = DecisionId::mint();
    assert_eq!(
        repo.close_bot_decision_needed_you(owner, &unknown, &denied(&qualified), at()),
        Err(BotError::NotFound)
    );
    assert!(live(&repo, attempt));
}

#[test]
fn decision_closure_refuses_replay_owned_by_an_unrelated_fence() {
    let (mut repo, owner, key, qualified, attempt) = enrolled();
    repo.connection
        .execute(
            "UPDATE bot_run_replay SET turn_id=?1 WHERE ai_attempt_id=?2",
            params![d::AgentTurnId::mint().to_string(), attempt.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.close_bot_decision_needed_you(owner, &key, &denied(&qualified), at()),
        Err(BotError::Conflict)
    );
    assert!(live(&repo, attempt));
}

#[test]
fn origin_enrollment_lookup_matches_only_the_exact_enrolled_origin_fence() {
    let (mut repo, owner, request, qualified, attempt) = enrollment::tests::fixture();
    let fence = repo.bot_replay_state(attempt).unwrap().fence.unwrap();
    assert_eq!(
        repo.bot_wait_enrollment_for_origin(owner, attempt, &fence),
        Ok(None)
    );
    repo.enroll_bot_wait_cleanup(owner, &request, &qualified, at())
        .unwrap();
    assert_eq!(
        repo.bot_wait_enrollment_for_origin(owner, attempt, &fence),
        Ok(Some(request.key.clone()))
    );
    let mut later = fence.clone();
    later.turn = Some(d::AgentTurnId::mint());
    let mut unturned = fence.clone();
    unturned.turn = None;
    let mut consumer = fence.clone();
    consumer.attempt = d::RunAttemptId::mint();
    consumer.generation = d::AttemptGeneration::from_raw(fence.generation.get() + 1).unwrap();
    consumer.session = d::SessionId::mint();
    for (case, other) in [later, unturned, consumer].iter().enumerate() {
        assert_eq!(
            repo.bot_wait_enrollment_for_origin(owner, attempt, other),
            Ok(None),
            "case {case}"
        );
    }
    let foreign = BotOwner {
        device: DeviceId::new(),
        ..owner
    };
    assert_eq!(
        repo.bot_wait_enrollment_for_origin(foreign, attempt, &fence),
        Err(BotError::Forbidden)
    );
}

#[test]
fn origin_enrollment_lookup_corrupt_row_fails_closed() {
    for case in 0..3 {
        let (mut repo, owner, request, qualified, attempt) = enrollment::tests::fixture();
        let fence = repo.bot_replay_state(attempt).unwrap().fence.unwrap();
        repo.enroll_bot_wait_cleanup(owner, &request, &qualified, at())
            .unwrap();
        repo.connection.execute_batch("DROP TRIGGER bot_decision_wait_enrollments_immutable; PRAGMA ignore_check_constraints=ON;").unwrap();
        match case {
            0 => repo
                .connection
                .execute(
                    "UPDATE bot_decision_wait_enrollments SET receipt_json='{}'",
                    [],
                )
                .unwrap(),
            1 => repo
                .connection
                .execute(
                    "UPDATE bot_decision_wait_enrollments SET receipt_json=?1",
                    [" ".repeat(4097)],
                )
                .unwrap(),
            _ => repo
                .connection
                .execute(
                    "UPDATE bot_decision_wait_enrollments SET receipt_digest=zeroblob(32)",
                    [],
                )
                .unwrap(),
        };
        assert_eq!(
            repo.bot_wait_enrollment_for_origin(owner, attempt, &fence),
            Err(BotError::Storage),
            "case {case}"
        );
    }
}
