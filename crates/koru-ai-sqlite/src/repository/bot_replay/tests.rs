// SPDX-License-Identifier: MPL-2.0
use super::super::tests::{chat, turn};
use super::*;
use koru_ai_core::bots::BotRepository;
use koru_ai_core::bots::{
    Bot, BotAccess, BotConfig, BotId, BotPage, BotRequestId, DeviceId, UserScopeId,
};
use koru_delegate_core as delegate;
use koru_delegate_core::{AGENT_EVENT_FIRST_SEQUENCE, AGENT_EVENT_INITIAL_CURSOR};
use std::collections::BTreeSet;
mod cancellation;
mod preparation;

fn at(n: i64) -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000 + n)
}
fn fixture(repo: &mut Repository) -> (Bot, PendingTurn, BotSubmission) {
    let workspace = WorkspaceId::new();
    let bot = Bot {
        id: BotId::mint(),
        home_workspace: workspace,
        user_scope: UserScopeId::new(),
        owner_device: DeviceId::from_uuid(Uuid::new_v4()),
        config: BotConfig {
            name: "Fixture".into(),
            role: BotPage {
                workspace,
                document: DocumentId::new(),
            },
            working_folder: "D:/fixture".into(),
            model: ModelSelection {
                provider: AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
        },
        access: BotAccess::Default,
        selected_pages: vec![],
        revision: 1,
        policy_revision: 1,
        main_chat: ChatId::new(),
        paused: false,
        folder_lock_request: None,
        folder_frozen_at_ms: None,
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    repo.save_bot(
        &bot,
        ProfileId::new(),
        BotRequestId::mint(),
        "fixture",
        None,
    )
    .unwrap();
    let turn = turn(bot.main_chat, Uuid::new_v4(), 1);
    let workspace = delegate::WorkspaceId::from_uuid(workspace.as_uuid());
    let submission = BotSubmission {
        provenance: delegate::BotProvenance {
            owner: delegate::BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device,
            },
            bot: bot.id,
            submission: BotRequestId::mint(),
            chat_id: bot.main_chat.as_uuid(),
            ai_attempt_id: Some(turn.attempt.id.as_uuid()),
            assistant_message_id: Some(turn.assistant_message.id.as_uuid()),
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        },
        policy: delegate::CapturedBotPolicy {
            workspace,
            destination: workspace,
            access: delegate::BotAccess::Default,
            selected_pages: BTreeSet::new(),
        },
        request: delegate::DelegationRequest {
            run: RunId::mint(),
            requested_by: delegate::ActorRef::Person {
                profile: delegate::ProfileId::from_uuid(Uuid::new_v4()),
            },
            objective: delegate::Objective::new("Fixture task").unwrap(),
            agent: delegate::SessionProfileKey::BuiltIn("codex".parse().unwrap()),
            requested_model: None,
            scope: delegate::WorkspaceScope::BotFolder {
                folder: delegate::BotWorkingFolder::new("D:/fixture".into()).unwrap(),
            },
            context: vec![],
            grants: BTreeSet::from([workspace]),
            executing_device: bot.owner_device,
            origin: delegate::RunOrigin::Manual,
        },
        workspace,
        input: delegate::PendingInput {
            idempotency_key: delegate::IdempotencyKey::mint(),
            text: "Question".into(),
            attachment_ids: vec![],
            references: vec![],
            mode: None,
            fast: None,
            reasoning: None,
            skills: vec![],
            model: None,
        },
    };
    assert!(submission.valid());
    let encoded = serde_json::json!({"version":1,"submission":submission}).to_string();
    repo.connection.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,
        captured_config_revision,captured_policy_revision,submission_version,submission_json,created_at_ms,updated_at_ms)
        VALUES (?1,?2,?3,'fixture','start','prepared',?4,1,1,1,?5,1,1)",params![bot.user_scope.to_string(),submission.provenance.submission.to_storage(),bot.id.to_storage(),submission.request.run.to_storage(),encoded]).unwrap();
    (bot, turn, submission)
}
fn prepared() -> (Repository, PendingTurn, BotSubmission, BotExecutionFence) {
    let mut repo = Repository::in_memory().unwrap();
    let (_, turn, submission) = fixture(&mut repo);
    repo.prepare_bot_turn(&turn, &submission, at(1)).unwrap();
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
    (repo, turn, submission, fence)
}
fn event(event: AiEvent) -> BotReplayEffect {
    BotReplayEffect::Event {
        terminal_cleanup: event.is_terminal().then(|| BotTerminalCleanup {
            retained_question_ids: vec![],
        }),
        event,
        finalized_blocks: vec![],
    }
}
fn batch(
    turn: &PendingTurn,
    fence: &BotExecutionFence,
    after: Option<u64>,
    effects: Vec<BotReplayEffect>,
) -> BotReplayBatch {
    BotReplayBatch {
        ai_attempt: turn.attempt.id,
        fence: fence.clone(),
        expected_after: after,
        events: vec![BotReplayEvent {
            sequence: after.unwrap_or(AGENT_EVENT_INITIAL_CURSOR) + 1,
            turn: fence.turn,
            effects,
        }],
    }
}
fn text(turn: &PendingTurn, value: &str) -> BotReplayEffect {
    event(AiEvent::TextDelta {
        attempt_id: turn.attempt.id,
        text: value.into(),
        at: at(3),
    })
}
fn complete(turn: &PendingTurn) -> BotReplayEffect {
    event(AiEvent::Completed {
        attempt_id: turn.attempt.id,
        stop_reason: None,
        at: at(4),
    })
}
fn tool(turn: &PendingTurn) -> ToolActivity {
    ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: turn.attempt.id,
        chat_id: turn.user_message.chat_id,
        assistant_message_id: turn.assistant_message.id,
        tool_name: AgentToolKind::Tool.as_token().into(),
        read_only: false,
        status: ToolActivityStatus::Proposed,
        approval: ToolApproval::NotRequired,
        proposed_at: at(3),
        started_at: None,
        completed_at: None,
        result_bytes: None,
        error_code: None,
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: Some("Fixture tool".into()),
        agent_summary: None,
        agent_input: None,
        agent_output: None,
        text_offset: None,
        display_target: None,
        live: None,
    }
}
fn source() -> BotToolSource {
    BotToolSource {
        kind: BotToolSourceKind::Tool,
        id: "native-fixture-call".into(),
    }
}

