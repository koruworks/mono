// SPDX-License-Identifier: MPL-2.0
//! Synthetic retained Routine history proves refusal; it never grants a claim.
use super::*;
use koru_ai_core::AgentToolKind;
use koru_ai_core::bots::replay::BotChatPreparation;

pub(in crate::worker::bots) fn seeded() -> (
    Repository,
    Bot,
    Arc<Authority>,
    CommandContext,
    SendTurnRequest,
    PendingTurn,
    BotSubmissionIntent,
) {
    let (mut repo, authority, create) = fixture();
    let bot = created(&mut repo, authority.clone(), create);
    let chat = repo
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let context = proof(authority.clone(), &bot, chat);
    let request = request(&bot);
    let mut turn = turn(bot.main_chat, request.operation_id, 1);
    turn.user_message.markdown = request.markdown.clone();
    let prepared = context.prepared.as_ref().unwrap();
    let sources = prepared.library_sources.as_ref().unwrap();
    let mut intent = crate::worker::bots::admission::make_library_intent(
        &bot,
        BotRequestId::mint(),
        bot.main_chat,
        bot.home_workspace,
        &BotTask {
            text: request.markdown.clone(),
            documents: vec![],
        },
        None,
        &prepared.pins,
        sources,
        authority.profile,
    )
    .unwrap();
    intent.submission.provenance.ai_attempt_id = Some(turn.attempt.id.as_uuid());
    intent.submission.provenance.assistant_message_id = Some(turn.assistant_message.id.as_uuid());
    let fingerprint = blake3::hash(&serde_json::to_vec(&("send", &request)).unwrap())
        .to_hex()
        .to_string();
    repo.prepare_bot_chat_with_library(
        &BotChatPreparation::Turn(turn.clone()),
        &intent,
        &sources.capture(&intent).unwrap(),
        &fingerprint,
        1,
        Timestamp::now(),
    )
    .unwrap();
    intent.version = 2;
    let routine = d::RoutineProvenance {
        schedule: d::ScheduleId::mint(),
        occurrence_ms: 60_000,
        definition: d::RoutineDefinitionIdentity {
            revision: 1,
            digest: d::RoutineDigest32::from_bytes([8; 32]),
            owner_handoff: None,
        },
        grant_revision: 1,
    };
    intent.submission.request.origin = d::RunOrigin::Scheduled {
        schedule: routine.schedule,
        occurrence_ms: routine.occurrence_ms,
        revision: 1,
    };
    intent.submission.provenance.routine = Some(routine);
    intent
        .validate_for_kind(BotOperationKind::RoutineOccurrence)
        .unwrap();
    let json = serde_json::to_string(&intent).unwrap().replace('\'', "''");
    repo.fixture_execute_batch(&format!(
        "UPDATE bot_command_receipts SET command_kind='routine_occurrence',submission_version=2,submission_json='{json}' WHERE request_id='{}'; UPDATE inference_attempts SET status='failed',stop_reason='original-refusal' WHERE id='{}'; UPDATE messages SET state='failed' WHERE id='{}';",
        intent.submission.provenance.submission, turn.attempt.id, turn.assistant_message.id,
    )).unwrap();
    assert_eq!(
        repo.bot_submission_for_attempt(turn.attempt.id)
            .unwrap()
            .unwrap()
            .intent,
        intent
    );
    (repo, bot, authority, context, request, turn, intent)
}

fn retry(bot: &Bot, turn: &PendingTurn) -> RetryTurnRequest {
    RetryTurnRequest {
        destination_workspace: Some(bot.home_workspace),
        operation_id: Uuid::new_v4(),
        message_id: turn.assistant_message.id,
        surface: AiSurface::Standalone,
        explicit_refs: vec![],
        reasoning_override: None,
        web_search: false,
        replace_pinned_documents: false,
    }
}

