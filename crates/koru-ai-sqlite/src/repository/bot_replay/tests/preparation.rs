// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::{BotError, BotSubmissionIntent};
use koru_ai_core::{AgentAnswer, AgentAnswerValue, AgentQuestion, AnswersRecord, QuestionsRecord};
mod context;

fn fresh() -> (Repository, Bot, PendingTurn, BotSubmissionIntent) {
    let mut repo = Repository::in_memory().unwrap();
    let (bot, turn, submission) = fixture(&mut repo);
    repo.connection
        .execute(
            "DELETE FROM bot_command_receipts WHERE request_id=?1",
            [submission.provenance.submission.to_storage()],
        )
        .unwrap();
    (
        repo,
        bot,
        turn,
        BotSubmissionIntent {
            version: 1,
            submission,
        },
    )
}

#[test]
fn shared_turn_and_library_capture_commit_or_rollback_together() {
    use koru_ai_core::bots::{BotCapturedMemory, BotLibraryCapture};
    let (mut repo, bot, turn, intent) = fresh();
    let prepared = BotChatPreparation::Turn(turn.clone());
    let capture = BotLibraryCapture {
        version: 1,
        owner: koru_ai_core::bots::BotOwner {
            user_scope: bot.user_scope,
            device: bot.owner_device,
        },
        bot: bot.id,
        submission: intent.submission.provenance.submission,
        config_revision: bot.revision,
        knowledge: vec![],
        memory: BotCapturedMemory::Absent {},
        index_digest: [0; 32],
    };
    repo.connection.execute_batch("CREATE TRIGGER reject_library_turn BEFORE INSERT ON bot_run_replay BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        repo.prepare_bot_chat_with_library(
            &prepared,
            &intent,
            &capture,
            &"a".repeat(64),
            bot.revision,
            at(1)
        )
        .is_err()
    );
    assert!(repo.messages(bot.main_chat).unwrap().is_empty());
    assert!(repo.attempt(turn.attempt.id).unwrap().is_none());
    assert!(
        repo.bot(bot.id)
            .unwrap()
            .unwrap()
            .folder_lock_request
            .is_none()
    );
    assert!(
        repo.bot_library_capture(capture.owner, bot.id, capture.submission)
            .unwrap()
            .is_none()
    );
    assert!(
        repo.bot_admission_receipt(capture.owner, bot.id, capture.submission)
            .unwrap()
            .is_none()
    );
    repo.connection
        .execute_batch("DROP TRIGGER reject_library_turn")
        .unwrap();
    repo.prepare_bot_chat_with_library(
        &prepared,
        &intent,
        &capture,
        &"a".repeat(64),
        bot.revision,
        at(2),
    )
    .unwrap();
    assert_eq!(repo.messages(bot.main_chat).unwrap().len(), 2);
    assert_eq!(
        repo.bot_library_capture(capture.owner, bot.id, capture.submission)
            .unwrap(),
        Some(capture.clone())
    );
    assert_eq!(
        repo.prepare_bot_chat_with_library(
            &prepared,
            &intent,
            &capture,
            &"a".repeat(64),
            bot.revision,
            at(3)
        )
        .unwrap()
        .id,
        turn.attempt.id
    );
    assert!(repo.delete_chat(bot.main_chat).is_err());
    let ordinary = chat(ChatId::new(), 1);
    repo.create_chat(&ordinary).unwrap();
    assert!(repo.delete_chats(&[ordinary.id, bot.main_chat]).is_err());
    assert!(repo.chat(ordinary.id).unwrap().is_some());
    assert_eq!(repo.clear_history().unwrap(), 1);
    repo.clear_usage().unwrap();
    assert!(repo.attempt(turn.attempt.id).unwrap().is_some());
    assert_eq!(repo.messages(bot.main_chat).unwrap().len(), 2);
    assert_eq!(
        repo.bot_library_capture(capture.owner, bot.id, capture.submission)
            .unwrap(),
        Some(capture)
    );
}

