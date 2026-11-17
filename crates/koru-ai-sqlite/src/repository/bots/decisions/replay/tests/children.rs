// SPDX-License-Identifier: MPL-2.0
//! Cohort `m6_child_chat_rows`, the two-table half: the installed consumer of
//! an AI attempt is read from the decision rollovers and the child rollovers.
//! A child rollover may follow a decision rollover, and the reverse.
use super::*;
use crate::repository::bots::children::tests as child;
use koru_delegate_core::children::*;

fn as_origin(fence: &BotExecutionFence) -> DecisionFence {
    DecisionFence {
        owner: fence.owner,
        bot: fence.bot,
        run: fence.run,
        attempt: fence.attempt,
        generation: fence.generation,
        session: fence.session,
        turn: fence.turn.unwrap(),
    }
}

fn enroll_child(
    repo: &mut Repository,
    owner: BotOwner,
    origin: &DecisionFence,
) -> Result<BotChildWaitEnrollmentKey, BotError> {
    let intent = BotChildWaitCleanupIntentV1::frozen(BotWaitId::mint(), origin).unwrap();
    let key = intent.key().unwrap();
    repo.enroll_bot_child_wait(
        owner,
        &EnrollBotChildWaitCleanup {
            key: key.clone(),
            cleanup_intent: intent,
        },
        at(),
    )?;
    Ok(key)
}

#[test]
fn m6_child_chat_rows_a_child_ask_enrolls_after_a_decision_resume() {
    // A decision of this Run was answered and its successor installed.
    let (mut repo, owner, key, resume, old, attempt) = fixture(false);
    let decided = roll(&mut repo, owner, &key, &resume, &old).unwrap();
    assert_eq!(
        repo.bot_replay_state(attempt).unwrap().fence.as_ref(),
        Some(&decided.consumer_fence)
    );
    // The successor asks another Bot: its wait's origin is the consumer the
    // decision installed, which only the decision table knows.
    let origin = as_origin(&decided.consumer_fence);
    let child_key = enroll_child(&mut repo, owner, &origin).unwrap();
    assert_eq!(
        repo.bot_child_wait_enrollment_for_origin(owner, attempt, &decided.consumer_fence)
            .unwrap(),
        Some(child_key.clone())
    );
    // And the child wait's own successor is installed over it.
    let rolled = repo
        .rollover_bot_child_replay(
            owner,
            &child_key,
            &child::acknowledged(child_key.wait_id, origin),
            &decided.consumer_fence,
            None,
            at(),
        )
        .unwrap();
    assert_eq!(rolled.consumer_fence.generation.get(), 3);
    assert_eq!(
        repo.bot_replay_state(attempt).unwrap().fence.as_ref(),
        Some(&rolled.consumer_fence)
    );
    assert_eq!(rows(&repo), 1);
    // An execution no wait installed is still refused: this is an exact
    // lookup in two tables, not a generation test.
    let mut invented = as_origin(&rolled.consumer_fence);
    invented.attempt = d::RunAttemptId::mint();
    assert_eq!(
        enroll_child(&mut repo, owner, &invented),
        Err(BotError::Conflict)
    );
}

#[test]
fn m6_child_chat_rows_a_decision_enrolls_after_a_child_resume() {
    // The decision fixture's Run first waits on another Bot and is resumed.
    let (mut repo, owner, request, mut qualified, attempt) = enrollment::tests::fixture();
    let first = qualified.cleanup_intent.origin();
    let child_key = enroll_child(&mut repo, owner, &first).unwrap();
    let old = repo.bot_replay_state(attempt).unwrap().fence.unwrap();
    let rolled = repo
        .rollover_bot_child_replay(
            owner,
            &child_key,
            &child::acknowledged(child_key.wait_id, first),
            &old,
            None,
            at(),
        )
        .unwrap();
    // The decision's own enrollment at the retired origin is refused: the
    // Run's current execution is the child wait's successor.
    assert_eq!(
        repo.enroll_bot_wait_cleanup(owner, &request, &qualified, at()),
        Err(BotError::Conflict)
    );
    // The successor asks a decision. Its origin is the consumer the child
    // wait installed, which only the child table knows. Before M6 this read
    // the decision table alone and refused.
    let consumer = &rolled.consumer_fence;
    let mut intent = qualified.cleanup_intent.clone();
    intent.wait_id = BotWaitId::mint();
    intent.decision_id = DecisionId::mint();
    intent.origin_attempt = consumer.attempt;
    intent.origin_generation = consumer.generation;
    intent.origin_session = consumer.session;
    intent.origin_turn = consumer.turn.unwrap();
    let key = intent.key().unwrap();
    qualified.key = key.clone();
    qualified.cleanup_intent = intent.clone();
    let reply = repo
        .enroll_bot_wait_cleanup(
            owner,
            &EnrollBotWaitCleanup {
                key: key.clone(),
                cleanup_intent: intent,
            },
            &qualified,
            at(),
        )
        .unwrap();
    assert!(matches!(reply, BotWaitEnrollmentReply::Enrolled { key: stored, .. } if stored == key));
    // The decision's rollover is accepted over the child wait's consumer too.
    card(&repo, &key, attempt);
    let origin = as_origin(consumer);
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
                receipt_digest: DecisionDigest::from_bytes([8; 32]),
            },
        },
    };
    let decided = repo
        .rollover_bot_decision_replay(owner, &key, &resume, consumer, None, at())
        .unwrap();
    assert_eq!(decided.consumer_fence.generation.get(), 3);
    assert_eq!(rows(&repo), 1);
    assert_eq!(
        repo.connection
            .query_row("SELECT count(*) FROM bot_child_replay_rollovers", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