#[test]
fn routine_retry_preflight_and_writer_refuse_without_capture_or_new_attempt() {
    let (repo, bot, authority, prepared, original_request, turn, intent) = seeded();
    let (return_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-routine-retry"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    config.test_repository_return = Some(return_tx);
    let worker = AiWorker::new(config).spawn().unwrap();
    for context in [
        CommandContext::from(super::super::context(true)),
        prepared.clone(),
    ] {
        let request = retry(&bot, &turn);
        let (reply, response) = sync_channel(1);
        worker
            .client()
            .send(WorkerCommand::RetryTurn {
                context,
                chat: bot.main_chat,
                request,
                reply,
            })
            .unwrap();
        let error = response
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .err()
            .unwrap();
        assert_eq!(error.code, "routine_retry_unsupported");
        assert!(!error.retryable);
        assert!(error.message.contains("New messages use Bot access"));
    }
    // Exact initial-send replay wins, but a reused operation with changed
    // fingerprint still conflicts through the existing immutable validator.
    for changed in [false, true] {
        let mut request = original_request.clone();
        if changed {
            request.markdown.push_str(" changed");
        }
        let (reply, response) = sync_channel(1);
        worker
            .client()
            .send(WorkerCommand::SendTurn {
                context: prepared.clone(),
                request,
                reply,
            })
            .unwrap();
        let result = response.recv_timeout(Duration::from_secs(5)).unwrap();
        if changed {
            assert_eq!(result.err().unwrap().code, "request_conflict");
        } else {
            let replay = result.unwrap();
            assert!(replay.accepted.replayed);
            assert_eq!(replay.accepted.attempt_id, turn.attempt.id);
        }
    }
    worker.shutdown();
    let _ = worker.join();
    let repo = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(repo.attempts_for_chat(bot.main_chat).unwrap().len(), 1);
    assert_eq!(repo.messages(bot.main_chat).unwrap().len(), 2);
    assert_eq!(
        repo.bot_submission_for_attempt(turn.attempt.id)
            .unwrap()
            .unwrap()
            .intent,
        intent
    );
    assert_eq!(
        repo.attempt(turn.attempt.id)
            .unwrap()
            .unwrap()
            .stop_reason
            .as_deref(),
        Some("original-refusal")
    );
}

#[test]
fn routine_continue_preflight_and_writer_refuse_before_owner_capture() {
    let (repo, bot, authority, prepared, _, _, intent) = seeded();
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-routine-continue"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    let worker = AiWorker::new(config).spawn().unwrap();
    for context in [CommandContext::from(super::super::context(true)), prepared] {
        let command = BotCommand::Continue {
            bot_id: bot.id,
            request_id: BotRequestId::mint(),
            chat_id: bot.main_chat,
            expected_revision: bot.revision,
            destination: bot.home_workspace,
            task: BotTask {
                text: "new continuation".into(),
                documents: vec![],
            },
            previous_run: intent.submission.request.run,
        };
        let (reply, response) = sync_channel(1);
        worker
            .client()
            .send(WorkerCommand::Bots {
                context,
                request: command,
                reply,
            })
            .unwrap();
        let error = response
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .err()
            .unwrap();
        assert_eq!(error.code, "routine_retry_unsupported");
    }
    worker.shutdown();
    let _ = worker.join();
}

#[test]
fn routine_terminal_answer_preflight_and_writer_select_the_question_attempt() {
    let (mut repo, bot, authority, prepared, _, turn, _) = seeded();
    let row = ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: turn.attempt.id,
        chat_id: bot.main_chat,
        assistant_message_id: turn.assistant_message.id,
        tool_name: AgentToolKind::Approval.as_token().into(),
        read_only: false,
        status: ToolActivityStatus::Proposed,
        approval: ToolApproval::Pending,
        proposed_at: Timestamp::now(),
        started_at: None,
        completed_at: None,
        result_bytes: None,
        error_code: None,
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: None,
        agent_summary: None,
        agent_input: None,
        agent_output: None,
        text_offset: None,
        display_target: None,
        live: None,
    };
    repo.create_tool_activity(&row).unwrap();
    let (return_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-routine-answer"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    config.test_repository_return = Some(return_tx);
    let worker = AiWorker::new(config).spawn().unwrap();
    // Startup recovery cancels proposed rows independently of this request.
    // Compare the actual post-startup baseline across the refused commands.
    let row = worker
        .client()
        .read_chat(super::super::context(true), bot.main_chat)
        .unwrap()
        .tool_activities
        .into_iter()
        .find(|stored| stored.id == row.id)
        .unwrap();
    for context in [CommandContext::from(super::super::context(true)), prepared] {
        let mut request = request(&bot);
        request.markdown.clear();
        request.answers = Some(koru_ai_core::NextMessageAnswers {
            invocation: row.id,
            answers: vec![],
        });
        let (reply, response) = sync_channel(1);
        worker
            .client()
            .send(WorkerCommand::SendTurn {
                context,
                request,
                reply,
            })
            .unwrap();
        assert_eq!(
            response
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .err()
                .unwrap()
                .code,
            "routine_retry_unsupported"
        );
    }
    worker.shutdown();
    let _ = worker.join();
    let repo = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(repo.tool_activity(row.id).unwrap().unwrap(), row);
    assert_eq!(repo.attempts_for_chat(bot.main_chat).unwrap().len(), 1);
}