#[test]
fn one_transaction_rolls_back_folder_receipt_messages_and_association_then_retries() {
    let (mut repo, bot, turn, intent) = fresh();
    let prepared = BotChatPreparation::Turn(turn.clone());
    repo.connection.execute_batch("CREATE TRIGGER reject_last BEFORE INSERT ON bot_run_replay BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        repo.prepare_bot_chat(&prepared, &intent, &"a".repeat(64), 1, at(1))
            .is_err()
    );
    assert!(repo.messages(bot.main_chat).unwrap().is_empty());
    assert!(repo.attempt(turn.attempt.id).unwrap().is_none());
    assert!(
        repo.bot(bot.id)
            .unwrap()
            .unwrap()
            .folder_lock_request
            .is_none()
    );
    assert!(
        crate::repository::bots::admission::read_bot_submission_on(
            &repo.connection,
            bot.user_scope,
            intent.submission.provenance.submission
        )
        .unwrap()
        .is_none()
    );
    repo.connection
        .execute_batch("DROP TRIGGER reject_last")
        .unwrap();
    repo.prepare_bot_chat(&prepared, &intent, &"a".repeat(64), 1, at(2))
        .unwrap();
    assert_eq!(repo.messages(bot.main_chat).unwrap().len(), 2);
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_lock_request,
        Some(intent.submission.provenance.submission)
    );
    repo.connection
        .execute(
            "UPDATE bots SET revision=2,policy_revision=2,paused=1 WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.prepare_bot_chat(&prepared, &intent, &"a".repeat(64), 1, at(3))
            .unwrap()
            .id,
        turn.attempt.id
    );
    let mut changed = intent.clone();
    changed.submission.input.text.push_str(" changed");
    assert!(matches!(
        repo.prepare_bot_chat(&prepared, &changed, &"a".repeat(64), 1, at(4)),
        Err(BotPreparationError::Admission(BotError::Conflict))
    ));
    assert_eq!(repo.messages(bot.main_chat).unwrap().len(), 2);
}

fn next(previous: &PendingTurn, submission: &BotSubmission) -> (PendingRetry, BotSubmissionIntent) {
    let next = turn(previous.user_message.chat_id, Uuid::new_v4(), 4);
    let mut retry = PendingRetry {
        assistant_message: next.assistant_message,
        attempt: next.attempt,
    };
    retry.assistant_message.ordinal = 2;
    retry.assistant_message.origin_message_id = Some(previous.assistant_message.id);
    let mut submission = submission.clone();
    submission.provenance.continued_from = Some(submission.request.run);
    submission.request.run = RunId::mint();
    submission.provenance.submission = BotRequestId::mint();
    submission.provenance.ai_attempt_id = Some(retry.attempt.id.as_uuid());
    submission.provenance.assistant_message_id = Some(retry.assistant_message.id.as_uuid());
    submission.input.idempotency_key = delegate::IdempotencyKey::mint();
    (
        retry,
        BotSubmissionIntent {
            version: 1,
            submission,
        },
    )
}

fn admitted() -> (Repository, PendingTurn, BotSubmission, BotExecutionFence) {
    let (mut repo, _, turn, intent) = fresh();
    repo.prepare_bot_chat(
        &BotChatPreparation::Turn(turn.clone()),
        &intent,
        &"0".repeat(64),
        1,
        at(1),
    )
    .unwrap();
    let submission = intent.submission;
    let fence = BotExecutionFence {
        owner: submission.provenance.owner,
        bot: submission.provenance.bot,
        run: submission.request.run,
        attempt: RunAttemptId::mint(),
        generation: AttemptGeneration::FIRST,
        session: SessionId::mint(),
        turn: Some(AgentTurnId::mint()),
    };
    repo.bind_bot_execution(turn.attempt.id, &fence, at(2))
        .unwrap();
    let p = &submission.provenance;
    repo.acknowledge_bot_submission(
        koru_ai_core::bots::BotOwner {
            user_scope: p.owner.user_scope,
            device: p.owner.device,
        },
        p.bot,
        p.submission,
        &delegate::BotAdmissionView {
            key: delegate::BotSubmissionKey {
                owner: p.owner,
                bot: p.bot,
                request: p.submission,
            },
            run: fence.run,
            state: delegate::BotAdmissionState::Executing,
            queue_sequence: 1,
            fence: Some(fence.clone()),
        },
        at(2).millis(),
    )
    .unwrap();
    (repo, turn, submission, fence)
}

