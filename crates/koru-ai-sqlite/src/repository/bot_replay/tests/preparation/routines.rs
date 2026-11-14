// SPDX-License-Identifier: MPL-2.0
//! Synthetic canonical history exercises refusal, not Routine claim authority.
use super::*;

fn mark_routine(repo: &Repository, mut submission: BotSubmission) -> BotSubmission {
    let routine = delegate::RoutineProvenance {
        schedule: delegate::ScheduleId::mint(),
        occurrence_ms: 60_000,
        definition: delegate::RoutineDefinitionIdentity {
            revision: 1,
            digest: delegate::RoutineDigest32::from_bytes([7; 32]),
            owner_handoff: None,
        },
        grant_revision: 1,
    };
    submission.request.origin = delegate::RunOrigin::Scheduled {
        schedule: routine.schedule,
        occurrence_ms: routine.occurrence_ms,
        revision: 1,
    };
    submission.provenance.routine = Some(routine);
    let intent = BotSubmissionIntent {
        version: 2,
        submission: submission.clone(),
    };
    intent
        .validate_for_kind(koru_ai_core::bots::BotOperationKind::RoutineOccurrence)
        .unwrap();
    repo.connection.execute(
        "UPDATE bot_command_receipts SET command_kind='routine_occurrence',submission_version=2,submission_json=?1 WHERE request_id=?2",
        params![serde_json::to_string(&intent).unwrap(), submission.provenance.submission.to_storage()],
    ).unwrap();
    submission
}

fn capture(intent: &BotSubmissionIntent) -> koru_ai_core::bots::BotLibraryCapture {
    let p = &intent.submission.provenance;
    koru_ai_core::bots::BotLibraryCapture {
        version: 1,
        owner: koru_ai_core::bots::BotOwner {
            user_scope: p.owner.user_scope,
            device: p.owner.device,
        },
        bot: p.bot,
        submission: p.submission,
        config_revision: p.config_revision,
        knowledge: vec![],
        memory: koru_ai_core::bots::BotCapturedMemory::Absent {},
        index_digest: [0; 32],
    }
}

#[test]
fn routine_retry_transaction_refuses_before_admission_capture_or_transcript_changes() {
    let (mut repo, previous, submission, _) = admitted();
    let (retry, intent) = next(&previous, &submission);
    mark_routine(&repo, submission.clone());
    let prep = BotChatPreparation::Retry {
        retry: retry.clone(),
        prior_attempt: previous.attempt.id,
    };
    let before = repo.connection.total_changes();
    for library in [false, true] {
        let result = if library {
            repo.prepare_bot_chat_with_library(
                &prep,
                &intent,
                &capture(&intent),
                &"b".repeat(64),
                1,
                at(4),
            )
        } else {
            repo.prepare_bot_chat(&prep, &intent, &"b".repeat(64), 1, at(4))
        };
        assert!(matches!(
            result,
            Err(BotPreparationError::Admission(
                BotError::RoutineRetryUnsupported
            ))
        ));
        assert_eq!(repo.connection.total_changes(), before);
        assert!(repo.attempt(retry.attempt.id).unwrap().is_none());
    }
    assert_eq!(
        repo.messages(previous.user_message.chat_id).unwrap().len(),
        2
    );
}

#[test]
fn routine_terminal_answer_transaction_leaves_question_and_receipts_unchanged() {
    let (mut repo, previous, submission, question) = question_fixture();
    let (mut retry, mut intent) = next(&previous, &submission);
    retry.assistant_message.origin_message_id = None;
    intent.submission.input.text = question.answers.encode_lossless().unwrap();
    mark_routine(&repo, submission);
    let before = repo.connection.total_changes();
    let row = repo.tool_activity(question.invocation).unwrap().unwrap();
    let prep = BotChatPreparation::AnsweredContinuation {
        retry,
        question: question.clone(),
    };
    assert!(matches!(
        repo.prepare_bot_chat_with_library(
            &prep,
            &intent,
            &capture(&intent),
            &"c".repeat(64),
            1,
            at(4)
        ),
        Err(BotPreparationError::Admission(
            BotError::RoutineRetryUnsupported
        ))
    ));
    assert_eq!(repo.connection.total_changes(), before);
    assert_eq!(
        repo.tool_activity(question.invocation).unwrap().unwrap(),
        row
    );
}

#[test]
fn routine_predecessor_does_not_block_immutable_old_retry_replay() {
    let (mut repo, previous, submission, fence) = admitted();
    repo.apply_bot_replay(
        &batch(
            &previous,
            &fence,
            None,
            vec![event(AiEvent::Failed {
                attempt_id: previous.attempt.id,
                error: AiError::new(AiErrorClass::Provider, "fixture", true),
                at: at(3),
            })],
        ),
        at(3),
    )
    .unwrap();
    let (retry, intent) = next(&previous, &submission);
    let prep = BotChatPreparation::Retry {
        retry: retry.clone(),
        prior_attempt: previous.attempt.id,
    };
    repo.prepare_bot_chat(&prep, &intent, &"b".repeat(64), 1, at(4))
        .unwrap();
    mark_routine(&repo, submission);
    let before = repo.connection.total_changes();
    assert_eq!(
        repo.prepare_bot_chat(&prep, &intent, &"b".repeat(64), 99, at(5))
            .unwrap()
            .id,
        retry.attempt.id
    );
    let mut changed = intent.clone();
    changed.submission.input.text.push_str(" changed");
    assert!(matches!(
        repo.prepare_bot_chat(&prep, &changed, &"b".repeat(64), 99, at(5)),
        Err(BotPreparationError::Admission(BotError::Conflict))
    ));
    assert_eq!(repo.connection.total_changes(), before);
}