#[test]
fn populated_v16_migrates_twice_without_changing_chat_bot_or_receipt() {
    let older = koru_sqlite::Schema {
        database: crate::DATABASE_FILE,
        understood: 16,
        migrations: &db::MIGRATIONS[..16],
    };
    let connection = koru_sqlite::open_in_memory(&older).unwrap();
    let mut repo = Repository {
        connection,
        bot_replay_fail_after_commit: false,
    };
    let (bot, _, submission) = fixture(&mut repo);
    let ordinary = ChatId::new();
    repo.create_chat(&chat(ordinary, 0)).unwrap();
    let completed = turn(ordinary, Uuid::new_v4(), 1);
    repo.begin_turn(&completed).unwrap();
    // Populate v16 using its existing mutation implementation, before the new
    // public apply_event guard can query the not-yet-created ai17 association.
    let tx = repo.connection.transaction().unwrap();
    mutations::apply_event_on(
        &tx,
        &AiEvent::TextDelta {
            attempt_id: completed.attempt.id,
            text: "Retained content".into(),
            at: at(2),
        },
        &[],
    )
    .unwrap();
    mutations::apply_event_on(
        &tx,
        &AiEvent::Completed {
            attempt_id: completed.attempt.id,
            stop_reason: None,
            at: at(3),
        },
        &[],
    )
    .unwrap();
    tx.commit().unwrap();
    let before = repo.messages(ordinary).unwrap();
    db::prepare(&mut repo.connection).unwrap();
    db::prepare(&mut repo.connection).unwrap();
    assert_eq!(repo.schema_version().unwrap(), db::SCHEMA_VERSION);
    assert_eq!(repo.messages(ordinary).unwrap(), before);
    assert_eq!(
        repo.attempt(completed.attempt.id).unwrap().unwrap().status,
        AttemptStatus::Complete
    );
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot));
    assert_eq!(
        repo.connection
            .query_row(
                "SELECT planned_run_id FROM bot_command_receipts WHERE request_id=?1",
                [submission.provenance.submission.to_storage()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        submission.request.run.to_storage()
    );
    assert!(koru_sqlite::prepare(&older, &mut repo.connection).is_err());
    assert_eq!(repo.schema_version().unwrap(), db::SCHEMA_VERSION);
}

#[test]
fn prepared_turn_binding_is_atomic_and_replay_is_identity_checked() {
    let mut repo = Repository::in_memory().unwrap();
    let (_, turn, submission) = fixture(&mut repo);
    repo.connection.execute_batch("CREATE TRIGGER reject_replay BEFORE INSERT ON bot_run_replay BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(repo.prepare_bot_turn(&turn, &submission, at(1)).is_err());
    assert!(repo.attempt(turn.attempt.id).unwrap().is_none());
    assert!(repo.messages(turn.user_message.chat_id).unwrap().is_empty());
    assert_eq!(
        repo.connection
            .query_row("SELECT count(*) FROM bot_chat_runs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    repo.connection
        .execute_batch("DROP TRIGGER reject_replay")
        .unwrap();
    repo.prepare_bot_turn(&turn, &submission, at(1)).unwrap();
    repo.prepare_bot_turn(&turn, &submission, at(2)).unwrap();
    assert_eq!(repo.messages(turn.user_message.chat_id).unwrap().len(), 2);
    repo.connection.execute("UPDATE bots SET revision=revision+1,policy_revision=policy_revision+1,paused=1 WHERE bot_id=?1",[submission.provenance.bot.to_storage()]).unwrap();
    repo.prepare_bot_turn(&turn, &submission, at(3)).unwrap();
    assert_eq!(repo.messages(turn.user_message.chat_id).unwrap().len(), 2);
    let mut wrong = turn.clone();
    wrong.attempt.operation_id = Uuid::new_v4();
    assert!(repo.prepare_bot_turn(&wrong, &submission, at(3)).is_err());
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
            [submission.provenance.bot.to_storage()],
        )
        .unwrap();
    assert!(repo.prepare_bot_turn(&turn, &submission, at(3)).is_err());
}

#[test]
fn receipt_shape_or_changed_typed_payload_cannot_create_an_attempt() {
    let mut repo = Repository::in_memory().unwrap();
    let (_, turn, submission) = fixture(&mut repo);
    let mut different_task = turn.clone();
    different_task
        .user_message
        .markdown
        .push_str(" a different task");
    assert!(
        repo.prepare_bot_turn(&different_task, &submission, at(1))
            .is_err()
    );
    let mut changed = submission.clone();
    changed.input.text.push_str(" different");
    assert!(repo.prepare_bot_turn(&turn, &changed, at(1)).is_err());
    repo.connection.execute("UPDATE bot_command_receipts SET submission_json=json_set(submission_json,'$.extra',1) WHERE request_id=?1",[submission.provenance.submission.to_storage()]).unwrap();
    assert!(repo.prepare_bot_turn(&turn, &submission, at(1)).is_err());
    assert!(repo.attempt(turn.attempt.id).unwrap().is_none());
}

#[test]
fn late_cursor_failure_rolls_back_text_tool_mode_completion_and_outbox() {
    let (mut repo, turn, _, fence) = prepared();
    let activity = tool(&turn);
    let events = batch(
        &turn,
        &fence,
        None,
        vec![
            text(&turn, "Answer"),
            BotReplayEffect::CreateTool {
                source: source(),
                activity: activity.clone(),
            },
            BotReplayEffect::Mode(Some("fixture-mode".into())),
            complete(&turn),
        ],
    );
    repo.connection.execute_batch("CREATE TRIGGER reject_cursor BEFORE UPDATE OF last_sequence ON bot_run_replay BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(repo.apply_bot_replay(&events, at(5)).is_err());
    assert_eq!(
        repo.messages(turn.user_message.chat_id).unwrap()[1].markdown,
        ""
    );
    assert!(repo.tool_activity(activity.id).unwrap().is_none());
    assert_eq!(
        repo.attempt(turn.attempt.id).unwrap().unwrap().status,
        AttemptStatus::Pending
    );
    assert!(
        repo.bot_delivery_receipt(turn.attempt.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.bot_replay_state(turn.attempt.id)
            .unwrap()
            .last_sequence,
        None
    );
    assert!(
        repo.connection
            .query_row(
                "SELECT agent_mode FROM chats WHERE id=?1",
                [turn.user_message.chat_id.to_storage()],
                |r| r.get::<_, Option<String>>(0)
            )
            .unwrap()
            .is_none()
    );
    repo.connection
        .execute_batch("DROP TRIGGER reject_cursor")
        .unwrap();
    assert_eq!(
        repo.apply_bot_replay(&events, at(6)).unwrap(),
        BotReplayDisposition::Applied
    );
    assert_eq!(
        repo.messages(turn.user_message.chat_id).unwrap()[1].markdown,
        "Answer"
    );
}

#[test]
fn real_agent_first_event_and_missing_first_event_are_distinct() {
    let (mut repo, turn, _, fence) = prepared();
    assert_eq!(
        repo.bot_replay_state(turn.attempt.id)
            .unwrap()
            .last_sequence,
        None
    );
    for (sequence, why) in [
        (
            AGENT_EVENT_INITIAL_CURSOR,
            "baseline is not an emitted event",
        ),
        (
            AGENT_EVENT_FIRST_SEQUENCE + 1,
            "first emitted event is missing",
        ),
    ] {
        let mut invalid = batch(&turn, &fence, None, vec![text(&turn, "invalid")]);
        invalid.events[0].sequence = sequence;
        assert!(repo.apply_bot_replay(&invalid, at(3)).is_err(), "{why}");
    }
    assert_eq!(
        repo.bot_replay_state(turn.attempt.id)
            .unwrap()
            .last_sequence,
        None
    );
    assert_eq!(repo.bot_reply_bytes(turn.attempt.id).unwrap(), 0);
    let first = batch(&turn, &fence, None, vec![text(&turn, "one")]);
    assert_eq!(first.events[0].sequence, AGENT_EVENT_FIRST_SEQUENCE);
    assert_eq!(
        repo.apply_bot_replay(&first, at(4)).unwrap(),
        BotReplayDisposition::Applied
    );
    assert_eq!(
        repo.apply_bot_replay(&first, at(4)).unwrap(),
        BotReplayDisposition::Duplicate
    );
    let mut gap = batch(
        &turn,
        &fence,
        Some(AGENT_EVENT_FIRST_SEQUENCE),
        vec![text(&turn, "gap")],
    );
    gap.events[0].sequence += 1;
    assert!(repo.apply_bot_replay(&gap, at(5)).is_err());
    let next = batch(
        &turn,
        &fence,
        Some(AGENT_EVENT_FIRST_SEQUENCE),
        vec![text(&turn, "two")],
    );
    repo.apply_bot_replay(&next, at(6)).unwrap();
    assert_eq!(
        repo.messages(turn.user_message.chat_id).unwrap()[1].markdown,
        "onetwo"
    );
    assert_eq!(
        repo.bot_replay_state(turn.attempt.id)
            .unwrap()
            .last_sequence,
        Some(AGENT_EVENT_FIRST_SEQUENCE + 1)
    );
}

#[test]
fn duplicate_overlap_gap_and_foreign_fences_never_append_twice() {
    let (mut repo, turn, _, fence) = prepared();
    let first = batch(&turn, &fence, None, vec![text(&turn, "one")]);
    repo.apply_bot_replay(&first, at(4)).unwrap();
    assert_eq!(
        repo.apply_bot_replay(&first, at(4)).unwrap(),
        BotReplayDisposition::Duplicate
    );
    let mut overlap = first.clone();
    overlap.events.push(BotReplayEvent {
        sequence: AGENT_EVENT_FIRST_SEQUENCE + 1,
        turn: fence.turn,
        effects: vec![text(&turn, "bad")],
    });
    assert!(repo.apply_bot_replay(&overlap, at(4)).is_err());
    let mut gap = batch(&turn, &fence, Some(AGENT_EVENT_FIRST_SEQUENCE), vec![]);
    gap.events[0].sequence = AGENT_EVENT_FIRST_SEQUENCE + 2;
    assert!(repo.apply_bot_replay(&gap, at(4)).is_err());
    for changed in [
        BotExecutionFence {
            generation: fence.generation.next(),
            ..fence.clone()
        },
        BotExecutionFence {
            session: SessionId::mint(),
            ..fence.clone()
        },
        BotExecutionFence {
            turn: Some(AgentTurnId::mint()),
            ..fence.clone()
        },
    ] {
        assert!(
            repo.bind_bot_execution(turn.attempt.id, &changed, at(4))
                .is_err()
        );
        assert!(
            repo.apply_bot_replay(
                &batch(
                    &turn,
                    &changed,
                    Some(AGENT_EVENT_FIRST_SEQUENCE),
                    vec![text(&turn, "bad")]
                ),
                at(4)
            )
            .is_err()
        );
    }
    let mut turnless = batch(
        &turn,
        &fence,
        Some(AGENT_EVENT_FIRST_SEQUENCE),
        vec![event(AiEvent::ContextUsage {
            attempt_id: turn.attempt.id,
            used: 4,
            size: 8,
        })],
    );
    turnless.events[0].turn = None;
    repo.apply_bot_replay(&turnless, at(4)).unwrap();
    assert_eq!(
        repo.bot_replay_state(turn.attempt.id).unwrap().fence,
        Some(fence)
    );
    assert_eq!(
        repo.messages(turn.user_message.chat_id).unwrap()[1].markdown,
        "one"
    );
}

#[test]
fn reconnect_resolves_random_tool_identity_and_multi_effect_event_is_atomic() {
    let (mut repo, turn, _, fence) = prepared();
    let mut activity = tool(&turn);
    let create = batch(
        &turn,
        &fence,
        None,
        vec![BotReplayEffect::CreateTool {
            source: source(),
            activity: activity.clone(),
        }],
    );
    repo.apply_bot_replay(&create, at(4)).unwrap();
    // New mapper reads the durable source binding, rather than deriving a UUID.
    assert_eq!(
        repo.bot_tool_source(turn.attempt.id, &source()).unwrap(),
        Some(activity.id)
    );
    assert_eq!(
        repo.bot_source_for_tool(turn.attempt.id, activity.id)
            .unwrap(),
        Some(source())
    );
    assert!(
        repo.bot_source_for_tool(InferenceAttemptId::new(), activity.id)
            .unwrap()
            .is_none()
    );
    assert!(
        repo.bot_source_for_tool(turn.attempt.id, ToolInvocationId::new())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.apply_bot_replay(&create, at(4)).unwrap(),
        BotReplayDisposition::Duplicate
    );
    activity.status = ToolActivityStatus::Complete;
    activity.started_at = Some(at(3));
    activity.completed_at = Some(at(4));
    let finished = batch(
        &turn,
        &fence,
        Some(AGENT_EVENT_FIRST_SEQUENCE),
        vec![
            text(&turn, "done"),
            BotReplayEffect::AdvanceTool {
                source: source(),
                activity: activity.clone(),
            },
            complete(&turn),
        ],
    );
    repo.apply_bot_replay(&finished, at(5)).unwrap();
    assert_eq!(
        repo.tool_activities(turn.user_message.chat_id)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        repo.tool_activity(activity.id).unwrap().unwrap().status,
        ToolActivityStatus::Complete
    );
    assert_eq!(
        repo.bot_replay_state(turn.attempt.id)
            .unwrap()
            .last_sequence,
        Some(AGENT_EVENT_FIRST_SEQUENCE + 1)
    );
}

#[test]
fn completion_outbox_survives_restart_and_ack_clock_reversal() {
    let (mut repo, turn, submission, fence) = prepared();
    repo.apply_bot_replay(
        &batch(
            &turn,
            &fence,
            None,
            vec![text(&turn, "complete"), complete(&turn)],
        ),
        at(5),
    )
    .unwrap();
    let receipt = repo.bot_delivery_receipt(turn.attempt.id).unwrap().unwrap();
    repo.recover_interrupted(at(6)).unwrap();
    let owner = koru_ai_core::bots::BotOwner {
        user_scope: submission.provenance.owner.user_scope,
        device: submission.provenance.owner.device,
    };
    assert_eq!(
        repo.pending_bot_deliveries(owner, None, 10)
            .unwrap()
            .receipts,
        vec![receipt.clone()]
    );
    let mut wrong = receipt.clone();
    wrong.confirmed_at_ms += 1;
    assert!(
        repo.acknowledge_bot_delivery(turn.attempt.id, &wrong, at(3))
            .is_err()
    );
    repo.mark_bot_delivery_unconfirmed(turn.attempt.id, &receipt)
        .unwrap();
    repo.acknowledge_bot_delivery(turn.attempt.id, &receipt, at(2))
        .unwrap();
    repo.acknowledge_bot_delivery(turn.attempt.id, &receipt, at(20))
        .unwrap();
    assert!(
        repo.pending_bot_deliveries(owner, None, 10)
            .unwrap()
            .receipts
            .is_empty()
    );
    assert_eq!(
        repo.connection
            .query_row(
                "SELECT delivery_acknowledged_at_ms FROM bot_chat_runs",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        at(2).millis()
    );
    assert_eq!(
        repo.apply_bot_replay(
            &batch(
                &turn,
                &fence,
                Some(AGENT_EVENT_FIRST_SEQUENCE),
                vec![text(&turn, "late")]
            ),
            at(7)
        )
        .unwrap(),
        BotReplayDisposition::Terminal
    );
    assert_eq!(
        repo.bot_replay_state(turn.attempt.id)
            .unwrap()
            .last_sequence,
        Some(AGENT_EVENT_FIRST_SEQUENCE)
    );
}

#[test]
fn restart_and_gap_interrupt_without_reopening_or_advancing() {
    for restart in [true, false] {
        let (mut repo, turn, _, fence) = prepared();
        let activity = tool(&turn);
        repo.apply_bot_replay(
            &batch(
                &turn,
                &fence,
                None,
                vec![
                    text(&turn, "partial"),
                    BotReplayEffect::CreateTool {
                        source: source(),
                        activity: activity.clone(),
                    },
                ],
            ),
            at(4),
        )
        .unwrap();
        if restart {
            repo.recover_interrupted(at(5)).unwrap();
        } else {
            repo.stop_bot_replay(turn.attempt.id, &fence, BotReplayStop::ReplayGap, at(5))
                .unwrap();
        }
        assert_eq!(
            repo.attempt(turn.attempt.id).unwrap().unwrap().status,
            AttemptStatus::Interrupted
        );
        assert_eq!(
            repo.tool_activity(activity.id).unwrap().unwrap().status,
            ToolActivityStatus::Cancelled
        );
        assert_eq!(
            repo.bot_replay_state(turn.attempt.id)
                .unwrap()
                .last_sequence,
            Some(AGENT_EVENT_FIRST_SEQUENCE)
        );
        assert_eq!(
            repo.bot_replay_state(turn.attempt.id).unwrap().stop_reason,
            Some(if restart {
                BotReplayStop::AiRestart
            } else {
                BotReplayStop::ReplayGap
            })
        );
        assert_eq!(
            repo.apply_bot_replay(
                &batch(
                    &turn,
                    &fence,
                    Some(AGENT_EVENT_FIRST_SEQUENCE),
                    vec![complete(&turn)]
                ),
                at(6)
            )
            .unwrap(),
            BotReplayDisposition::Terminal
        );
        assert!(
            repo.bot_delivery_receipt(turn.attempt.id)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn a_refused_initial_input_fails_the_attempt_with_its_class_and_keeps_the_reason() {
    let mut repo = Repository::in_memory().unwrap();
    let (_, turn, submission) = fixture(&mut repo);
    repo.prepare_bot_turn(&turn, &submission, at(1)).unwrap();
    let id = turn.attempt.id;
    repo.refuse_bot_input(
        id,
        AiErrorClass::ContextTooLarge,
        "bot_input_too_large",
        at(3),
    )
    .unwrap();
    let attempt = repo.attempt(id).unwrap().unwrap();
    assert_eq!(attempt.status, AttemptStatus::Failed);
    assert_eq!(attempt.error_class, Some(AiErrorClass::ContextTooLarge));
    assert_eq!(attempt.stop_reason.as_deref(), Some("bot_input_too_large"));
    let reply = repo.message(turn.assistant_message.id).unwrap().unwrap();
    assert_eq!(reply.state, MessageState::Failed);
    assert_eq!(reply.error_class, Some(AiErrorClass::ContextTooLarge));
    // Replay stops as for any local end: nothing later reopens the reply.
    assert_eq!(
        repo.bot_replay_state(id).unwrap().stop_reason,
        Some(BotReplayStop::OwnerUnavailable)
    );
    // Terminal is absorbing: a later interruption changes neither status nor reason.
    repo.interrupt_bot_preparation(id, at(4)).unwrap();
    let attempt = repo.attempt(id).unwrap().unwrap();
    assert_eq!(attempt.status, AttemptStatus::Failed);
    assert_eq!(attempt.stop_reason.as_deref(), Some("bot_input_too_large"));
}

#[test]
fn bounded_batches_and_wrong_effect_owners_refuse_without_writes() {
    let (mut repo, turn, _, fence) = prepared();
    let mut too_many = batch(&turn, &fence, None, vec![]);
    too_many.events = (1..=MAX_REPLAY_EVENTS + 1)
        .map(|n| BotReplayEvent {
            sequence: n as u64 + AGENT_EVENT_INITIAL_CURSOR,
            turn: fence.turn,
            effects: vec![],
        })
        .collect();
    assert!(repo.apply_bot_replay(&too_many, at(4)).is_err());
    assert!(
        repo.apply_bot_replay(
            &batch(
                &turn,
                &fence,
                None,
                vec![text(&turn, &"x".repeat(MAX_REPLAY_BYTES + 1))]
            ),
            at(4)
        )
        .is_err()
    );
    let mut wrong = tool(&turn);
    wrong.assistant_message_id = MessageId::new();
    assert!(
        repo.apply_bot_replay(
            &batch(
                &turn,
                &fence,
                None,
                vec![
                    text(&turn, "rollback"),
                    BotReplayEffect::CreateTool {
                        source: source(),
                        activity: wrong
                    }
                ]
            ),
            at(4)
        )
        .is_err()
    );
    assert!(
        repo.apply_bot_replay(
            &batch(
                &turn,
                &fence,
                None,
                vec![complete(&turn), text(&turn, "after terminal")]
            ),
            at(4)
        )
        .is_err()
    );
    assert_eq!(
        repo.messages(turn.user_message.chat_id).unwrap()[1].markdown,
        ""
    );
    assert_eq!(
        repo.bot_replay_state(turn.attempt.id)
            .unwrap()
            .last_sequence,
        None
    );
}

#[test]
fn sql_cohorts_and_restrict_keys_are_not_nullable_bypasses() {
    let (repo, turn, _, _) = prepared();
    for statement in [
        "UPDATE bot_run_replay SET generation=NULL",
        "UPDATE bot_run_replay SET session_id=NULL",
        "UPDATE bot_run_replay SET run_attempt_id=NULL",
        "UPDATE bot_run_replay SET generation=0",
        "UPDATE bot_run_replay SET last_sequence=-1",
        "UPDATE bot_chat_runs SET delivery_state='pending'",
        "UPDATE bot_chat_runs SET delivery_state='acknowledged',delivery_confirmed_at_ms=4",
        "DELETE FROM inference_attempts",
        "DELETE FROM bot_chats",
        "DELETE FROM messages",
    ] {
        assert!(
            repo.connection.execute_batch(statement).is_err(),
            "{statement}"
        );
    }
    assert_eq!(
        repo.attempt(turn.attempt.id).unwrap().unwrap().status,
        AttemptStatus::Pending
    );
}

#[test]
fn explicit_terminal_cleanup_preserves_next_message_questions_and_rolls_back_with_cursor() {
    let (mut repo, turn, _, fence) = prepared();
    let ordinary = tool(&turn);
    let question = tool(&turn);
    let question_source = BotToolSource {
        kind: BotToolSourceKind::Questions,
        id: "question-set".into(),
    };
    repo.apply_bot_replay(
        &batch(
            &turn,
            &fence,
            None,
            vec![
                BotReplayEffect::CreateTool {
                    source: source(),
                    activity: ordinary.clone(),
                },
                BotReplayEffect::CreateTool {
                    source: question_source,
                    activity: question.clone(),
                },
            ],
        ),
        at(4),
    )
    .unwrap();
    let terminal = AiEvent::Completed {
        attempt_id: turn.attempt.id,
        stop_reason: None,
        at: at(5),
    };
    // A terminal event must explicitly supply a cleanup decision, even empty.
    assert!(
        repo.apply_bot_replay(
            &batch(
                &turn,
                &fence,
                Some(AGENT_EVENT_FIRST_SEQUENCE),
                vec![BotReplayEffect::Event {
                    event: terminal.clone(),
                    finalized_blocks: vec![],
                    terminal_cleanup: None,
                }]
            ),
            at(5)
        )
        .is_err()
    );
    assert!(
        repo.apply_event(&terminal, &[]).is_err(),
        "linked transcript cannot bypass its cursor"
    );
    // Retaining a normal tool under the question exception is invalid.
    assert!(
        repo.apply_bot_replay(
            &batch(
                &turn,
                &fence,
                Some(AGENT_EVENT_FIRST_SEQUENCE),
                vec![BotReplayEffect::Event {
                    event: terminal.clone(),
                    finalized_blocks: vec![],
                    terminal_cleanup: Some(BotTerminalCleanup {
                        retained_question_ids: vec![ordinary.id]
                    }),
                }]
            ),
            at(5)
        )
        .is_err()
    );
    let final_batch = batch(
        &turn,
        &fence,
        Some(AGENT_EVENT_FIRST_SEQUENCE),
        vec![
            text(&turn, "final"),
            BotReplayEffect::Event {
                event: terminal,
                finalized_blocks: vec![],
                terminal_cleanup: Some(BotTerminalCleanup {
                    retained_question_ids: vec![question.id],
                }),
            },
        ],
    );
    repo.connection.execute_batch("CREATE TRIGGER reject_cursor BEFORE UPDATE OF last_sequence ON bot_run_replay BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(repo.apply_bot_replay(&final_batch, at(5)).is_err());
    assert_eq!(
        repo.tool_activity(ordinary.id).unwrap().unwrap().status,
        ToolActivityStatus::Proposed
    );
    assert_eq!(
        repo.tool_activity(question.id).unwrap().unwrap().status,
        ToolActivityStatus::Proposed
    );
    assert!(
        repo.bot_delivery_receipt(turn.attempt.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repo.bot_replay_state(turn.attempt.id)
            .unwrap()
            .last_sequence,
        Some(AGENT_EVENT_FIRST_SEQUENCE)
    );
    repo.connection
        .execute_batch("DROP TRIGGER reject_cursor")
        .unwrap();
    repo.apply_bot_replay(&final_batch, at(6)).unwrap();
    assert_eq!(
        repo.tool_activity(ordinary.id).unwrap().unwrap().status,
        ToolActivityStatus::Cancelled
    );
    assert_eq!(
        repo.tool_activity(question.id).unwrap().unwrap().status,
        ToolActivityStatus::Proposed
    );
    assert!(
        repo.bot_delivery_receipt(turn.attempt.id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn one_batch_commits_interleaved_source_effects_blocks_and_notice_at_the_common_cut() {
    let (mut repo, turn, _, fence) = prepared();
    let notice = AgentNotice {
        id: AgentNoticeId::new(),
        attempt_id: turn.attempt.id,
        chat_id: turn.user_message.chat_id,
        assistant_message_id: turn.assistant_message.id,
        text: "Fixture status".into(),
        text_offset: Some(0),
        created_at: at(3),
    };
    let blocks = vec![MessageBlock {
        id: MessageBlockId::new(),
        message_id: turn.assistant_message.id,
        ordinal: 0,
        kind: MessageBlockKind::Paragraph,
        markdown: "alphaomega".into(),
        plain_text: "alphaomega".into(),
    }];
    let events = BotReplayBatch {
        ai_attempt: turn.attempt.id,
        fence: fence.clone(),
        expected_after: None,
        events: vec![
            BotReplayEvent {
                sequence: AGENT_EVENT_FIRST_SEQUENCE,
                turn: fence.turn,
                effects: vec![text(&turn, "alpha")],
            },
            BotReplayEvent {
                sequence: AGENT_EVENT_FIRST_SEQUENCE + 1,
                turn: fence.turn,
                effects: vec![
                    event(AiEvent::ReasoningDelta {
                        attempt_id: turn.attempt.id,
                        text: "ephemeral reason".into(),
                        at: at(3),
                    }),
                    BotReplayEffect::Notice(notice),
                ],
            },
            BotReplayEvent {
                sequence: AGENT_EVENT_FIRST_SEQUENCE + 2,
                turn: None,
                effects: vec![],
            },
            BotReplayEvent {
                sequence: AGENT_EVENT_FIRST_SEQUENCE + 3,
                turn: fence.turn,
                effects: vec![
                    text(&turn, "omega"),
                    BotReplayEffect::Event {
                        event: AiEvent::Completed {
                            attempt_id: turn.attempt.id,
                            stop_reason: None,
                            at: at(4),
                        },
                        finalized_blocks: blocks,
                        terminal_cleanup: Some(BotTerminalCleanup {
                            retained_question_ids: vec![],
                        }),
                    },
                ],
            },
        ],
    };
    repo.apply_bot_replay(&events, at(5)).unwrap();
    assert_eq!(
        repo.messages(turn.user_message.chat_id).unwrap()[1].markdown,
        "alphaomega"
    );
    assert_eq!(repo.blocks(turn.user_message.chat_id).unwrap().len(), 1);
    assert_eq!(
        repo.agent_notices(turn.user_message.chat_id).unwrap()[0].text_offset,
        Some(5)
    );
    assert_eq!(
        repo.bot_replay_state(turn.attempt.id)
            .unwrap()
            .last_sequence,
        Some(AGENT_EVENT_FIRST_SEQUENCE + 3)
    );
    assert_eq!(
        repo.apply_bot_replay(&events, at(6)).unwrap(),
        BotReplayDisposition::Duplicate
    );
    assert_eq!(
        repo.agent_notices(turn.user_message.chat_id).unwrap().len(),
        1
    );
}

#[test]
fn keyset_delivery_scan_advances_past_a_full_page_of_unconfirmed_work() {
    let mut repo = Repository::in_memory().unwrap();
    let (bot, first_turn, original) = fixture(&mut repo);
    let owner = koru_ai_core::bots::BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    for index in 0..260 {
        let (pending, submission) = if index == 0 {
            (first_turn.clone(), original.clone())
        } else {
            let mut pending = turn(bot.main_chat, Uuid::new_v4(), 1);
            pending.user_message.ordinal = index * 2;
            pending.assistant_message.ordinal = index * 2 + 1;
            let mut submission = original.clone();
            submission.provenance.submission = BotRequestId::mint();
            submission.request.run = RunId::mint();
            submission.provenance.ai_attempt_id = Some(pending.attempt.id.as_uuid());
            submission.provenance.assistant_message_id =
                Some(pending.assistant_message.id.as_uuid());
            repo.connection.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,
                captured_config_revision,captured_policy_revision,submission_version,submission_json,created_at_ms,updated_at_ms)
                VALUES (?1,?2,?3,'fixture','start','prepared',?4,1,1,1,?5,1,1)",params![bot.user_scope.to_string(),submission.provenance.submission.to_storage(),bot.id.to_storage(),
                submission.request.run.to_storage(),serde_json::json!({"version":1,"submission":submission}).to_string()]).unwrap();
            (pending, submission)
        };
        // Equal timestamps exercise the identity tie-breaker, not just time.
        repo.prepare_bot_turn(&pending, &submission, at(1)).unwrap();
        let fence = BotExecutionFence {
            owner: submission.provenance.owner,
            bot: bot.id,
            run: submission.request.run,
            attempt: RunAttemptId::mint(),
            generation: AttemptGeneration::FIRST,
            session: SessionId::mint(),
            turn: Some(AgentTurnId::mint()),
        };
        repo.bind_bot_execution(pending.attempt.id, &fence, at(2))
            .unwrap();
        repo.apply_bot_replay(
            &batch(&pending, &fence, None, vec![complete(&pending)]),
            at(5),
        )
        .unwrap();
        let receipt = repo
            .bot_delivery_receipt(pending.attempt.id)
            .unwrap()
            .unwrap();
        repo.mark_bot_delivery_unconfirmed(pending.attempt.id, &receipt)
            .unwrap();
    }
    let first = repo.pending_bot_deliveries(owner, None, 256).unwrap();
    assert_eq!(first.receipts.len(), 256);
    assert!(first.next_cursor.is_some());
    let second = repo
        .pending_bot_deliveries(owner, first.next_cursor, 256)
        .unwrap();
    assert_eq!(second.receipts.len(), 4);
    assert!(second.next_cursor.is_none());
    assert!(second.receipts.iter().all(|r| !first.receipts.contains(r)));
    // Even when every oldest item remains unresolved, later work is reachable.
    assert_eq!(
        repo.pending_bot_deliveries(owner, None, 256).unwrap(),
        first
    );
    let other = koru_ai_core::bots::BotOwner {
        user_scope: UserScopeId::new(),
        ..owner
    };
    assert!(
        repo.pending_bot_deliveries(other, None, 256)
            .unwrap()
            .receipts
            .is_empty()
    );
}
