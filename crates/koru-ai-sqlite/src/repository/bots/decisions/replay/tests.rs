// SPDX-License-Identifier: MPL-2.0
//! Repository-only fixtures, not transport or SameSession release qualification.
use super::*;
use koru_ai_core::bots::replay::{BotReplayBatch, BotReplayEvent};
use koru_delegate_core as d;

fn at() -> Timestamp {
    Timestamp::from_millis(20)
}
fn card(repo: &Repository, key: &BotWaitEnrollmentKey, attempt: InferenceAttemptId) {
    let (chat, message): (String, String) = repo
        .connection
        .query_row(
            "SELECT chat_id,assistant_message_id FROM bot_chat_runs WHERE ai_attempt_id=?1",
            [attempt.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let invocation = ToolInvocationId::new();
    repo.connection.execute("INSERT INTO tool_invocations(id,attempt_id,chat_id,tool_name,read_only,status,approval,proposed_at_ms) VALUES(?1,?2,?3,'tool',0,'approval_required','pending',1)",params![invocation.to_storage(),attempt.to_storage(),chat]).unwrap();
    repo.connection.execute("INSERT INTO bot_decision_invocations(decision_id,run_id,ai_attempt_id,chat_id,assistant_message_id,invocation_id,decision_revision,event_id,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1,1)",
        params![key.decision_id.to_storage(),key.run.to_storage(),attempt.to_storage(),chat,message,invocation.to_storage(),key.decision_revision.get(),Uuid::new_v4().to_string()]).unwrap();
}
fn fixture(
    same: bool,
) -> (
    Repository,
    BotOwner,
    BotWaitEnrollmentKey,
    DecisionResume,
    BotExecutionFence,
    InferenceAttemptId,
) {
    let (mut repo, owner, request, qualified, attempt) = enrollment::tests::fixture();
    repo.enroll_bot_wait_cleanup(owner, &request, &qualified, at())
        .unwrap();
    card(&repo, &request.key, attempt);
    repo.connection
        .execute(
            "UPDATE bot_run_replay SET last_sequence=1000 WHERE ai_attempt_id=?1",
            [attempt.to_storage()],
        )
        .unwrap();
    let old = repo.bot_replay_state(attempt).unwrap().fence.unwrap();
    let origin = qualified.cleanup_intent.origin();
    let consumer = if same {
        DecisionConsumer::SameSession {
            creation_key: d::IdempotencyKey::mint(),
        }
    } else {
        DecisionConsumer::RecordedSuccessor {
            attempt: d::RunAttemptId::mint(),
            generation: d::AttemptGeneration::from_raw(origin.generation.get() + 1).unwrap(),
            creation_key: d::IdempotencyKey::mint(),
            session: Some(d::SessionId::mint()),
        }
    };
    let resume = DecisionResume {
        decision: request.key.decision_id,
        origin,
        consumer,
        input_key: d::IdempotencyKey::mint(),
        input_fingerprint: [7; 32],
        state: DecisionResumeState::InputAcknowledged {
            acknowledgment: DecisionInputAcknowledgment {
                turn: d::AgentTurnId::mint(),
                receipt_digest: DecisionDigest::from_bytes([8; 32]),
            },
        },
    };
    (repo, owner, request.key, resume, old, attempt)
}
fn roll(
    repo: &mut Repository,
    owner: BotOwner,
    key: &BotWaitEnrollmentKey,
    resume: &DecisionResume,
    old: &BotExecutionFence,
) -> Result<BotDecisionReplayRollover, BotError> {
    repo.rollover_bot_decision_replay(owner, key, resume, old, Some(1000), at())
}
fn rows(repo: &Repository) -> i64 {
    repo.connection
        .query_row(
            "SELECT count(*) FROM bot_decision_replay_rollovers",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn successor_resets_high_cursor_and_old_fence_batch_is_rejected() {
    let (mut repo, owner, key, resume, old, attempt) = fixture(false);
    let result = roll(&mut repo, owner, &key, &resume, &old).unwrap();
    assert_eq!(result.old_last_sequence, Some(1000));
    assert_eq!(result.installed_after_sequence, None);
    let batch = BotReplayBatch {
        ai_attempt: attempt,
        fence: result.consumer_fence.clone(),
        expected_after: None,
        events: vec![BotReplayEvent {
            sequence: d::AGENT_EVENT_FIRST_SEQUENCE,
            turn: result.consumer_fence.turn,
            effects: vec![],
        }],
    };
    repo.apply_bot_replay(&batch, at()).unwrap();
    assert_eq!(
        repo.bot_replay_state(attempt).unwrap().last_sequence,
        Some(d::AGENT_EVENT_FIRST_SEQUENCE)
    );
    let old_batch = BotReplayBatch {
        ai_attempt: attempt,
        fence: old,
        expected_after: Some(1000),
        events: vec![BotReplayEvent {
            sequence: 1001,
            turn: Some(key.origin_turn),
            effects: vec![],
        }],
    };
    assert!(repo.apply_bot_replay(&old_batch, at()).is_err());
    assert_eq!(
        repo.bot_replay_state(attempt).unwrap().last_sequence,
        Some(d::AGENT_EVENT_FIRST_SEQUENCE)
    );
    assert_eq!(rows(&repo), 1);
}

#[test]
fn same_session_new_turn_preserves_exact_session_cursor() {
    let (mut repo, owner, key, resume, old, attempt) = fixture(true);
    let result = roll(&mut repo, owner, &key, &resume, &old).unwrap();
    assert_eq!(result.consumer_fence.session, old.session);
    assert_ne!(result.consumer_fence.turn, old.turn);
    assert_eq!(result.installed_after_sequence, Some(1000));
    assert_eq!(
        repo.bot_replay_state(attempt).unwrap().last_sequence,
        Some(1000)
    );
    assert_eq!(result.consumer_fence.attempt, old.attempt);
}

#[test]
fn exact_discarded_return_replays_after_terminal_policy_and_later_consumer_changes() {
    let (mut repo, owner, key, resume, old, attempt) = fixture(false);
    let discarded = roll(&mut repo, owner, &key, &resume, &old).unwrap();
    repo.connection
        .execute(
            "UPDATE bots SET model='',paused=1,policy_revision=policy_revision+1",
            [],
        )
        .unwrap();
    repo.connection.execute("UPDATE inference_attempts SET status='interrupted',stop_reason='human_stop' WHERE id=?1",[attempt.to_storage()]).unwrap();
    repo.connection.execute("UPDATE bot_run_replay SET run_attempt_id=?1,generation=generation+1,session_id=?2,turn_id=?3,last_sequence=99,stop_reason='ai_restart' WHERE ai_attempt_id=?4",
        params![d::RunAttemptId::mint().to_storage(),d::SessionId::mint().to_string(),d::AgentTurnId::mint().to_string(),attempt.to_storage()]).unwrap();
    assert_eq!(
        roll(&mut repo, owner, &key, &resume, &old).unwrap(),
        discarded
    );
    assert_eq!(rows(&repo), 1);
    let mut changed = resume.clone();
    changed.input_key = d::IdempotencyKey::mint();
    assert_eq!(
        roll(&mut repo, owner, &key, &changed, &old),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.rollover_bot_decision_replay(owner, &key, &resume, &old, Some(999), at()),
        Err(BotError::Conflict)
    );
}

#[test]
fn foreign_owner_or_invalid_existing_card_link_cannot_rollover() {
    let (mut repo, owner, key, resume, old, _) = fixture(false);
    assert_eq!(
        roll(
            &mut repo,
            BotOwner {
                device: DeviceId::new(),
                ..owner
            },
            &key,
            &resume,
            &old
        ),
        Err(BotError::Forbidden)
    );
    for mutation in [
        "UPDATE bot_decision_invocations SET run_id='00000000-0000-4000-8000-000000000001'",
        "UPDATE bot_decision_invocations SET assistant_message_id=(SELECT id FROM messages WHERE role='user' LIMIT 1)",
        "UPDATE bot_decision_invocations SET event_id='xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'",
        "UPDATE tool_invocations SET attempt_id='00000000-0000-4000-8000-000000000001'",
    ] {
        let (mut repo, owner, key, resume, old, _) = fixture(false);
        repo.connection
            .execute_batch(
                "PRAGMA foreign_keys=OFF; DROP TRIGGER bot_decision_invocations_immutable;",
            )
            .unwrap();
        repo.connection.execute(mutation, []).unwrap();
        assert!(
            roll(&mut repo, owner, &key, &resume, &old).is_err(),
            "{mutation}"
        );
        assert_eq!(rows(&repo), 0);
    }
}

#[test]
fn terminal_ordinary_stop_and_changed_expected_cursor_refuse_without_archive() {
    for case in 0..6 {
        let (mut repo, owner, key, mut resume, old, attempt) = fixture(false);
        let before = repo.bot_replay_state(attempt).unwrap();
        match case {
            0=>repo.connection.execute("UPDATE inference_attempts SET status='complete' WHERE id=?1",[attempt.to_storage()]).map(|_|()).unwrap(),
            1=>repo.connection.execute("UPDATE inference_attempts SET stop_reason='human_stop' WHERE id=?1",[attempt.to_storage()]).map(|_|()).unwrap(),
            2=>repo.connection.execute("UPDATE bot_run_replay SET stop_reason='owner_unavailable' WHERE ai_attempt_id=?1",[attempt.to_storage()]).map(|_|()).unwrap(),
            3=>repo.connection.execute("UPDATE bot_run_replay SET last_sequence=1001 WHERE ai_attempt_id=?1",[attempt.to_storage()]).map(|_|()).unwrap(),
            4=>resume.state=DecisionResumeState::InputUnconfirmed {},
            _=>resume.decision=DecisionId::mint(),
        }
        assert!(
            roll(&mut repo, owner, &key, &resume, &old).is_err(),
            "case {case}"
        );
        assert_eq!(rows(&repo), 0);
        assert_eq!(repo.bot_replay_state(attempt).unwrap().fence, before.fence);
    }
}

#[test]
fn insert_or_cas_failure_rolls_back_both_archive_and_replay() {
    for trigger in [
        "CREATE TEMP TRIGGER fail_roll AFTER INSERT ON bot_decision_replay_rollovers BEGIN SELECT RAISE(ABORT,'fixture'); END;",
        "CREATE TEMP TRIGGER fail_roll BEFORE UPDATE ON bot_run_replay BEGIN SELECT RAISE(IGNORE); END;",
        "CREATE TEMP TRIGGER fail_roll AFTER UPDATE ON bot_run_replay BEGIN SELECT RAISE(ABORT,'fixture'); END;",
    ] {
        let (mut repo, owner, key, resume, old, attempt) = fixture(false);
        let before = repo.bot_replay_state(attempt).unwrap();
        repo.connection.execute_batch(trigger).unwrap();
        assert!(roll(&mut repo, owner, &key, &resume, &old).is_err());
        assert_eq!(rows(&repo), 0);
        assert_eq!(repo.bot_replay_state(attempt).unwrap(), before);
    }
}

#[test]
fn malformed_oversized_typed_or_mismatched_archive_fails_closed() {
    for mutation in [
        "UPDATE bot_decision_replay_rollovers SET resume_receipt_json='{}'",
        "UPDATE bot_decision_replay_rollovers SET resume_receipt_json=printf('%65537s','')",
        "UPDATE bot_decision_replay_rollovers SET resume_receipt_json=CAST(resume_receipt_json AS BLOB)",
        "UPDATE bot_decision_replay_rollovers SET old_generation=1.5",
        "UPDATE bot_decision_replay_rollovers SET consumer_turn_id='00000000-0000-4000-8000-000000000001'",
        "UPDATE bot_decision_replay_rollovers SET installed_after_sequence=1000",
        "UPDATE bot_decision_replay_rollovers SET resume_receipt_json=json_set(resume_receipt_json,'$.unknown',1)",
    ] {
        let (mut repo, owner, key, resume, old, attempt) = fixture(false);
        roll(&mut repo, owner, &key, &resume, &old).unwrap();
        let before = repo.bot_replay_state(attempt).unwrap();
        repo.connection.execute_batch("DROP TRIGGER bot_decision_replay_rollovers_immutable; PRAGMA ignore_check_constraints=ON;").unwrap();
        repo.connection.execute(mutation, []).unwrap();
        assert_eq!(
            roll(&mut repo, owner, &key, &resume, &old),
            Err(BotError::Storage),
            "{mutation}"
        );
        assert_eq!(repo.bot_replay_state(attempt).unwrap(), before);
    }
}

#[test]
fn existing_other_decision_consumer_is_a_conflict() {
    let (mut repo, owner, key, resume, old, attempt) = fixture(false);
    let (_, _, mut request, mut qualified, _) = enrollment::tests::fixture();
    // A second valid enrollment on the same original subject, before rollover.
    request.cleanup_intent = BotWaitCleanupIntentV1 {
        version: DecisionVersionV1,
        owner: key.owner,
        bot: key.bot,
        run: key.run,
        wait_id: BotWaitId::mint(),
        decision_id: DecisionId::mint(),
        decision_revision: key.decision_revision,
        wait_revision: key.wait_revision,
        origin_attempt: key.origin_attempt,
        origin_generation: key.origin_generation,
        origin_session: key.origin_session,
        origin_turn: key.origin_turn,
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
    };
    request.key = request.cleanup_intent.key().unwrap();
    qualified.key = request.key.clone();
    qualified.cleanup_intent = request.cleanup_intent.clone();
    repo.enroll_bot_wait_cleanup(owner, &request, &qualified, at())
        .unwrap();
    card(&repo, &request.key, attempt);
    roll(&mut repo, owner, &key, &resume, &old).unwrap();
    let mut other = resume;
    other.decision = request.key.decision_id;
    assert_eq!(
        roll(&mut repo, owner, &request.key, &other, &old),
        Err(BotError::Conflict)
    );
    assert_eq!(rows(&repo), 1);
}

#[test]
fn later_wait_requires_exact_installed_consumer_and_retains_original_admission() {
    for case in 0..4 {
        let (mut repo, owner, key, resume, old, attempt) = fixture(false);
        let original: String = repo
            .connection
            .query_row(
                "SELECT outcome_json FROM bot_command_receipts WHERE planned_run_id=?1",
                [key.run.to_storage()],
                |r| r.get(0),
            )
            .unwrap();
        let prior = roll(&mut repo, owner, &key, &resume, &old).unwrap();
        let (_, _, mut request, mut qualified, _) = enrollment::tests::fixture();
        let consumer = &prior.consumer_fence;
        request.cleanup_intent = BotWaitCleanupIntentV1 {
            version: DecisionVersionV1,
            owner: key.owner,
            bot: key.bot,
            run: key.run,
            wait_id: BotWaitId::mint(),
            decision_id: DecisionId::mint(),
            decision_revision: key.decision_revision,
            wait_revision: key.wait_revision,
            origin_attempt: consumer.attempt,
            origin_generation: consumer.generation,
            origin_session: consumer.session,
            origin_turn: consumer.turn.unwrap(),
            cause: BotWaitCleanupCause::DecisionWaitCleanup,
        };
        if case == 1 {
            request.cleanup_intent.origin_turn = d::AgentTurnId::mint();
            repo.connection
                .execute(
                    "UPDATE bot_run_replay SET turn_id=?1 WHERE ai_attempt_id=?2",
                    params![
                        request.cleanup_intent.origin_turn.to_string(),
                        attempt.to_storage()
                    ],
                )
                .unwrap();
        }
        request.key = request.cleanup_intent.key().unwrap();
        qualified.key = request.key.clone();
        qualified.cleanup_intent = request.cleanup_intent.clone();
        if case == 2 {
            repo.connection.execute_batch("DROP TRIGGER bot_decision_replay_rollovers_retain; DELETE FROM bot_decision_replay_rollovers;").unwrap();
        } else if case == 3 {
            repo.connection.execute_batch("DROP TRIGGER bot_decision_replay_rollovers_immutable; UPDATE bot_decision_replay_rollovers SET resume_receipt_json='{}';").unwrap();
        }
        let result = repo.enroll_bot_wait_cleanup(owner, &request, &qualified, at());
        if case == 0 {
            assert!(result.is_ok());
            card(&repo, &request.key, attempt);
            let mut next = resume.clone();
            next.decision = request.key.decision_id;
            next.origin = request.cleanup_intent.origin();
            next.consumer = DecisionConsumer::RecordedSuccessor {
                attempt: d::RunAttemptId::mint(),
                generation: consumer.generation.next(),
                creation_key: d::IdempotencyKey::mint(),
                session: Some(d::SessionId::mint()),
            };
            next.state = DecisionResumeState::InputAcknowledged {
                acknowledgment: DecisionInputAcknowledgment {
                    turn: d::AgentTurnId::mint(),
                    receipt_digest: DecisionDigest::from_bytes([9; 32]),
                },
            };
            let second = repo
                .rollover_bot_decision_replay(owner, &request.key, &next, consumer, None, at())
                .unwrap();
            assert_eq!(second.old_fence, *consumer);
            assert_eq!(rows(&repo), 2);
            assert_eq!(roll(&mut repo, owner, &key, &resume, &old).unwrap(), prior);
        } else {
            assert!(result.is_err(), "case {case}");
        }
        let after: String = repo
            .connection
            .query_row(
                "SELECT outcome_json FROM bot_command_receipts WHERE planned_run_id=?1",
                [key.run.to_storage()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, original, "case {case}");
    }
}