#[test]
fn new_manual_turn_and_its_interactive_retry_work_in_a_routine_chat() {
    let (mut repo, previous, submission, fence) = admitted();
    repo.apply_bot_replay(
        &batch(
            &previous,
            &fence,
            None,
            vec![event(AiEvent::Failed {
                attempt_id: previous.attempt.id,
                error: AiError::new(AiErrorClass::Provider, "fixture", true),
                at: at(3),
            })],
        ),
        at(3),
    )
    .unwrap();
    mark_routine(&repo, submission.clone());
    let mut manual = turn(previous.user_message.chat_id, Uuid::new_v4(), 4);
    manual.user_message.ordinal = 2;
    manual.user_message.markdown = submission.input.text.clone();
    manual.assistant_message.ordinal = 3;
    let mut intent = BotSubmissionIntent {
        version: 1,
        submission,
    };
    intent.submission.provenance.submission = BotRequestId::mint();
    intent.submission.provenance.ai_attempt_id = Some(manual.attempt.id.as_uuid());
    intent.submission.provenance.assistant_message_id = Some(manual.assistant_message.id.as_uuid());
    intent.submission.request.run = RunId::mint();
    assert!(intent.submission.provenance.routine.is_none());
    assert!(intent.submission.provenance.continued_from.is_none());
    repo.prepare_bot_chat(
        &BotChatPreparation::Turn(manual.clone()),
        &intent,
        &"c".repeat(64),
        1,
        at(4),
    )
    .unwrap();
    repo.connection
        .execute(
            "UPDATE inference_attempts SET status='failed' WHERE id=?1",
            [manual.attempt.id.to_storage()],
        )
        .unwrap();
    repo.connection
        .execute(
            "UPDATE messages SET state='failed' WHERE id=?1",
            [manual.assistant_message.id.to_storage()],
        )
        .unwrap();
    let (mut retry, next_intent) = next(&manual, &intent.submission);
    retry.assistant_message.ordinal = 4;
    repo.prepare_bot_chat(
        &BotChatPreparation::Retry {
            retry: retry.clone(),
            prior_attempt: manual.attempt.id,
        },
        &next_intent,
        &"d".repeat(64),
        1,
        at(5),
    )
    .unwrap();
    let saved = repo
        .bot_submission_for_attempt(retry.attempt.id)
        .unwrap()
        .unwrap();
    assert!(saved.intent.submission.provenance.routine.is_none());
    assert!(matches!(
        saved.intent.submission.request.origin,
        delegate::RunOrigin::Manual
    ));
    assert_eq!(
        saved.intent.submission.provenance.continued_from,
        Some(intent.submission.request.run)
    );
    assert_eq!(
        repo.messages(previous.user_message.chat_id).unwrap().len(),
        5
    );
}

#[test]
fn routine_history_branch_copies_no_execution_authority() {
    let (mut repo, previous, submission, fence) = admitted();
    repo.apply_bot_replay(
        &batch(
            &previous,
            &fence,
            None,
            vec![event(AiEvent::Failed {
                attempt_id: previous.attempt.id,
                error: AiError::new(AiErrorClass::Provider, "fixture", true),
                at: at(3),
            })],
        ),
        at(3),
    )
    .unwrap();
    mark_routine(&repo, submission.clone());
    let bot = repo.bot(submission.provenance.bot).unwrap().unwrap();
    let source = repo
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let destination = ChatId::new();
    let mut policy = repo.chat_tool_policy(bot.main_chat).unwrap().unwrap();
    policy.chat_id = destination;
    let messages = repo
        .messages(bot.main_chat)
        .unwrap()
        .into_iter()
        .map(|mut message| {
            message.origin_message_id = Some(message.id);
            message.id = MessageId::new();
            message.chat_id = destination;
            message
        })
        .collect();
    let branch = BranchCopy {
        chat: Chat {
            id: destination,
            branched_from: Some(BranchOrigin {
                chat_id: bot.main_chat,
                through_message_id: previous.assistant_message.id,
            }),
            ..source
        },
        tool_policy: policy,
        messages,
        blocks: vec![],
        citations: vec![],
        pinned_documents: vec![],
        attachments: vec![],
    };
    let count: u64 = repo
        .connection
        .query_row("SELECT COUNT(*) FROM bot_command_receipts", [], |row| {
            row.get(0)
        })
        .unwrap();
    repo.create_bot_branch(&bot, &branch).unwrap();
    assert_eq!(repo.bot_for_chat(destination).unwrap(), Some(bot.id));
    assert_eq!(repo.messages(destination).unwrap(), branch.messages);
    assert!(repo.attempts_for_chat(destination).unwrap().is_empty());
    assert!(repo.tool_activities(destination).unwrap().is_empty());
    let after: u64 = repo
        .connection
        .query_row("SELECT COUNT(*) FROM bot_command_receipts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(after, count);
}
