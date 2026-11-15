// SPDX-License-Identifier: MPL-2.0
//! Synthetic repository observations do not qualify the stage E handshake.
use super::*;
use koru_delegate_core as d;

fn fixture() -> (
    Repository,
    BotOwner,
    EnrollBotWaitCleanup,
    BotDecisionWaitObservation,
    InferenceAttemptId,
) {
    let (mut repo, turn, submission, fence) = crate::repository::bot_replay::tests::prepared();
    let p = &submission.provenance;
    let owner = BotOwner {
        user_scope: p.owner.user_scope,
        device: p.owner.device,
    };
    let admission = d::BotAdmissionView {
        key: d::BotSubmissionKey {
            owner: p.owner,
            bot: p.bot,
            request: p.submission,
        },
        run: submission.request.run,
        state: d::BotAdmissionState::Executing,
        queue_sequence: 1,
        fence: Some(fence.clone()),
    };
    repo.connection
        .execute(
            "UPDATE bots SET folder_lock_request_id=?1 WHERE bot_id=?2",
            params![p.submission.to_storage(), p.bot.to_storage()],
        )
        .unwrap();
    repo.acknowledge_bot_submission(owner, p.bot, p.submission, &admission, 3)
        .unwrap();
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET fingerprint=?1 WHERE request_id=?2",
            params!["a".repeat(64), p.submission.to_storage()],
        )
        .unwrap();
    let cleanup_intent = BotWaitCleanupIntentV1 {
        version: DecisionVersionV1,
        owner: p.owner,
        bot: p.bot,
        run: submission.request.run,
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
    let observation = BotDecisionWaitObservation {
        key: key.clone(),
        cleanup_intent: cleanup_intent.clone(),
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
    (
        repo,
        owner,
        EnrollBotWaitCleanup {
            key,
            cleanup_intent,
        },
        observation,
        turn.attempt.id,
    )
}
fn enroll(
    repo: &mut Repository,
    owner: BotOwner,
    request: &EnrollBotWaitCleanup,
    qualified: &BotDecisionWaitObservation,
) -> Result<BotWaitEnrollmentReply, BotError> {
    repo.enroll_bot_wait_cleanup(owner, request, qualified, Timestamp::from_millis(10))
}
fn count(repo: &Repository) -> i64 {
    repo.connection
        .query_row(
            "SELECT count(*) FROM bot_decision_wait_enrollments",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn enrollment_without_card_is_immutable_and_exact_retry_survives_policy_and_terminal_change() {
    let (mut repo, owner, request, mut qualified, attempt) = fixture();
    assert_eq!(
        repo.connection
            .query_row("SELECT count(*) FROM bot_decision_invocations", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
    let before = repo.bot_replay_state(attempt).unwrap();
    let first = enroll(&mut repo, owner, &request, &qualified).unwrap();
    assert_eq!(count(&repo), 1);
    repo.connection
        .execute(
            "UPDATE bots SET model='',policy_revision=policy_revision+1,paused=1",
            [],
        )
        .unwrap();
    repo.connection.execute("UPDATE inference_attempts SET status='interrupted',stop_reason='human_stop' WHERE id=?1",[attempt.to_storage()]).unwrap();
    repo.connection
        .execute(
            "UPDATE bot_run_replay SET stop_reason='ai_restart' WHERE ai_attempt_id=?1",
            [attempt.to_storage()],
        )
        .unwrap();
    qualified.fresh_stop_eligibility = FreshWaitStopEligibility::Terminal;
    assert_eq!(
        enroll(&mut repo, owner, &request, &qualified).unwrap(),
        first
    );
    assert_eq!(
        repo.read_bot_wait_cleanup(
            owner,
            &ReadBotWaitCleanup {
                key: request.key.clone()
            }
        )
        .unwrap(),
        first
    );
    assert_eq!(
        repo.bot_replay_state(attempt).unwrap().last_sequence,
        before.last_sequence
    );
    assert!(
        repo.connection
            .execute(
                "UPDATE bot_decision_wait_enrollments SET created_at_ms=20",
                []
            )
            .is_err()
    );
    assert!(
        repo.connection
            .execute("DELETE FROM bot_decision_wait_enrollments", [])
            .is_err()
    );
}

#[test]
fn enrollment_changed_duplicate_and_foreign_owner_refuse() {
    let (mut repo, owner, request, qualified, _) = fixture();
    enroll(&mut repo, owner, &request, &qualified).unwrap();
    let mut changed = request.clone();
    changed.cleanup_intent.origin_turn = d::AgentTurnId::mint();
    changed.key = changed.cleanup_intent.key().unwrap();
    assert_eq!(
        enroll(&mut repo, owner, &changed, &qualified),
        Err(BotError::Conflict)
    );
    let foreign = BotOwner {
        device: DeviceId::new(),
        ..owner
    };
    assert_eq!(
        enroll(&mut repo, foreign, &request, &qualified),
        Err(BotError::Forbidden)
    );
    assert_eq!(
        repo.read_bot_wait_cleanup(
            foreign,
            &ReadBotWaitCleanup {
                key: request.key.clone()
            }
        ),
        Err(BotError::Forbidden)
    );
    assert_eq!(count(&repo), 1);
    let mut second = request.clone();
    second.cleanup_intent.wait_id = BotWaitId::mint();
    second.cleanup_intent.decision_id = DecisionId::mint();
    second.key = second.cleanup_intent.key().unwrap();
    let mut second_qualified = qualified.clone();
    second_qualified.key = second.key.clone();
    second_qualified.cleanup_intent = second.cleanup_intent.clone();
    enroll(&mut repo, owner, &second, &second_qualified).unwrap();
    let mut crosswired = request.clone();
    crosswired.cleanup_intent.decision_id = second.key.decision_id;
    crosswired.key = crosswired.cleanup_intent.key().unwrap();
    assert_eq!(
        enroll(&mut repo, owner, &crosswired, &qualified),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.read_bot_wait_cleanup(
            owner,
            &ReadBotWaitCleanup {
                key: crosswired.key
            }
        ),
        Err(BotError::Conflict)
    );
    assert_eq!(count(&repo), 2);
}

#[test]
fn enrollment_historical_retry_survives_current_replay_successor() {
    let (mut repo, owner, request, mut qualified, attempt) = fixture();
    let first = enroll(&mut repo, owner, &request, &qualified).unwrap();
    repo.connection.execute("UPDATE bot_run_replay SET run_attempt_id=?1,generation=generation+1,session_id=?2,turn_id=?3 WHERE ai_attempt_id=?4",
        params![d::RunAttemptId::mint().to_storage(),d::SessionId::mint().to_string(),d::AgentTurnId::mint().to_string(),attempt.to_storage()]).unwrap();
    qualified.fresh_stop_eligibility = FreshWaitStopEligibility::Superseded;
    assert_eq!(
        repo.read_bot_wait_cleanup(
            owner,
            &ReadBotWaitCleanup {
                key: request.key.clone()
            }
        )
        .unwrap(),
        first
    );
    assert_eq!(
        enroll(&mut repo, owner, &request, &qualified).unwrap(),
        first
    );
    assert_eq!(count(&repo), 1);
}

#[test]
fn enrollment_fresh_fence_ordinary_stop_and_ineligible_observation_refuse_atomically() {
    for case in 0..7 {
        let (mut repo, owner, request, mut qualified, attempt) = fixture();
        match case {
            0 => qualified.fresh_stop_eligibility=FreshWaitStopEligibility::Superseded,
            1 => qualified.current_revision=DecisionRevision::new(3).unwrap(),
            2 => repo.connection.execute("UPDATE inference_attempts SET stop_reason='human_stop' WHERE id=?1",[attempt.to_storage()]).map(|_|()).unwrap(),
            3 => repo.connection.execute("UPDATE bot_run_replay SET stop_reason='owner_unavailable' WHERE ai_attempt_id=?1",[attempt.to_storage()]).map(|_|()).unwrap(),
            4 => repo.connection.execute("UPDATE bot_run_replay SET turn_id=?1 WHERE ai_attempt_id=?2",params![d::AgentTurnId::mint().to_string(),attempt.to_storage()]).map(|_|()).unwrap(),
            5 => qualified.cleanup_intent.origin_turn=d::AgentTurnId::mint(),
            _ => repo.connection.execute("UPDATE inference_attempts SET status='complete' WHERE id=?1",[attempt.to_storage()]).map(|_|()).unwrap(),
        }
        assert!(
            enroll(&mut repo, owner, &request, &qualified).is_err(),
            "case {case}"
        );
        assert_eq!(count(&repo), 0, "case {case}");
    }
}

#[test]
fn enrollment_missing_requires_validated_retained_subject() {
    let (repo, owner, request, _, attempt) = fixture();
    let read = ReadBotWaitCleanup {
        key: request.key.clone(),
    };
    assert_eq!(
        repo.read_bot_wait_cleanup(owner, &read).unwrap(),
        BotWaitEnrollmentReply::Missing {
            key: request.key.clone()
        }
    );
    let mut unknown = read.clone();
    unknown.key.run = d::RunId::mint();
    assert!(repo.read_bot_wait_cleanup(owner, &unknown).is_err());
    repo.connection
        .execute(
            "UPDATE bot_run_replay SET turn_id=?1 WHERE ai_attempt_id=?2",
            params![d::AgentTurnId::mint().to_string(), attempt.to_storage()],
        )
        .unwrap();
    assert!(repo.read_bot_wait_cleanup(owner, &read).is_err());
    assert_eq!(count(&repo), 0);
}

#[test]
fn enrollment_corrupt_oversized_and_typed_rows_fail_closed() {
    for case in 0..6 {
        let (mut repo, owner, request, qualified, _) = fixture();
        enroll(&mut repo, owner, &request, &qualified).unwrap();
        repo.connection.execute_batch("DROP TRIGGER bot_decision_wait_enrollments_immutable; PRAGMA ignore_check_constraints=ON;").unwrap();
        match case {
            0 => repo.connection.execute("UPDATE bot_decision_wait_enrollments SET receipt_json='{}'",[]).unwrap(),
            1 => repo.connection.execute("UPDATE bot_decision_wait_enrollments SET receipt_json=?1",[" ".repeat(4097)]).unwrap(),
            2 => repo.connection.execute("UPDATE bot_decision_wait_enrollments SET receipt_json=CAST(receipt_json AS BLOB)",[]).unwrap(),
            3 => repo.connection.execute("UPDATE bot_decision_wait_enrollments SET origin_generation=1.5",[]).unwrap(),
            4 => repo.connection.execute("UPDATE bot_decision_wait_enrollments SET receipt_digest=zeroblob(32)",[]).unwrap(),
            _ => repo.connection.execute("UPDATE bot_decision_wait_enrollments SET receipt_json=json_set(receipt_json,'$.unknown',1)",[]).unwrap(),
        };
        assert_eq!(
            repo.read_bot_wait_cleanup(
                owner,
                &ReadBotWaitCleanup {
                    key: request.key.clone()
                }
            ),
            Err(BotError::Storage),
            "case {case}"
        );
        assert_eq!(
            enroll(&mut repo, owner, &request, &qualified),
            Err(BotError::Storage),
            "case {case}"
        );
    }
}

#[test]
fn enrollment_insert_failure_rolls_back_and_lost_response_replays_first_receipt() {
    let (mut repo, owner, request, qualified, _) = fixture();
    repo.connection.execute_batch("CREATE TEMP TRIGGER fail_enrollment AFTER INSERT ON bot_decision_wait_enrollments BEGIN SELECT RAISE(ABORT,'fixture rollback'); END;").unwrap();
    assert!(enroll(&mut repo, owner, &request, &qualified).is_err());
    assert_eq!(count(&repo), 0);
    repo.connection
        .execute_batch("DROP TRIGGER fail_enrollment;")
        .unwrap();
    let lost = enroll(&mut repo, owner, &request, &qualified).unwrap();
    assert_eq!(
        enroll(&mut repo, owner, &request, &qualified).unwrap(),
        lost
    );
    assert_eq!(count(&repo), 1);
    assert_eq!(
        repo.connection
            .query_row(
                "SELECT created_at_ms FROM bot_decision_wait_enrollments",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        10
    );
}