#[test]
fn retry_is_assistant_only_and_bound_to_failed_prior_run() {
    let (mut repo, previous, submission, fence) = admitted();
    repo.apply_bot_replay(
        &batch(
            &previous,
            &fence,
            None,
            vec![event(AiEvent::Failed {
                attempt_id: previous.attempt.id,
                error: AiError::new(AiErrorClass::Provider, "Fixture", true),
                at: at(3),
            })],
        ),
        at(3),
    )
    .unwrap();
    let (retry, intent) = next(&previous, &submission);
    let mut wrong = intent.clone();
    wrong.submission.provenance.continued_from = Some(RunId::mint());
    let prep = BotChatPreparation::Retry {
        retry: retry.clone(),
        prior_attempt: previous.attempt.id,
    };
    assert!(
        repo.prepare_bot_chat(&prep, &wrong, &"b".repeat(64), 1, at(4))
            .is_err()
    );
    repo.prepare_bot_chat(&prep, &intent, &"b".repeat(64), 1, at(4))
        .unwrap();
    repo.prepare_bot_chat(&prep, &intent, &"b".repeat(64), 1, at(5))
        .unwrap();
    let messages = repo.messages(previous.user_message.chat_id).unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages.iter().filter(|m| m.role == ChatRole::User).count(),
        1
    );
    assert_eq!(
        messages[2].origin_message_id,
        Some(previous.assistant_message.id)
    );
}

fn question_fixture() -> (
    Repository,
    PendingTurn,
    BotSubmission,
    BotQuestionContinuation,
) {
    let (mut repo, previous, submission, fence) = admitted();
    let mut row = tool(&previous);
    let source = BotToolSource {
        kind: BotToolSourceKind::Questions,
        id: "question-source".into(),
    };
    row.status = ToolActivityStatus::Proposed;
    row.approval = ToolApproval::Pending;
    row.started_at = None;
    row.tool_name = AgentToolKind::Approval.as_token().into();
    row.agent_input = QuestionsRecord::of(&[AgentQuestion {
        id: "choice".into(),
        header: "Choose".into(),
        question: "Which?".into(),
        options: vec![],
        allow_other: true,
        secret: false,
    }])
    .answered_by_next_message(true)
    .encode();
    let mut done = event(AiEvent::Completed {
        attempt_id: previous.attempt.id,
        stop_reason: None,
        at: at(3),
    });
    if let BotReplayEffect::Event {
        terminal_cleanup: Some(cleanup),
        ..
    } = &mut done
    {
        cleanup.retained_question_ids.push(row.id);
    }
    repo.apply_bot_replay(
        &batch(
            &previous,
            &fence,
            None,
            vec![
                BotReplayEffect::CreateTool {
                    source: source.clone(),
                    activity: row.clone(),
                },
                done,
            ],
        ),
        at(3),
    )
    .unwrap();
    let question = BotQuestionContinuation {
        prior_attempt: previous.attempt.id,
        invocation: row.id,
        source,
        answers: AnswersRecord {
            answers: vec![AgentAnswer {
                question_id: "choice".into(),
                value: AgentAnswerValue::Text {
                    text: "Answer preserved ".repeat(50),
                },
            }],
        },
    };
    (repo, previous, submission, question)
}

