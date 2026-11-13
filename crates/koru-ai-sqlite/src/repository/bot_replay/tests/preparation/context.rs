// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::{ContextCommit, MessageDocumentRef, RequestMeasurements, StoreRevision};

fn add_pin(repo: &mut Repository, submission: &mut BotSubmission, document: DocumentId) {
    submission.request.context = vec![delegate::RunContextItem::Document {
        workspace: submission.workspace,
        document,
        version: delegate::DocumentVersionId::new(),
    }];
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET submission_json=?1 WHERE request_id=?2",
            params![
                serde_json::json!({"version":1,"submission":submission}).to_string(),
                submission.provenance.submission.to_storage()
            ],
        )
        .unwrap();
}
fn context(turn: &PendingTurn, document: DocumentId) -> ContextCommit {
    ContextCommit {
        attempt_id: turn.attempt.id,
        started_at: at(2),
        completed_at: at(2),
        measurements: RequestMeasurements {
            context_document_count: 1,
            ..Default::default()
        },
        pinned_documents: vec![PinnedDocument {
            chat_id: turn.user_message.chat_id,
            document_id: document,
            display_title: "Original title".into(),
            pinned: true,
            first_used_at: at(2),
            last_used_at: at(2),
        }],
        document_refs: vec![MessageDocumentRef {
            message_id: turn.user_message.id,
            document_id: document,
            relation: DocumentRelation::Context,
            display_title: "Original title".into(),
            source_revision: Some(StoreRevision(vec![1, 2, 3])),
            used_at: at(2),
        }],
        message_block_refs: vec![],
        replace_pinned_documents: false,
    }
}
fn stored_ref(repo: &Repository, commit: &ContextCommit) -> (String, Vec<u8>, i64) {
    repo.connection.query_row("SELECT display_title,source_revision,used_at_ms FROM message_document_refs WHERE message_id=?1 AND document_id=?2 AND relation='context'",
        params![commit.document_refs[0].message_id.to_storage(),commit.document_refs[0].document_id.to_storage()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap()
}

#[test]
fn bot_context_keeps_first_reference_when_retry_captures_new_history() {
    let (mut repo, previous, mut submission, fence) = admitted();
    let document = DocumentId::new();
    add_pin(&mut repo, &mut submission, document);
    let first = context(&previous, document);
    repo.commit_context(&first).unwrap();
    let saved = stored_ref(&repo, &first);
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
    let (retry, mut intent) = next(&previous, &submission);
    intent.submission.request.context = vec![delegate::RunContextItem::Document {
        workspace: submission.workspace,
        document,
        version: delegate::DocumentVersionId::new(),
    }];
    let prep = BotChatPreparation::Retry {
        retry: retry.clone(),
        prior_attempt: previous.attempt.id,
    };
    repo.prepare_bot_chat(&prep, &intent, &"b".repeat(64), 1, at(4))
        .unwrap();
    let mut retried = first.clone();
    retried.attempt_id = retry.attempt.id;
    retried.started_at = at(5);
    retried.completed_at = at(5);
    retried.document_refs[0].display_title = "New title".into();
    retried.document_refs[0].source_revision = Some(StoreRevision(vec![4, 5, 6]));
    retried.document_refs[0].used_at = at(5);
    repo.commit_context(&retried).unwrap();
    repo.commit_context(&retried).unwrap();
    assert_eq!(stored_ref(&repo, &first), saved);
    assert_eq!(
        repo.bot_submission_for_attempt(retry.attempt.id)
            .unwrap()
            .unwrap()
            .intent,
        intent
    );
    assert_ne!(
        intent.submission.request.context,
        submission.request.context
    );
}

#[test]
fn bot_context_rejects_foreign_unpinned_and_missing_association_atomically() {
    for case in 0..8 {
        let (mut repo, previous, mut submission, _) = admitted();
        let document = DocumentId::new();
        add_pin(&mut repo, &mut submission, document);
        let mut commit = context(&previous, document);
        match case {
            0 => commit.document_refs[0].message_id = previous.assistant_message.id,
            1 => {
                let id = DocumentId::new();
                commit.document_refs[0].document_id = id;
                commit.pinned_documents[0].document_id = id;
            }
            2 => commit.pinned_documents[0].chat_id = ChatId::new(),
            3 => commit.pinned_documents.clear(),
            4 => commit.document_refs[0].source_revision = None,
            5 => commit.document_refs[0].source_revision = Some(StoreRevision(vec![])),
            6 => {
                repo.connection
                    .execute(
                        "DELETE FROM bot_run_replay WHERE ai_attempt_id=?1",
                        [previous.attempt.id.to_storage()],
                    )
                    .unwrap();
                repo.connection
                    .execute(
                        "DELETE FROM bot_chat_runs WHERE ai_attempt_id=?1",
                        [previous.attempt.id.to_storage()],
                    )
                    .unwrap();
            }
            7 => {
                repo.connection
                    .execute(
                        "UPDATE bot_command_receipts SET submission_json='{}' WHERE request_id=?1",
                        [submission.provenance.submission.to_storage()],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(repo.commit_context(&commit).is_err(), "case {case}");
        assert!(
            repo.pinned_documents(previous.user_message.chat_id)
                .unwrap()
                .is_empty()
        );
        let count: i64 = repo
            .connection
            .query_row("SELECT count(*) FROM message_document_refs", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
        assert!(
            repo.attempt(previous.attempt.id)
                .unwrap()
                .unwrap()
                .context_completed_at
                .is_none()
        );
    }
}

#[test]
fn ordinary_context_still_requires_store_revision() {
    let mut repo = Repository::in_memory().unwrap();
    let chat = chat(ChatId::new(), 0);
    repo.create_chat(&chat).unwrap();
    let turn = turn(chat.id, Uuid::new_v4(), 1);
    repo.begin_turn(&turn).unwrap();
    let mut commit = context(&turn, DocumentId::new());
    commit.document_refs[0].source_revision = None;
    assert!(repo.commit_context(&commit).is_err());
    assert!(repo.pinned_documents(chat.id).unwrap().is_empty());
    commit.document_refs[0].source_revision = Some(StoreRevision(vec![1, 2, 3]));
    repo.commit_context(&commit).unwrap();
}

#[test]
fn answered_context_and_its_retry_use_exact_assistant_anchors() {
    let (mut repo, previous, submission, question) = question_fixture();
    let document = DocumentId::new();
    let (mut answer, mut intent) = next(&previous, &submission);
    answer.assistant_message.origin_message_id = None;
    intent.submission.input.text = question.answers.encode_lossless().unwrap();
    intent.submission.request.context = vec![delegate::RunContextItem::Document {
        workspace: submission.workspace,
        document,
        version: delegate::DocumentVersionId::new(),
    }];
    repo.prepare_bot_chat(
        &BotChatPreparation::AnsweredContinuation {
            retry: answer.clone(),
            question: question.clone(),
        },
        &intent,
        &"c".repeat(64),
        1,
        at(4),
    )
    .unwrap();
    let mut answer_context = context(&previous, document);
    answer_context.attempt_id = answer.attempt.id;
    answer_context.started_at = at(5);
    answer_context.completed_at = at(5);
    answer_context.document_refs[0].message_id = previous.assistant_message.id;
    repo.commit_context(&answer_context).unwrap();
    let saved = stored_ref(&repo, &answer_context);
    let fence = BotExecutionFence {
        owner: submission.provenance.owner,
        bot: submission.provenance.bot,
        run: intent.submission.request.run,
        attempt: RunAttemptId::mint(),
        generation: AttemptGeneration::FIRST,
        session: SessionId::mint(),
        turn: Some(AgentTurnId::mint()),
    };
    repo.bind_bot_execution(answer.attempt.id, &fence, at(5))
        .unwrap();
    let p = &intent.submission.provenance;
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
    let answer_turn = PendingTurn {
        user_message: previous.user_message.clone(),
        assistant_message: answer.assistant_message.clone(),
        attempt: answer.attempt.clone(),
        attachment_ids: vec![],
    };
    repo.apply_bot_replay(
        &batch(
            &answer_turn,
            &fence,
            None,
            vec![event(AiEvent::Failed {
                attempt_id: answer.attempt.id,
                error: AiError::new(AiErrorClass::Provider, "fixture", true),
                at: at(6),
            })],
        ),
        at(6),
    )
    .unwrap();
    let (mut retry, mut retried) = next(&answer_turn, &intent.submission);
    retry.assistant_message.ordinal = 3;
    retry.assistant_message.created_at = at(8);
    retry.attempt.queued_at = at(8);
    retried.submission.request.context = vec![delegate::RunContextItem::Document {
        workspace: submission.workspace,
        document,
        version: delegate::DocumentVersionId::new(),
    }];
    repo.prepare_bot_chat(
        &BotChatPreparation::Retry {
            retry: retry.clone(),
            prior_attempt: answer.attempt.id,
        },
        &retried,
        &"d".repeat(64),
        1,
        at(8),
    )
    .unwrap();
    let mut retry_context = answer_context.clone();
    retry_context.attempt_id = retry.attempt.id;
    retry_context.started_at = at(9);
    retry_context.completed_at = at(9);
    retry_context.document_refs[0].message_id = answer.assistant_message.id;
    retry_context.document_refs[0].source_revision = Some(StoreRevision(vec![7, 8, 9]));
    repo.commit_context(&retry_context).unwrap();
    assert_eq!(stored_ref(&repo, &answer_context), saved);
    assert_eq!(stored_ref(&repo, &retry_context).1, vec![7, 8, 9]);
    assert_eq!(
        repo.messages(previous.user_message.chat_id)
            .unwrap()
            .iter()
            .filter(|message| message.role == ChatRole::User)
            .count(),
        1
    );
}

#[test]
fn answered_context_refuses_changed_foreign_or_unanswered_question_evidence() {
    for case in 0..3 {
        let (mut repo, previous, submission, question) = question_fixture();
        let document = DocumentId::new();
        let (mut answer, mut intent) = next(&previous, &submission);
        answer.assistant_message.origin_message_id = None;
        intent.submission.input.text = question.answers.encode_lossless().unwrap();
        intent.submission.request.context = vec![delegate::RunContextItem::Document {
            workspace: submission.workspace,
            document,
            version: delegate::DocumentVersionId::new(),
        }];
        repo.prepare_bot_chat(
            &BotChatPreparation::AnsweredContinuation {
                retry: answer.clone(),
                question: question.clone(),
            },
            &intent,
            &"c".repeat(64),
            1,
            at(4),
        )
        .unwrap();
        match case {
            0 => {
                repo.connection
                    .execute(
                        "UPDATE tool_invocations SET agent_output='changed' WHERE id=?1",
                        [question.invocation.to_storage()],
                    )
                    .unwrap();
            }
            1 => {
                repo.connection
                    .execute(
                        "UPDATE tool_invocations SET attempt_id=?1 WHERE id=?2",
                        params![
                            answer.attempt.id.to_storage(),
                            question.invocation.to_storage()
                        ],
                    )
                    .unwrap();
            }
            2 => {
                repo.connection
                    .execute(
                        "UPDATE tool_invocations SET approval='pending' WHERE id=?1",
                        [question.invocation.to_storage()],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let mut commit = context(&previous, document);
        commit.attempt_id = answer.attempt.id;
        commit.started_at = at(5);
        commit.completed_at = at(5);
        commit.document_refs[0].message_id = previous.assistant_message.id;
        assert!(repo.commit_context(&commit).is_err(), "case {case}");
        assert!(
            repo.pinned_documents(previous.user_message.chat_id)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn bot_context_refuses_malformed_existing_revision_without_overwriting_it() {
    for value in ["x''", "zeroblob(65537)", "'wrong type'"] {
        let (mut repo, previous, mut submission, _) = admitted();
        let document = DocumentId::new();
        add_pin(&mut repo, &mut submission, document);
        let commit = context(&previous, document);
        repo.commit_context(&commit).unwrap();
        repo.connection
            .execute(
                &format!("UPDATE message_document_refs SET source_revision={value}"),
                [],
            )
            .unwrap();
        assert!(repo.commit_context(&commit).is_err(), "value {value}");
    }
}

#[test]
fn answered_question_candidates_skip_ordinary_and_malformed_matching_outputs() {
    let (mut repo, previous, _, question) = question_fixture();
    let mut valid = repo.tool_activity(question.invocation).unwrap().unwrap();
    valid.status = ToolActivityStatus::Proposed;
    valid.approval = ToolApproval::Approved;
    valid.agent_output = Some(question.answers.encode_lossless().unwrap());
    let input = valid.agent_output.clone().unwrap();
    for (index, mode) in [0, 1, 2].into_iter().enumerate() {
        let mut row = valid.clone();
        row.id =
            ToolInvocationId::parse(&format!("00000000-0000-4000-8000-{:012}", index + 1)).unwrap();
        if mode == 0 {
            let mut questions =
                QuestionsRecord::decode(row.agent_input.as_deref().unwrap()).unwrap();
            questions.next_message = false;
            row.agent_input = questions.encode();
        } else if mode == 1 {
            row.agent_input = Some("{malformed".into());
        }
        repo.create_tool_activity(&row).unwrap();
        repo.connection.execute("UPDATE tool_invocations SET status='complete',started_at_ms=proposed_at_ms,completed_at_ms=proposed_at_ms,bot_source_kind='questions',bot_source_id=?1 WHERE id=?2",params![format!("candidate-{index}"),row.id.to_storage()]).unwrap();
    }
    super::super::super::context::validate_answered_question(
        &repo.connection,
        &previous.attempt.id.to_storage(),
        previous.user_message.chat_id,
        &input,
    )
    .unwrap();
}

#[test]
fn answered_question_candidates_refuse_more_than_64_matching_cards() {
    let (mut repo, previous, _, question) = question_fixture();
    let mut row = repo.tool_activity(question.invocation).unwrap().unwrap();
    row.status = ToolActivityStatus::Proposed;
    row.approval = ToolApproval::Approved;
    row.agent_output = Some(question.answers.encode_lossless().unwrap());
    for index in 0..65 {
        row.id = ToolInvocationId::new();
        repo.create_tool_activity(&row).unwrap();
        repo.connection.execute("UPDATE tool_invocations SET status='complete',started_at_ms=proposed_at_ms,completed_at_ms=proposed_at_ms,bot_source_kind='questions',bot_source_id=?1 WHERE id=?2",params![format!("overflow-{index}"),row.id.to_storage()]).unwrap();
    }
    let error = super::super::super::context::validate_answered_question(
        &repo.connection,
        &previous.attempt.id.to_storage(),
        previous.user_message.chat_id,
        row.agent_output.as_deref().unwrap(),
    )
    .unwrap_err();
    assert_eq!(error.class, AiErrorClass::ContextTooLarge);
}