#[test]
fn terminal_answers_close_only_bound_card_with_new_run_atomically_and_losslessly() {
    let (mut repo, previous, submission, question) = question_fixture();
    let (mut retry, mut intent) = next(&previous, &submission);
    retry.assistant_message.origin_message_id = None;
    intent.submission.input.text = question.answers.encode_lossless().unwrap();
    let prep = BotChatPreparation::AnsweredContinuation {
        retry: retry.clone(),
        question: question.clone(),
    };
    repo.connection.execute_batch("CREATE TRIGGER reject_answer BEFORE UPDATE OF agent_output ON tool_invocations BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        repo.prepare_bot_chat(&prep, &intent, &"c".repeat(64), 1, at(4))
            .is_err()
    );
    assert!(repo.attempt(retry.attempt.id).unwrap().is_none());
    assert!(
        crate::repository::bots::admission::read_bot_submission_on(
            &repo.connection,
            submission.provenance.owner.user_scope,
            intent.submission.provenance.submission
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        repo.tool_activity(question.invocation)
            .unwrap()
            .unwrap()
            .approval,
        ToolApproval::Pending
    );
    repo.connection
        .execute_batch("DROP TRIGGER reject_answer")
        .unwrap();
    repo.prepare_bot_chat(&prep, &intent, &"c".repeat(64), 1, at(4))
        .unwrap();
    repo.prepare_bot_chat(&prep, &intent, &"c".repeat(64), 1, at(5))
        .unwrap();
    let row = repo.tool_activity(question.invocation).unwrap().unwrap();
    assert_eq!(row.status, ToolActivityStatus::Complete);
    assert_eq!(
        AnswersRecord::decode(row.agent_output.as_deref().unwrap()),
        Some(question.answers.clone())
    );
    assert_eq!(
        repo.messages(previous.user_message.chat_id).unwrap().len(),
        3
    );
    let (retry2, mut second) = next(&previous, &submission);
    second.submission.input.text = intent.submission.input.text.clone();
    let secondprep = BotChatPreparation::AnsweredContinuation {
        retry: retry2,
        question,
    };
    assert!(
        repo.prepare_bot_chat(&secondprep, &second, &"d".repeat(64), 1, at(6))
            .is_err()
    );
}

#[test]
fn answers_refuse_foreign_source_unwithheld_secret_and_oversize_before_admission() {
    for mode in 0..3 {
        let (mut repo, previous, submission, mut question) = question_fixture();
        let (retry, mut intent) = next(&previous, &submission);
        if mode == 0 {
            question.source.id = "foreign".into();
        }
        if mode == 1 {
            let mut row = repo.tool_activity(question.invocation).unwrap().unwrap();
            let mut record = QuestionsRecord::decode(row.agent_input.as_deref().unwrap()).unwrap();
            record.questions[0].secret = true;
            row.agent_input = record.encode();
            mutations::advance_agent_activity_on(&repo.connection, &row).unwrap();
        }
        if mode == 2 {
            question.answers.answers[0].value = AgentAnswerValue::Text {
                text: "x".repeat(5000),
            };
        }
        intent.submission.input.text = question
            .answers
            .encode_lossless()
            .unwrap_or_else(|| "oversized".into());
        let prep = BotChatPreparation::AnsweredContinuation { retry, question };
        assert!(
            repo.prepare_bot_chat(&prep, &intent, &"e".repeat(64), 1, at(4))
                .is_err()
        );
        assert_eq!(
            repo.messages(previous.user_message.chat_id).unwrap().len(),
            2
        );
    }
}

#[test]
fn failed_answer_continuation_retries_its_actual_task_but_never_withheld_secrets() {
    for secret in [false, true] {
        let (mut repo, previous, submission, mut question) = question_fixture();
        if secret {
            let mut row = repo.tool_activity(question.invocation).unwrap().unwrap();
            let mut record = QuestionsRecord::decode(row.agent_input.as_deref().unwrap()).unwrap();
            record.questions[0].secret = true;
            row.agent_input = record.encode();
            mutations::advance_agent_activity_on(&repo.connection, &row).unwrap();
            question.answers.answers[0].value = AgentAnswerValue::Withheld;
        }
        let (retry, mut intent) = next(&previous, &submission);
        intent.submission.input.text = question.answers.encode_lossless().unwrap();
        let prep = BotChatPreparation::AnsweredContinuation {
            retry: retry.clone(),
            question,
        };
        repo.prepare_bot_chat(&prep, &intent, &"f".repeat(64), 1, at(4))
            .unwrap();
        let p = &intent.submission.provenance;
        let fence = BotExecutionFence {
            owner: p.owner,
            bot: p.bot,
            run: intent.submission.request.run,
            attempt: RunAttemptId::mint(),
            generation: AttemptGeneration::FIRST,
            session: SessionId::mint(),
            turn: Some(AgentTurnId::mint()),
        };
        repo.acknowledge_bot_submission(
            koru_ai_core::bots::BotOwner {
                user_scope: p.owner.user_scope,
                device: p.owner.device,
            },
            p.bot,
            p.submission,
            &delegate::BotAdmissionView {
                key: delegate::BotSubmissionKey {
                    owner: p.owner,
                    bot: p.bot,
                    request: p.submission,
                },
                run: fence.run,
                state: delegate::BotAdmissionState::Executing,
                queue_sequence: 2,
                fence: Some(fence.clone()),
            },
            at(5).millis(),
        )
        .unwrap();
        repo.bind_bot_execution(retry.attempt.id, &fence, at(5))
            .unwrap();
        let failed = PendingTurn {
            user_message: previous.user_message.clone(),
            assistant_message: retry.assistant_message.clone(),
            attempt: retry.attempt.clone(),
            attachment_ids: vec![],
        };
        repo.apply_bot_replay(
            &batch(
                &failed,
                &fence,
                None,
                vec![event(AiEvent::Failed {
                    attempt_id: retry.attempt.id,
                    error: AiError::new(AiErrorClass::Provider, "Fixture", true),
                    at: at(6),
                })],
            ),
            at(6),
        )
        .unwrap();
        let (mut retry2, intent2) = next(&failed, &intent.submission);
        retry2.assistant_message.ordinal = 3;
        let retry2 = BotChatPreparation::Retry {
            retry: retry2,
            prior_attempt: retry.attempt.id,
        };
        let outcome = repo.prepare_bot_chat(&retry2, &intent2, &"1".repeat(64), 1, at(7));
        if secret {
            assert!(outcome.is_err());
            assert_eq!(
                repo.messages(previous.user_message.chat_id).unwrap().len(),
                3
            );
        } else {
            outcome.unwrap();
            assert_eq!(
                repo.messages(previous.user_message.chat_id).unwrap().len(),
                4
            );
            assert_ne!(
                intent2.submission.input.text,
                previous.user_message.markdown
            );
            repo.prepare_bot_chat(&retry2, &intent2, &"1".repeat(64), 1, at(8))
                .unwrap();
        }
    }
}

#[test]
fn retry_refuses_a_corrupt_prior_link_to_another_valid_same_text_receipt() {
    let (mut repo, previous, submission, fence) = admitted();
    repo.apply_bot_replay(
        &batch(
            &previous,
            &fence,
            None,
            vec![event(AiEvent::Failed {
                attempt_id: previous.attempt.id,
                error: AiError::new(AiErrorClass::Provider, "Fixture", true),
                at: at(3),
            })],
        ),
        at(3),
    )
    .unwrap();
    let (retry, intent) = next(&previous, &submission);
    let mut foreign = intent.clone();
    foreign.submission.provenance.submission = BotRequestId::mint();
    foreign.submission.request.run = RunId::mint();
    foreign.submission.provenance.ai_attempt_id = Some(Uuid::new_v4());
    foreign.submission.provenance.assistant_message_id = Some(Uuid::new_v4());
    repo.prepare_bot_submission(&foreign, &"2".repeat(64), 1, at(4).millis())
        .unwrap();
    repo.connection
        .execute(
            "UPDATE bot_chat_runs SET submission_id=?2 WHERE ai_attempt_id=?1",
            params![
                previous.attempt.id.to_storage(),
                foreign.submission.provenance.submission.to_storage()
            ],
        )
        .unwrap();
    assert!(
        repo.prepare_bot_chat(
            &BotChatPreparation::Retry {
                retry,
                prior_attempt: previous.attempt.id
            },
            &intent,
            &"3".repeat(64),
            1,
            at(5)
        )
        .is_err()
    );
    assert_eq!(
        repo.messages(previous.user_message.chat_id).unwrap().len(),
        2
    );
}
