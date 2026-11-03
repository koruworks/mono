// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::{
    AiErrorClass, AiEvent, AiIntent, AiPersistence, AiProviderId, AiSearchQuery, AiSettings,
    AiSurface, AiUsage, AttemptStatus, BranchCopy, BranchOrigin, Chat, ChatAccessLens, ChatId,
    ChatRole, ChatToolPolicy, ContextCommit, DocumentDestination, DocumentExport, DocumentExportId,
    DocumentId, DocumentRelation, ExportStatus, FeedbackEvent, FeedbackEventId, FeedbackValue,
    InferenceAttempt, InferenceAttemptId, ListChatsToolRequest, Message, MessageBlock,
    MessageBlockId, MessageBlockKind, MessageBlockRef, MessageDocumentRef, MessageId, MessageState,
    ModelSelection, PendingRetry, PendingTurn, PinnedDocument, ProfileId, ReadChatToolRequest,
    ReasoningChoice, ReasoningLevel, RequestMeasurements, SearchChatsToolRequest, SourceLink,
    StoreRevision, Timestamp, ToolActivity, ToolActivityStatus, ToolApproval, ToolInvocationId,
    ToolMode, UsageSource, WebSearchPhase, WorkspaceId, list_chats, read_chat, search_chats,
};
use uuid::Uuid;

use super::*;

fn at(offset: i64) -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000 + offset)
}

fn model() -> ModelSelection {
    ModelSelection {
        provider: AiProviderId::parse("mock").unwrap(),
        requested_model: "deterministic-v1".into(),
        default_reasoning: ReasoningChoice::Default,
    }
}

fn chat(id: ChatId, offset: i64) -> Chat {
    Chat {
        id,
        title: "Test chat".into(),
        private: false,
        current_model: model(),
        branched_from: None,
        agent_working_folder: None,
        agent_mode: None,
        agent_fast: None,
        pinned_at: None,
        created_at: at(offset),
        updated_at: at(offset),
    }
}

#[test]
fn a_chats_working_folder_is_durable_listed_and_unset_by_none() {
    let mut repository = Repository::in_memory().unwrap();
    let chat_id = ChatId::new();
    repository.create_chat(&chat(chat_id, 0)).unwrap();
    assert!(
        repository
            .chat(chat_id)
            .unwrap()
            .unwrap()
            .agent_working_folder
            .is_none()
    );

    let updated = repository
        .set_chat_working_folder(chat_id, Some("D:/work/koru"))
        .unwrap()
        .unwrap();
    assert_eq!(
        updated.agent_working_folder.as_deref(),
        Some("D:/work/koru")
    );
    assert_eq!(updated.updated_at, at(0), "not activity (D85)");
    let listed = repository.chats().unwrap();
    assert_eq!(
        listed[0].agent_working_folder.as_deref(),
        Some("D:/work/koru")
    );

    let cleared = repository
        .set_chat_working_folder(chat_id, None)
        .unwrap()
        .unwrap();
    assert!(cleared.agent_working_folder.is_none());
    assert!(
        repository
            .set_chat_working_folder(ChatId::new(), None)
            .unwrap()
            .is_none()
    );
}

/// **A chat's approval mode** (koru-ai D7) is durable, listed, and — like
/// a rename — leaves the chat where it is in the list.
#[test]
fn a_chats_agent_mode_is_durable_listed_and_leaves_its_place() {
    let mut repository = Repository::in_memory().unwrap();
    let chat_id = ChatId::new();
    repository.create_chat(&chat(chat_id, 0)).unwrap();
    assert!(
        repository
            .chat(chat_id)
            .unwrap()
            .unwrap()
            .agent_mode
            .is_none()
    );

    let updated = repository
        .set_chat_agent_mode(chat_id, Some("acceptEdits"))
        .unwrap()
        .unwrap();
    assert_eq!(updated.agent_mode.as_deref(), Some("acceptEdits"));
    assert_eq!(updated.updated_at, at(0), "the list order is untouched");
    assert_eq!(
        repository.chats().unwrap()[0].agent_mode.as_deref(),
        Some("acceptEdits")
    );
    assert!(
        repository
            .set_chat_agent_mode(ChatId::new(), Some("plan"))
            .unwrap()
            .is_none()
    );

    // Fast mode (D11) beside it: on, then off, and listed.
    let fast = repository
        .set_chat_agent_fast(chat_id, Some(true))
        .unwrap()
        .unwrap();
    assert_eq!(fast.agent_fast, Some(true));
    assert_eq!(fast.agent_mode.as_deref(), Some("acceptEdits"));
    repository
        .set_chat_agent_fast(chat_id, Some(false))
        .unwrap();
    assert_eq!(repository.chats().unwrap()[0].agent_fast, Some(false));
}

#[test]
fn chat_privacy_is_durable_and_visible_in_cheap_overviews() {
    let mut repository = Repository::in_memory().unwrap();
    let chat_id = ChatId::new();
    repository.create_chat(&chat(chat_id, 0)).unwrap();
    assert!(!repository.chat(chat_id).unwrap().unwrap().private);

    let updated = repository.set_chat_private(chat_id, true).unwrap().unwrap();
    assert!(updated.private);
    assert_eq!(updated.updated_at, at(0), "not activity (D85)");
    assert!(repository.chat(chat_id).unwrap().unwrap().private);

    let overviews = repository.chat_overviews().unwrap();
    assert_eq!(overviews.len(), 1);
    assert_eq!(overviews[0].chat_id, chat_id);
    assert!(overviews[0].private);
    assert_eq!(overviews[0].message_count, 0);
}

#[test]
fn chat_tool_policy_defaults_deny_all_and_replaces_the_complete_grant_set() {
    let mut repository = Repository::in_memory().unwrap();
    let chat_id = ChatId::new();
    repository.create_chat(&chat(chat_id, 0)).unwrap();
    assert_eq!(
        repository.chat_tool_policy(chat_id).unwrap().unwrap(),
        ChatToolPolicy {
            chat_id,
            mode: ToolMode::Manual,
            workspace_ids: Vec::new(),
        }
    );

    let first = WorkspaceId::new();
    let second = WorkspaceId::new();
    let policy = repository
        .replace_chat_tool_policy(chat_id, ToolMode::Auto, &[second, first, second], at(1))
        .unwrap()
        .unwrap();
    assert_eq!(policy.mode, ToolMode::Auto);
    assert_eq!(policy.workspace_ids, {
        let mut ids = vec![first, second];
        ids.sort_unstable();
        ids
    });

    let cleared = repository
        .replace_chat_tool_policy(chat_id, ToolMode::Manual, &[], at(2))
        .unwrap()
        .unwrap();
    assert_eq!(cleared.workspace_ids, Vec::<WorkspaceId>::new());
}

#[test]
fn tool_audit_keeps_only_stable_targets_and_public_failure_context() {
    let mut repository = Repository::in_memory().unwrap();
    let chat_id = ChatId::new();
    repository.create_chat(&chat(chat_id, 0)).unwrap();
    let pending = turn(chat_id, Uuid::new_v4(), 1);
    repository.begin_turn(&pending).unwrap();
    let mut activity = ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: pending.attempt.id,
        chat_id,
        assistant_message_id: pending.assistant_message.id,
        tool_name: "update_document".into(),
        read_only: false,
        status: ToolActivityStatus::Proposed,
        approval: ToolApproval::Pending,
        proposed_at: at(2),
        started_at: None,
        completed_at: None,
        result_bytes: None,
        error_code: None,
        target_workspace_id: Some(WorkspaceId::new()),
        target_document_id: Some(koru_ai_core::DocumentId::new()),
        error_message: None,
        agent_label: None,
        agent_summary: None,
        agent_input: None,
        agent_output: None,
        text_offset: None,
        display_target: None,
        live: None,
    };
    repository.create_tool_activity(&activity).unwrap();
    activity.status = ToolActivityStatus::Denied;
    activity.approval = ToolApproval::Denied;
    activity.completed_at = Some(at(3));
    activity.error_code = Some(koru_ai_core::ToolErrorCode::PermissionDenied);
    activity.error_message = Some("The user denied this workspace change.".into());
    repository.update_tool_activity(&activity).unwrap();
    assert_eq!(
        repository.tool_activity(activity.id).unwrap(),
        Some(activity)
    );

    let columns = repository
        .connection()
        .prepare("PRAGMA table_info(tool_invocations)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for forbidden in ["arguments", "results", "query", "title", "path", "content"] {
        assert!(!columns.iter().any(|column| column.contains(forbidden)));
    }
}

#[test]
fn an_agent_row_records_where_the_reply_stood_when_it_began() {
    let mut repository = Repository::in_memory().unwrap();
    let chat_id = ChatId::new();
    repository.create_chat(&chat(chat_id, 0)).unwrap();
    let pending = turn(chat_id, Uuid::new_v4(), 1);
    repository.begin_turn(&pending).unwrap();
    // Bytes, not characters: the surface slices its draft by the same count.
    repository
        .apply_event(
            &AiEvent::TextDelta {
                attempt_id: pending.attempt.id,
                text: "Je vais lire é.".into(),
                at: at(2),
            },
            &[],
        )
        .unwrap();
    let row = |tool_name: &str| ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: pending.attempt.id,
        chat_id,
        assistant_message_id: pending.assistant_message.id,
        tool_name: tool_name.into(),
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
        agent_label: None,
        agent_summary: None,
        agent_input: None,
        agent_output: None,
        text_offset: None,
        display_target: None,
        live: None,
    };
    let agent = row(AgentToolKind::Tool.as_token());
    repository.create_tool_activity(&agent).unwrap();
    let workspace = row("read_document");
    repository.create_tool_activity(&workspace).unwrap();
    assert_eq!(
        repository
            .tool_activity(agent.id)
            .unwrap()
            .unwrap()
            .text_offset,
        Some("Je vais lire é.".len() as u32)
    );
    // Koru's own tool rows keep no offset: their reply reads as before.
    assert_eq!(
        repository
            .tool_activity(workspace.id)
            .unwrap()
            .unwrap()
            .text_offset,
        None
    );
}

/// **A local agent's notice is kept where the reply stood** (koru-ai D69,
/// schema 12): in byte length, in the order notices came, with its reply,
/// and gone with its chat.
#[test]
fn an_agent_notice_records_where_the_reply_stood_and_goes_with_its_chat() {
    let mut repository = Repository::in_memory().unwrap();
    let chat_id = ChatId::new();
    repository.create_chat(&chat(chat_id, 0)).unwrap();
    let pending = turn(chat_id, Uuid::new_v4(), 1);
    repository.begin_turn(&pending).unwrap();
    let notice = |text: &str, offset: i64| koru_ai_core::AgentNotice {
        id: koru_ai_core::AgentNoticeId::new(),
        attempt_id: pending.attempt.id,
        chat_id,
        assistant_message_id: pending.assistant_message.id,
        text: text.into(),
        text_offset: None,
        created_at: at(offset),
    };
    let first = repository
        .create_agent_notice(&notice("Hook failed: exit 127", 2))
        .unwrap();
    assert_eq!(first.text_offset, Some(0), "before any reply text");
    assert_eq!(first.assistant_message_id, pending.assistant_message.id);
    repository
        .apply_event(
            &AiEvent::TextDelta {
                attempt_id: pending.attempt.id,
                text: "Voilà é.".into(),
                at: at(3),
            },
            &[],
        )
        .unwrap();
    let second = repository
        .create_agent_notice(&notice("Second", 4))
        .unwrap();
    assert_eq!(second.text_offset, Some("Voilà é.".len() as u32));
    assert_eq!(
        repository.agent_notices(chat_id).unwrap(),
        vec![first, second]
    );
    // A notice naming another chat's reply is refused, never misfiled.
    let mut foreign = notice("x", 5);
    foreign.chat_id = ChatId::new();
    assert!(repository.create_agent_notice(&foreign).is_err());
    repository.delete_chat(chat_id).unwrap();
    assert!(repository.agent_notices(chat_id).unwrap().is_empty());
}

/// **A pin is the chat's, durable and listed** (schema 12), and like a
/// rename it leaves the chat where it is in the list.
#[test]
fn a_chats_pin_is_durable_listed_and_leaves_its_place() {
    let mut repository = Repository::in_memory().unwrap();
    let chat_id = ChatId::new();
    repository.create_chat(&chat(chat_id, 0)).unwrap();
    assert_eq!(repository.chat(chat_id).unwrap().unwrap().pinned_at, None);
    let pinned = repository
        .set_chat_pinned(chat_id, Some(at(7)))
        .unwrap()
        .unwrap();
    assert_eq!(pinned.pinned_at, Some(at(7)));
    assert_eq!(pinned.updated_at, at(0), "the list order is untouched");
    assert_eq!(repository.chats().unwrap()[0].pinned_at, Some(at(7)));
    let unpinned = repository.set_chat_pinned(chat_id, None).unwrap().unwrap();
    assert_eq!(unpinned.pinned_at, None);
    assert!(
        repository
            .set_chat_pinned(ChatId::new(), Some(at(1)))
            .unwrap()
            .is_none()
    );
}

fn turn(chat_id: ChatId, operation_id: Uuid, offset: i64) -> PendingTurn {
    let user_id = MessageId::new();
    let assistant_id = MessageId::new();
    PendingTurn {
        user_message: Message {
            id: user_id,
            chat_id,
            ordinal: 0,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: "Question".into(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: at(offset),
            completed_at: Some(at(offset)),
        },
        assistant_message: Message {
            id: assistant_id,
            chat_id,
            ordinal: 1,
            role: ChatRole::Assistant,
            state: MessageState::Pending,
            markdown: String::new(),
            origin_message_id: None,
            provider: Some(AiProviderId::parse("mock").unwrap()),
            requested_model: Some("deterministic-v1".into()),
            resolved_model: None,
            error_class: None,
            created_at: at(offset),
            completed_at: None,
        },
        attempt: InferenceAttempt {
            id: InferenceAttemptId::new(),
            operation_id,
            chat_id: Some(chat_id),
            assistant_message_id: Some(assistant_id),
            surface: AiSurface::Standalone,
            intent: AiIntent::Chat,
            provider: AiProviderId::parse("mock").unwrap(),
            requested_model: "deterministic-v1".into(),
            resolved_model: None,
            requested_reasoning: ReasoningChoice::Default,
            effective_reasoning: None,
            prompt_template_version: "mock-v1".into(),
            status: AttemptStatus::Pending,
            queued_at: at(offset),
            context_started_at: None,
            context_completed_at: None,
            provider_started_at: None,
            first_token_at: None,
            completed_at: None,
            request_payload_bytes: None,
            web_search_requested: false,
            web_search_calls: 0,
            context_payload_bytes: 0,
            message_count: 1,
            context_document_count: 0,
            context_message_block_count: 0,
            usage: AiUsage::default(),
            stop_reason: None,
            error_class: None,
            provider_response_id: None,
        },
        attachment_ids: Vec::new(),
    }
}

fn repository_with_chat() -> (Repository, ChatId) {
    let mut repository = Repository::in_memory().unwrap();
    let chat_id = ChatId::new();
    repository.create_chat(&chat(chat_id, 0)).unwrap();
    (repository, chat_id)
}

fn complete_turn(
    repository: &mut Repository,
    chat_id: ChatId,
    ordinal: u32,
    markdown: &str,
    offset: i64,
) -> PendingTurn {
    let mut pending = turn(chat_id, Uuid::new_v4(), offset);
    pending.user_message.ordinal = ordinal;
    pending.user_message.markdown = format!("Question {ordinal}");
    pending.assistant_message.ordinal = ordinal + 1;
    repository.begin_turn(&pending).unwrap();
    repository
        .apply_event(
            &AiEvent::TextDelta {
                attempt_id: pending.attempt.id,
                text: markdown.into(),
                at: at(offset + 1),
            },
            &[],
        )
        .unwrap();
    repository
        .apply_event(
            &AiEvent::Completed {
                attempt_id: pending.attempt.id,
                stop_reason: None,
                at: at(offset + 2),
            },
            &[MessageBlock {
                id: MessageBlockId::new(),
                message_id: pending.assistant_message.id,
                ordinal: 0,
                kind: MessageBlockKind::Paragraph,
                markdown: markdown.into(),
                plain_text: markdown.into(),
            }],
        )
        .unwrap();
    pending
}

#[test]
fn provider_settings_are_optional_then_upsert_as_one_non_secret_row() {
    let mut repository = Repository::in_memory().unwrap();
    assert_eq!(repository.settings().unwrap(), None);

    let mut settings = AiSettings::unconfigured(at(0));
    settings.selected_provider = Some(AiProviderId::parse("openai").unwrap());
    settings.openai.model = Some("gpt-5.6-sol".into());
    settings.openai.key_account = Some("stable-project".into());
    settings.openai.reasoning = ReasoningChoice::Level(ReasoningLevel::High);
    repository.save_settings(&settings).unwrap();
    assert_eq!(repository.settings().unwrap(), Some(settings.clone()));

    settings.updated_at = at(10);
    settings.selected_provider = Some(AiProviderId::parse("ollama").unwrap());
    settings.ollama.model = Some("gpt-oss:20b".into());
    repository.save_settings(&settings).unwrap();
    assert_eq!(repository.settings().unwrap(), Some(settings.clone()));

    settings.updated_at = at(20);
    settings.selected_provider = Some(AiProviderId::parse("koru").unwrap());
    repository.save_settings(&settings).unwrap();
    assert_eq!(repository.settings().unwrap(), Some(settings.clone()));

    // A local agent is a provider a person can make the default (AGENTUX);
    // an unknown provider is still refused before it reaches the row.
    settings.updated_at = at(30);
    settings.selected_provider = Some(AiProviderId::parse("agent-codex").unwrap());
    repository.save_settings(&settings).unwrap();
    assert_eq!(repository.settings().unwrap(), Some(settings.clone()));
    settings.selected_provider = Some(AiProviderId::parse("elsewhere").unwrap());
    assert!(repository.save_settings(&settings).is_err());
    settings.selected_provider = Some(AiProviderId::parse("koru").unwrap());
    assert_eq!(
        repository
            .connection()
            .query_row("SELECT count(*) FROM ai_settings", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn chats_round_trip_through_the_authoritative_model() {
    let (repository, chat_id) = repository_with_chat();
    assert_eq!(repository.chat(chat_id).unwrap(), Some(chat(chat_id, 0)));
    assert!(repository.quick_check().unwrap());
}

#[test]
fn attempts_for_chat_returns_only_message_backed_attempts_for_that_chat() {
    let (mut repository, chat_id) = repository_with_chat();
    let visible = turn(chat_id, Uuid::new_v4(), 1);
    repository.begin_turn(&visible).unwrap();

    let mut detached = visible.attempt.clone();
    detached.id = InferenceAttemptId::new();
    detached.operation_id = Uuid::new_v4();
    detached.assistant_message_id = None;
    detached.queued_at = at(2);
    insert_attempt(repository.connection(), &detached).unwrap();

    let other_chat = ChatId::new();
    repository.create_chat(&chat(other_chat, 3)).unwrap();
    let other = turn(other_chat, Uuid::new_v4(), 4);
    repository.begin_turn(&other).unwrap();

    let attempts = repository.attempts_for_chat(chat_id).unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].id, visible.attempt.id);
    assert_eq!(
        attempts[0].assistant_message_id,
        Some(visible.assistant_message.id)
    );
}

/// A rename changes the title and nothing else: not the recency the Chats
/// list is ordered by, so the chat keeps its place.
#[test]
fn chat_rename_changes_the_title_and_keeps_the_place() {
    let (mut repository, chat_id) = repository_with_chat();
    let before = repository.chat(chat_id).unwrap().unwrap().updated_at;
    assert!(repository.rename_chat(chat_id, "Renamed chat").unwrap());
    let renamed = repository.chat(chat_id).unwrap().unwrap();
    assert_eq!(renamed.title, "Renamed chat");
    assert_eq!(renamed.updated_at, before);
    assert!(!repository.rename_chat(ChatId::new(), "Missing").unwrap());
}

#[test]
fn chat_model_update_is_durable_and_does_not_touch_the_title() {
    let (mut repository, chat_id) = repository_with_chat();
    let selected = ModelSelection {
        provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
        requested_model: "gpt-next".into(),
        default_reasoning: ReasoningChoice::Level(ReasoningLevel::High),
    };
    assert!(repository.update_chat_model(chat_id, &selected).unwrap());
    let updated = repository.chat(chat_id).unwrap().unwrap();
    assert_eq!(updated.title, chat(chat_id, 0).title);
    assert_eq!(updated.current_model, selected);
    assert_eq!(
        updated.updated_at,
        chat(chat_id, 0).updated_at,
        "not activity (D85)"
    );
    assert!(
        !repository
            .update_chat_model(ChatId::new(), &model())
            .unwrap()
    );
}

/// **Only messages count as activity** (koru-ai D85, the user's call
/// 2026-11-03): changing an older chat's model, privacy, folder or tools
/// leaves it below the newer one; a message sent in it moves it up.
#[test]
fn only_a_message_moves_a_chat_in_the_list() {
    let mut repository = Repository::in_memory().unwrap();
    let older = ChatId::new();
    let newer = ChatId::new();
    repository.create_chat(&chat(older, 1)).unwrap();
    repository.create_chat(&chat(newer, 2)).unwrap();
    let order = |repository: &Repository| {
        repository
            .chats()
            .unwrap()
            .into_iter()
            .map(|chat| chat.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&repository), [newer, older]);

    let other = ModelSelection {
        provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
        requested_model: "gpt-other".into(),
        default_reasoning: ReasoningChoice::Default,
    };
    assert!(repository.update_chat_model(older, &other).unwrap());
    repository.set_chat_private(older, true).unwrap().unwrap();
    repository
        .set_chat_working_folder(older, Some("D:/work"))
        .unwrap()
        .unwrap();
    repository
        .replace_chat_tool_policy(older, ToolMode::Auto, &[], at(50))
        .unwrap()
        .unwrap();
    assert_eq!(order(&repository), [newer, older], "none of it is activity");
    assert_eq!(
        repository.chat(older).unwrap().unwrap().updated_at,
        at(1),
        "its *when* has not moved"
    );

    complete_turn(&mut repository, older, 0, "Moving up", 60);
    assert_eq!(order(&repository), [older, newer], "a message is");
}

/// **A reply that ends is activity** (koru-ai D85): the chat's place in the
/// list moves to when its reply arrived, as it moved when the message was
/// sent — whether the reply completed, failed or was stopped — and a late
/// event for a reply already ended moves nothing.
#[test]
fn a_reply_that_ends_moves_its_chat_to_when_it_arrived() {
    let (mut repository, chat_id) = repository_with_chat();
    complete_turn(&mut repository, chat_id, 0, "Done", 10);
    let chat = repository.chat(chat_id).unwrap().unwrap();
    assert_eq!(chat.updated_at, at(12), "the completion, not the send");

    let mut pending = turn(chat_id, Uuid::new_v4(), 20);
    pending.user_message.ordinal = 2;
    pending.assistant_message.ordinal = 3;
    let attempt_id = pending.attempt.id;
    repository.begin_turn(&pending).unwrap();
    assert_eq!(
        repository.chat(chat_id).unwrap().unwrap().updated_at,
        at(20)
    );
    repository
        .apply_event(
            &AiEvent::Cancelled {
                attempt_id,
                at: at(25),
            },
            &[],
        )
        .unwrap();
    assert_eq!(
        repository.chat(chat_id).unwrap().unwrap().updated_at,
        at(25)
    );
    repository
        .apply_event(
            &AiEvent::Cancelled {
                attempt_id,
                at: at(30),
            },
            &[],
        )
        .unwrap();
    assert_eq!(
        repository.chat(chat_id).unwrap().unwrap().updated_at,
        at(25),
        "a reply already ended moves nothing"
    );
}

#[test]
fn chat_list_is_most_recent_first() {
    let mut repository = Repository::in_memory().unwrap();
    let older = ChatId::new();
    let newer = ChatId::new();
    repository.create_chat(&chat(older, 1)).unwrap();
    repository.create_chat(&chat(newer, 2)).unwrap();

    let chats = repository.chats().unwrap();
    assert_eq!(
        chats.iter().map(|chat| chat.id).collect::<Vec<_>>(),
        [newer, older]
    );
}

#[test]
fn a_turn_is_atomic_and_duplicate_operation_ids_return_the_original_attempt() {
    let (mut repository, chat_id) = repository_with_chat();
    let operation = Uuid::new_v4();
    let original = turn(chat_id, operation, 1);
    let accepted = repository.begin_turn(&original).unwrap();

    let replay = turn(chat_id, operation, 2);
    let replayed = repository.begin_turn(&replay).unwrap();
    assert_eq!(replayed.id, accepted.id);
    assert_eq!(repository.messages(chat_id).unwrap().len(), 2);
    assert_eq!(
        repository.attempt_by_operation(operation).unwrap(),
        Some(original.attempt)
    );
}

#[test]
fn a_failed_second_insert_rolls_back_the_first_message() {
    let (mut repository, chat_id) = repository_with_chat();
    let mut pending = turn(chat_id, Uuid::new_v4(), 1);
    pending.assistant_message.origin_message_id = Some(MessageId::new());

    assert!(repository.begin_turn(&pending).is_err());
    assert!(repository.messages(chat_id).unwrap().is_empty());
    assert!(
        repository
            .attempt_by_operation(pending.attempt.operation_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn feedback_is_append_only_and_the_latest_non_cleared_value_is_current() {
    let (mut repository, chat_id) = repository_with_chat();
    let pending = turn(chat_id, Uuid::new_v4(), 1);
    let message = pending.assistant_message.id;
    repository.begin_turn(&pending).unwrap();
    for (offset, value) in [
        (2, FeedbackValue::Good),
        (3, FeedbackValue::Cleared),
        (4, FeedbackValue::Bad),
    ] {
        repository
            .append_feedback(&FeedbackEvent {
                id: FeedbackEventId::new(),
                chat_id: Some(chat_id),
                message_id: Some(message),
                attempt_id: Some(pending.attempt.id),
                value,
                surface: AiSurface::Standalone,
                created_at: at(offset),
            })
            .unwrap();
    }
    let current = repository.feedback(chat_id).unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].value, FeedbackValue::Bad);
    assert_eq!(
        repository
            .connection()
            .query_row("SELECT count(*) FROM feedback_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        3
    );
}

#[test]
fn branch_retry_and_export_preserve_history_and_retry_identity() {
    let (mut repository, chat_id) = repository_with_chat();
    let original = turn(chat_id, Uuid::new_v4(), 1);
    repository.begin_turn(&original).unwrap();
    repository
        .apply_event(
            &AiEvent::Failed {
                attempt_id: original.attempt.id,
                error: koru_ai_core::AiError::new(AiErrorClass::Network, "safe", true),
                at: at(2),
            },
            &[],
        )
        .unwrap();

    let retry_message = Message {
        id: MessageId::new(),
        chat_id,
        ordinal: 2,
        role: ChatRole::Assistant,
        state: MessageState::Pending,
        markdown: String::new(),
        origin_message_id: Some(original.assistant_message.id),
        provider: Some(AiProviderId::parse("mock").unwrap()),
        requested_model: Some("deterministic-v1".into()),
        resolved_model: None,
        error_class: None,
        created_at: at(3),
        completed_at: None,
    };
    let mut retry_attempt = original.attempt.clone();
    retry_attempt.id = InferenceAttemptId::new();
    retry_attempt.operation_id = Uuid::new_v4();
    retry_attempt.assistant_message_id = Some(retry_message.id);
    retry_attempt.queued_at = at(3);
    let accepted = repository
        .begin_retry(&PendingRetry {
            assistant_message: retry_message.clone(),
            attempt: retry_attempt.clone(),
        })
        .unwrap();
    assert_eq!(accepted.id, retry_attempt.id);
    assert_eq!(
        repository.messages(chat_id).unwrap().len(),
        3,
        "failed response remains beside retry"
    );

    let branch_id = ChatId::new();
    let copied_user = Message {
        id: MessageId::new(),
        chat_id: branch_id,
        origin_message_id: Some(original.user_message.id),
        ..original.user_message.clone()
    };
    let copied_failed = Message {
        id: MessageId::new(),
        chat_id: branch_id,
        origin_message_id: Some(original.assistant_message.id),
        ..repository
            .message(original.assistant_message.id)
            .unwrap()
            .unwrap()
    };
    let branch = Chat {
        id: branch_id,
        title: "Branch".into(),
        private: false,
        current_model: model(),
        branched_from: Some(BranchOrigin {
            chat_id,
            through_message_id: original.assistant_message.id,
        }),
        agent_working_folder: None,
        agent_mode: None,
        agent_fast: None,
        pinned_at: None,
        created_at: at(4),
        updated_at: at(4),
    };
    repository
        .create_branch(&BranchCopy {
            chat: branch,
            tool_policy: ChatToolPolicy {
                chat_id: branch_id,
                mode: ToolMode::Manual,
                workspace_ids: Vec::new(),
            },
            messages: vec![copied_user, copied_failed],
            blocks: Vec::new(),
            citations: Vec::new(),
            pinned_documents: Vec::new(),
            attachments: Vec::new(),
        })
        .unwrap();
    assert_eq!(repository.messages(branch_id).unwrap().len(), 2);

    let export = DocumentExport {
        id: DocumentExportId::new(),
        idempotency_key: Uuid::new_v4(),
        chat_id,
        through_message_id: original.assistant_message.id,
        destination: DocumentDestination {
            profile_id: ProfileId::new(),
            workspace_id: WorkspaceId::new(),
            parent_document_id: None,
            index: Some(0),
            title: "Saved".into(),
        },
        status: ExportStatus::Pending,
        document_id: None,
        error_class: None,
        created_at: at(5),
        completed_at: None,
    };
    assert_eq!(repository.begin_export(&export).unwrap(), export);
    let document = DocumentId::new();
    let complete = repository
        .finish_export(export.id, Some(document), None, at(6))
        .unwrap();
    assert_eq!(complete.status, ExportStatus::Complete);
    assert_eq!(
        repository
            .finish_export(export.id, None, Some(AiErrorClass::Storage), at(7))
            .unwrap(),
        complete,
        "terminal export is absorbing"
    );
}

#[test]
fn context_measurements_and_provenance_commit_atomically_before_provider_work() {
    let (mut repository, chat_id) = repository_with_chat();
    let first = turn(chat_id, Uuid::new_v4(), 1);
    let source_message = first.assistant_message.id;
    let first_attempt = first.attempt.id;
    repository.begin_turn(&first).unwrap();
    let source_block = MessageBlockId::new();
    repository
        .apply_event(
            &AiEvent::Completed {
                attempt_id: first_attempt,
                stop_reason: None,
                at: at(2),
            },
            &[MessageBlock {
                id: source_block,
                message_id: source_message,
                ordinal: 0,
                kind: MessageBlockKind::Paragraph,
                markdown: "Reusable answer".into(),
                plain_text: "Reusable answer".into(),
            }],
        )
        .unwrap();

    let mut second = turn(chat_id, Uuid::new_v4(), 3);
    second.user_message.ordinal = 2;
    second.assistant_message.ordinal = 3;
    let target_message = second.user_message.id;
    let attempt_id = second.attempt.id;
    repository.begin_turn(&second).unwrap();
    let document_id = DocumentId::new();
    let context = ContextCommit {
        attempt_id,
        started_at: at(4),
        completed_at: at(5),
        measurements: RequestMeasurements {
            message_count: 3,
            context_document_count: 1,
            context_message_block_count: 1,
            context_payload_bytes: 27,
        },
        pinned_documents: vec![PinnedDocument {
            chat_id,
            document_id,
            display_title: "Project brief".into(),
            pinned: true,
            first_used_at: at(4),
            last_used_at: at(5),
        }],
        document_refs: vec![MessageDocumentRef {
            message_id: target_message,
            document_id,
            relation: DocumentRelation::Context,
            display_title: "Project brief".into(),
            source_revision: Some(StoreRevision(vec![1, 2, 3])),
            used_at: at(5),
        }],
        message_block_refs: vec![MessageBlockRef {
            message_id: target_message,
            source_block_id: source_block,
            used_at: at(5),
        }],
        replace_pinned_documents: false,
    };

    let updated = repository.commit_context(&context).unwrap();
    assert_eq!(updated.context_started_at, Some(at(4)));
    assert_eq!(updated.context_completed_at, Some(at(5)));
    assert_eq!(updated.context_payload_bytes, 27);
    assert_eq!(updated.context_document_count, 1);
    assert_eq!(updated.context_message_block_count, 1);
    assert_eq!(
        repository.pinned_documents(chat_id).unwrap(),
        context.pinned_documents
    );
    assert_eq!(
        repository
            .message_block_context(source_block)
            .unwrap()
            .unwrap()
            .markdown,
        "Reusable answer"
    );
    repository.set_chat_private(chat_id, true).unwrap().unwrap();
    assert!(
        repository
            .message_block_context(source_block)
            .unwrap()
            .unwrap()
            .chat_private
    );
    assert_eq!(
        repository
            .connection()
            .query_row("SELECT count(*) FROM message_document_refs", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
    assert_eq!(
        repository
            .connection()
            .query_row("SELECT count(*) FROM message_block_refs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn normalized_events_update_attempt_and_message_in_the_same_transactions() {
    let (mut repository, chat_id) = repository_with_chat();
    let mut pending = turn(chat_id, Uuid::new_v4(), 1);
    pending.attempt.web_search_requested = true;
    let attempt_id = pending.attempt.id;
    repository.begin_turn(&pending).unwrap();

    repository
        .apply_event(
            &AiEvent::Started {
                attempt_id,
                provider_response_id: Some("response-1".into()),
                request_payload_bytes: Some(287),
                resolved_model: Some("deterministic-v1.0".into()),
                effective_reasoning: Some(ReasoningChoice::Off),
                at: at(2),
            },
            &[],
        )
        .unwrap();
    repository
        .apply_event(
            &AiEvent::WebSearch {
                attempt_id,
                call_id: "search-1".into(),
                phase: WebSearchPhase::Started,
                at: at(2),
            },
            &[],
        )
        .unwrap();
    repository
        .apply_event(
            &AiEvent::Citation {
                attempt_id,
                source: SourceLink {
                    url: "https://example.com/source".into(),
                    title: "Example source".into(),
                },
                at: at(3),
            },
            &[],
        )
        .unwrap();
    repository
        .apply_event(
            &AiEvent::WebSearch {
                attempt_id,
                call_id: "search-1".into(),
                phase: WebSearchPhase::Completed,
                at: at(3),
            },
            &[],
        )
        .unwrap();
    repository
        .apply_event(
            &AiEvent::TextDelta {
                attempt_id,
                text: "Hello".into(),
                at: at(3),
            },
            &[],
        )
        .unwrap();
    repository
        .apply_event(
            &AiEvent::Usage {
                attempt_id,
                usage: AiUsage {
                    prompt_tokens: Some(3),
                    cached_prompt_tokens: Some(1),
                    completion_tokens: Some(2),
                    reasoning_tokens: None,
                    total_tokens: Some(5),
                    source: UsageSource::ProviderReported,
                },
            },
            &[],
        )
        .unwrap();
    let completed = repository
        .apply_event(
            &AiEvent::Completed {
                attempt_id,
                stop_reason: Some("stop".into()),
                at: at(4),
            },
            &[],
        )
        .unwrap();

    assert_eq!(completed.status, AttemptStatus::Complete);
    assert_eq!(completed.provider_started_at, Some(at(2)));
    assert_eq!(completed.first_token_at, Some(at(3)));
    assert_eq!(
        completed.resolved_model.as_deref(),
        Some("deterministic-v1.0")
    );
    assert_eq!(completed.usage.total_tokens, Some(5));
    assert!(completed.web_search_requested);
    assert_eq!(completed.web_search_calls, 1);
    assert_eq!(completed.request_payload_bytes, Some(287));
    assert_eq!(
        repository.citations(chat_id).unwrap(),
        vec![(
            pending.assistant_message.id,
            SourceLink {
                url: "https://example.com/source".into(),
                title: "Example source".into(),
            },
        )]
    );
    let messages = repository.messages(chat_id).unwrap();
    assert_eq!(messages[1].markdown, "Hello");
    assert_eq!(messages[1].state, MessageState::Complete);
    assert_eq!(messages[1].completed_at, Some(at(4)));
}

#[test]
fn terminal_state_ignores_late_provider_events() {
    let (mut repository, chat_id) = repository_with_chat();
    let pending = turn(chat_id, Uuid::new_v4(), 1);
    let attempt_id = pending.attempt.id;
    repository.begin_turn(&pending).unwrap();
    repository
        .apply_event(
            &AiEvent::Completed {
                attempt_id,
                stop_reason: None,
                at: at(2),
            },
            &[],
        )
        .unwrap();

    let unchanged = repository
        .apply_event(
            &AiEvent::TextDelta {
                attempt_id,
                text: "too late".into(),
                at: at(3),
            },
            &[],
        )
        .unwrap();
    assert_eq!(unchanged.status, AttemptStatus::Complete);
    assert_eq!(repository.messages(chat_id).unwrap()[1].markdown, "");
}

#[test]
fn cancellation_is_idempotent_and_maps_the_message_to_interrupted() {
    let (mut repository, chat_id) = repository_with_chat();
    let pending = turn(chat_id, Uuid::new_v4(), 1);
    let attempt_id = pending.attempt.id;
    repository.begin_turn(&pending).unwrap();
    let cancel = AiEvent::Cancelled {
        attempt_id,
        at: at(2),
    };

    assert_eq!(
        repository.apply_event(&cancel, &[]).unwrap().status,
        AttemptStatus::Cancelled
    );
    assert_eq!(
        repository.apply_event(&cancel, &[]).unwrap().status,
        AttemptStatus::Cancelled
    );
    let assistant = &repository.messages(chat_id).unwrap()[1];
    assert_eq!(assistant.state, MessageState::Interrupted);
    assert_eq!(assistant.error_class, Some(AiErrorClass::Cancelled));
}

#[test]
fn startup_recovery_interrupts_pending_and_streaming_work_once() {
    let mut repository = Repository::in_memory().unwrap();
    let first_chat = ChatId::new();
    let second_chat = ChatId::new();
    repository.create_chat(&chat(first_chat, 0)).unwrap();
    repository.create_chat(&chat(second_chat, 0)).unwrap();
    let first = turn(first_chat, Uuid::new_v4(), 1);
    let second = turn(second_chat, Uuid::new_v4(), 1);
    repository.begin_turn(&first).unwrap();
    repository.begin_turn(&second).unwrap();
    repository
        .apply_event(
            &AiEvent::Started {
                attempt_id: second.attempt.id,
                provider_response_id: None,
                request_payload_bytes: None,
                resolved_model: None,
                effective_reasoning: None,
                at: at(2),
            },
            &[],
        )
        .unwrap();

    assert_eq!(
        repository.recover_interrupted(at(3)).unwrap(),
        RecoveryCounts {
            attempts: 2,
            messages: 2,
        }
    );
    assert_eq!(
        repository
            .attempt(first.attempt.id)
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Interrupted
    );
    assert_eq!(
        repository
            .attempt(second.attempt.id)
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Interrupted
    );
    assert_eq!(
        repository.recover_interrupted(at(4)).unwrap(),
        RecoveryCounts::default()
    );
}

#[test]
fn finalized_blocks_feed_fts_and_cannot_be_mutated() {
    let (mut repository, chat_id) = repository_with_chat();
    let pending = turn(chat_id, Uuid::new_v4(), 1);
    repository.begin_turn(&pending).unwrap();
    let block_id = MessageBlockId::new();
    let block = MessageBlock {
        id: block_id,
        message_id: pending.assistant_message.id,
        ordinal: 0,
        kind: MessageBlockKind::Paragraph,
        markdown: "Answer".into(),
        plain_text: "Answer".into(),
    };
    repository
        .apply_event(
            &AiEvent::Completed {
                attempt_id: pending.attempt.id,
                stop_reason: None,
                at: at(2),
            },
            &[block],
        )
        .unwrap();

    let matches: i64 = repository
        .connection()
        .query_row(
            "SELECT count(*) FROM message_blocks_fts WHERE message_blocks_fts MATCH 'answer'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(matches, 1);
    assert_eq!(repository.blocks(chat_id).unwrap()[0].id, block_id);
    assert!(
        repository
            .connection()
            .execute(
                "UPDATE message_blocks SET plain_text = 'changed' WHERE id = ?1",
                [block_id.to_storage()],
            )
            .is_err()
    );
}

#[test]
fn search_returns_one_highlighted_best_block_per_finalized_response() {
    let (mut repository, chat_id) = repository_with_chat();
    let pending = turn(chat_id, Uuid::new_v4(), 1);
    repository.begin_turn(&pending).unwrap();
    let weaker = MessageBlockId::new();
    let stronger = MessageBlockId::new();
    repository
        .apply_event(
            &AiEvent::Completed {
                attempt_id: pending.attempt.id,
                stop_reason: None,
                at: at(2),
            },
            &[
                MessageBlock {
                    id: weaker,
                    message_id: pending.assistant_message.id,
                    ordinal: 0,
                    kind: MessageBlockKind::Paragraph,
                    markdown: "Search once".into(),
                    plain_text: "Search once".into(),
                },
                MessageBlock {
                    id: stronger,
                    message_id: pending.assistant_message.id,
                    ordinal: 1,
                    kind: MessageBlockKind::Paragraph,
                    markdown: "Search search answer".into(),
                    plain_text: "Search search answer".into(),
                },
            ],
        )
        .unwrap();
    repository.set_chat_private(chat_id, true).unwrap().unwrap();

    let page = repository
        .search(&AiSearchQuery {
            text: "search".into(),
            limit: 50,
            cursor: None,
            visibility: ChatAccessLens::UserSurface,
        })
        .unwrap();
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].message_id, pending.assistant_message.id);
    assert!(matches!(page.hits[0].block_id, id if id == weaker || id == stronger));
    assert!(!page.hits[0].spans.is_empty());
    let span = &page.hits[0].spans[0];
    assert_eq!(
        page.hits[0].snippet[span.start..span.end].to_ascii_lowercase(),
        "search"
    );
    assert!(page.next_cursor.is_none());

    let empty = repository
        .search(&AiSearchQuery {
            text: "not-present".into(),
            limit: 50,
            cursor: None,
            visibility: ChatAccessLens::UserSurface,
        })
        .unwrap();
    assert!(empty.hits.is_empty());
}

#[test]
fn chat_tools_apply_one_privacy_lens_to_list_search_and_read() {
    let mut repository = Repository::in_memory().unwrap();
    let public_id = ChatId::new();
    let private_id = ChatId::new();
    let calling_id = ChatId::new();
    for (id, title, offset) in [
        (public_id, "Public chat", 0),
        (private_id, "Private chat", 10),
        (calling_id, "Calling chat", 20),
    ] {
        let mut value = chat(id, offset);
        value.title = title.into();
        repository.create_chat(&value).unwrap();
        complete_turn(&mut repository, id, 0, "sharedneedle answer", offset + 1);
    }
    repository.set_chat_private(private_id, true).unwrap();
    repository.set_chat_private(calling_id, true).unwrap();

    let lens = ChatAccessLens::ToolSurface {
        calling_chat: Some(calling_id),
    };
    let listed = list_chats(&repository, lens, ListChatsToolRequest::default()).unwrap();
    let listed_ids = listed
        .chats
        .iter()
        .map(|chat| chat.chat_id)
        .collect::<Vec<_>>();
    assert!(listed_ids.contains(&public_id));
    assert!(listed_ids.contains(&calling_id));
    assert!(!listed_ids.contains(&private_id));

    let searched = search_chats(
        &repository,
        lens,
        SearchChatsToolRequest {
            query: "sharedneedle".into(),
            cursor: None,
            page_size: None,
        },
    )
    .unwrap();
    let searched_ids = searched
        .hits
        .iter()
        .map(|hit| hit.chat_id)
        .collect::<Vec<_>>();
    assert!(searched_ids.contains(&public_id));
    assert!(searched_ids.contains(&calling_id));
    assert!(!searched_ids.contains(&private_id));

    let own = read_chat(
        &repository,
        lens,
        ReadChatToolRequest {
            chat_id: calling_id,
            cursor: None,
            page_size: None,
        },
    )
    .unwrap();
    assert_eq!(own.chat_id, calling_id);
    let private_error = read_chat(
        &repository,
        lens,
        ReadChatToolRequest {
            chat_id: private_id,
            cursor: None,
            page_size: None,
        },
    )
    .unwrap_err();
    let missing_error = read_chat(
        &repository,
        lens,
        ReadChatToolRequest {
            chat_id: ChatId::new(),
            cursor: None,
            page_size: None,
        },
    )
    .unwrap_err();
    assert_eq!(private_error.public_message, missing_error.public_message);
    assert_eq!(private_error.class, missing_error.class);
}

#[test]
fn read_chat_pages_backwards_by_ordinal_and_bounds_each_message() {
    let (mut repository, chat_id) = repository_with_chat();
    complete_turn(&mut repository, chat_id, 0, "First answer", 1);
    complete_turn(&mut repository, chat_id, 2, "Second answer", 10);
    let long = format!("{}é", "a".repeat(16 * 1024 - 1));
    complete_turn(&mut repository, chat_id, 4, &long, 20);
    let lens = ChatAccessLens::ToolSurface {
        calling_chat: Some(chat_id),
    };

    let newest = read_chat(
        &repository,
        lens,
        ReadChatToolRequest {
            chat_id,
            cursor: None,
            page_size: Some(2),
        },
    )
    .unwrap();
    assert_eq!(newest.total_messages, 6);
    assert_eq!(
        newest
            .messages
            .iter()
            .map(|message| message.ordinal)
            .collect::<Vec<_>>(),
        vec![4, 5]
    );
    assert_eq!(newest.next_cursor.as_deref(), Some("4"));
    assert!(newest.messages[1].truncated);
    assert_eq!(newest.messages[1].markdown.len(), 16 * 1024 - 1);

    let previous = read_chat(
        &repository,
        lens,
        ReadChatToolRequest {
            chat_id,
            cursor: newest.next_cursor,
            page_size: Some(2),
        },
    )
    .unwrap();
    assert_eq!(
        previous
            .messages
            .iter()
            .map(|message| message.ordinal)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(previous.next_cursor.as_deref(), Some("2"));
}

#[test]
fn invalid_finalized_blocks_roll_back_the_terminal_event() {
    let (mut repository, chat_id) = repository_with_chat();
    let pending = turn(chat_id, Uuid::new_v4(), 1);
    repository.begin_turn(&pending).unwrap();
    let invalid = MessageBlock {
        id: MessageBlockId::new(),
        message_id: MessageId::new(),
        ordinal: 0,
        kind: MessageBlockKind::Paragraph,
        markdown: "wrong message".into(),
        plain_text: "wrong message".into(),
    };

    assert!(
        repository
            .apply_event(
                &AiEvent::Completed {
                    attempt_id: pending.attempt.id,
                    stop_reason: None,
                    at: at(2),
                },
                &[invalid],
            )
            .is_err()
    );
    assert_eq!(
        repository
            .attempt(pending.attempt.id)
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Pending
    );
    assert_eq!(
        repository.messages(chat_id).unwrap()[1].state,
        MessageState::Pending
    );
    assert!(repository.blocks(chat_id).unwrap().is_empty());
}

#[test]
fn deleting_a_chat_cascades_content_but_retains_anonymous_attempt_metrics() {
    let (mut repository, chat_id) = repository_with_chat();
    let pending = turn(chat_id, Uuid::new_v4(), 1);
    repository.begin_turn(&pending).unwrap();
    let block = MessageBlock {
        id: MessageBlockId::new(),
        message_id: pending.assistant_message.id,
        ordinal: 0,
        kind: MessageBlockKind::Paragraph,
        markdown: "Answer".into(),
        plain_text: "Answer".into(),
    };
    repository
        .apply_event(
            &AiEvent::Completed {
                attempt_id: pending.attempt.id,
                stop_reason: None,
                at: at(2),
            },
            &[block],
        )
        .unwrap();

    repository
        .connection()
        .execute("DELETE FROM chats WHERE id = ?1", [chat_id.to_storage()])
        .unwrap();

    let messages: i64 = repository
        .connection()
        .query_row("SELECT count(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    let blocks: i64 = repository
        .connection()
        .query_row("SELECT count(*) FROM message_blocks", [], |row| row.get(0))
        .unwrap();
    let indexed: i64 = repository
        .connection()
        .query_row("SELECT count(*) FROM message_blocks_fts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!((messages, blocks, indexed), (0, 0, 0));

    let attempt = repository
        .attempt(pending.attempt.id)
        .unwrap()
        .expect("usage attempt retained");
    assert_eq!(attempt.status, AttemptStatus::Complete);
    assert_eq!(attempt.chat_id, None);
    assert_eq!(attempt.assistant_message_id, None);
}

#[test]
fn usage_reports_nullable_provider_fields_and_survives_history_until_cleared() {
    let (mut repository, chat_id) = repository_with_chat();
    let pending = turn(chat_id, Uuid::new_v4(), 1);
    let attempt_id = pending.attempt.id;
    let assistant_id = pending.assistant_message.id;
    repository.begin_turn(&pending).unwrap();
    repository
        .commit_context(&ContextCommit {
            attempt_id,
            started_at: at(1),
            completed_at: at(2),
            measurements: RequestMeasurements {
                message_count: 1,
                context_document_count: 0,
                context_message_block_count: 0,
                context_payload_bytes: 17,
            },
            pinned_documents: Vec::new(),
            document_refs: Vec::new(),
            message_block_refs: Vec::new(),
            replace_pinned_documents: false,
        })
        .unwrap();
    repository
        .apply_event(
            &AiEvent::Started {
                attempt_id,
                provider_response_id: Some("safe-response-id".into()),
                request_payload_bytes: None,
                resolved_model: Some("deterministic-v1.0".into()),
                effective_reasoning: Some(ReasoningChoice::Off),
                at: at(3),
            },
            &[],
        )
        .unwrap();
    repository
        .apply_event(
            &AiEvent::TextDelta {
                attempt_id,
                text: "Private answer".into(),
                at: at(4),
            },
            &[],
        )
        .unwrap();
    repository
        .apply_event(
            &AiEvent::Usage {
                attempt_id,
                usage: AiUsage {
                    prompt_tokens: Some(11),
                    cached_prompt_tokens: None,
                    completion_tokens: Some(5),
                    reasoning_tokens: None,
                    total_tokens: Some(16),
                    source: UsageSource::ProviderReported,
                },
            },
            &[],
        )
        .unwrap();
    repository
        .apply_event(
            &AiEvent::Completed {
                attempt_id,
                stop_reason: Some("stop".into()),
                at: at(5),
            },
            &[MessageBlock {
                id: MessageBlockId::new(),
                message_id: assistant_id,
                ordinal: 0,
                kind: MessageBlockKind::Paragraph,
                markdown: "Private answer".into(),
                plain_text: "Private answer".into(),
            }],
        )
        .unwrap();
    repository
        .append_feedback(&FeedbackEvent {
            id: FeedbackEventId::new(),
            chat_id: Some(chat_id),
            message_id: Some(assistant_id),
            attempt_id: Some(attempt_id),
            value: FeedbackValue::Good,
            surface: AiSurface::Standalone,
            created_at: at(6),
        })
        .unwrap();

    assert_eq!(
        repository
            .attempt_for_message(assistant_id)
            .unwrap()
            .map(|attempt| attempt.id),
        Some(attempt_id)
    );
    let report = repository.usage(Some(chat_id)).unwrap();
    let current = report.current_chat.expect("current chat totals");
    assert_eq!(current.attempts, 1);
    assert_eq!(current.context_payload_bytes, 17);
    assert_eq!(current.prompt_tokens, Some(11));
    assert_eq!(current.cached_prompt_tokens, None);
    assert_eq!(current.average_time_to_first_token_ms, Some(1));
    assert_eq!(current.good_feedback, 1);
    assert_eq!(report.groups.len(), 1);
    assert_eq!(report.groups[0].reasoning, ReasoningChoice::Off);

    assert!(repository.delete_chat(chat_id).unwrap());
    assert!(!repository.delete_chat(chat_id).unwrap());
    let retained = repository.usage(None).unwrap();
    assert_eq!(retained.groups.len(), 1);
    assert_eq!(retained.groups[0].totals.good_feedback, 1);
    assert_eq!(
        repository
            .search(&AiSearchQuery {
                text: "Private".into(),
                limit: 10,
                cursor: None,
                visibility: ChatAccessLens::UserSurface,
            })
            .unwrap()
            .hits
            .len(),
        0
    );

    let cleared = repository.clear_usage().unwrap();
    assert_eq!(cleared.attempts, 1);
    assert_eq!(cleared.feedback_events, 1);
    assert!(repository.usage(None).unwrap().groups.is_empty());
}

fn staged(chat_id: ChatId, name: &str, offset: i64) -> koru_ai_core::AttachmentRecord {
    koru_ai_core::AttachmentRecord {
        chat_id,
        attachment: koru_ai_core::ChatAttachment {
            id: koru_ai_core::ChatAttachmentId::new(),
            message_id: None,
            ordinal: None,
            kind: koru_ai_core::AttachmentKind::Image,
            source: koru_ai_core::AttachmentSource::Clipboard,
            file_name: name.into(),
            media_type: "image/png".into(),
            byte_count: 1_024,
            dimensions: Some((40, 30)),
            origin: None,
            created_at: at(offset),
        },
        has_sent_copy: false,
        content_blake3: "a".repeat(64),
    }
}

#[test]
fn a_staged_attachment_is_bound_to_the_user_message_in_the_turn_transaction() {
    let (mut repository, chat_id) = repository_with_chat();
    let first = staged(chat_id, "first.png", 1);
    let second = staged(chat_id, "second.png", 2);
    let kept = staged(chat_id, "kept.png", 3);
    for record in [&first, &second, &kept] {
        repository.stage_attachment(record).unwrap();
    }
    assert_eq!(
        repository.attachment(first.attachment.id).unwrap(),
        Some(first.clone())
    );

    let mut pending = turn(chat_id, Uuid::new_v4(), 10);
    // Bound in the order the send names them, not the order they were added.
    pending.attachment_ids = vec![second.attachment.id, first.attachment.id];
    repository.begin_turn(&pending).unwrap();

    let listed = repository.attachments_for_chat(chat_id).unwrap();
    let names: Vec<_> = listed.iter().map(|item| item.file_name.as_str()).collect();
    assert_eq!(names, ["second.png", "first.png", "kept.png"]);
    assert_eq!(listed[0].message_id, Some(pending.user_message.id));
    assert_eq!(listed[0].ordinal, Some(0));
    assert_eq!(listed[1].ordinal, Some(1));
    assert_eq!(listed[2].message_id, None, "the one not named stays staged");
}

#[test]
fn a_turn_naming_an_unknown_or_foreign_attachment_writes_nothing() {
    let (mut repository, chat_id) = repository_with_chat();
    let other_chat = ChatId::new();
    repository.create_chat(&chat(other_chat, 0)).unwrap();
    let foreign = staged(other_chat, "foreign.png", 1);
    repository.stage_attachment(&foreign).unwrap();

    for ids in [
        vec![koru_ai_core::ChatAttachmentId::new()],
        vec![foreign.attachment.id],
    ] {
        let mut pending = turn(chat_id, Uuid::new_v4(), 10);
        pending.attachment_ids = ids;
        assert!(repository.begin_turn(&pending).is_err());
        assert!(repository.messages(chat_id).unwrap().is_empty());
    }
    assert_eq!(
        repository
            .attachment(foreign.attachment.id)
            .unwrap()
            .unwrap()
            .attachment
            .message_id,
        None
    );
}

#[test]
fn only_a_staged_attachment_can_be_removed() {
    let (mut repository, chat_id) = repository_with_chat();
    let sent = staged(chat_id, "sent.png", 1);
    let draft = staged(chat_id, "draft.png", 2);
    repository.stage_attachment(&sent).unwrap();
    repository.stage_attachment(&draft).unwrap();
    let mut pending = turn(chat_id, Uuid::new_v4(), 10);
    pending.attachment_ids = vec![sent.attachment.id];
    repository.begin_turn(&pending).unwrap();

    assert_eq!(
        repository
            .remove_staged_attachment(chat_id, sent.attachment.id)
            .unwrap(),
        RemovedAttachment::Sent
    );
    assert_eq!(
        repository
            .remove_staged_attachment(ChatId::new(), draft.attachment.id)
            .unwrap(),
        RemovedAttachment::NotFound,
        "another chat cannot remove it"
    );
    assert_eq!(
        repository
            .remove_staged_attachment(chat_id, draft.attachment.id)
            .unwrap(),
        RemovedAttachment::Removed
    );
    assert_eq!(
        repository
            .remove_staged_attachment(chat_id, draft.attachment.id)
            .unwrap(),
        RemovedAttachment::NotFound
    );
}

#[test]
fn old_staged_rows_are_swept_and_the_known_folders_are_listed() {
    let (mut repository, chat_id) = repository_with_chat();
    let old = staged(chat_id, "old.png", 1);
    let fresh = staged(chat_id, "fresh.png", 100);
    let sent = staged(chat_id, "sent.png", 2);
    for record in [&old, &fresh, &sent] {
        repository.stage_attachment(record).unwrap();
    }
    let mut pending = turn(chat_id, Uuid::new_v4(), 10);
    pending.attachment_ids = vec![sent.attachment.id];
    repository.begin_turn(&pending).unwrap();

    assert_eq!(
        repository.remove_staged_attachments_before(at(50)).unwrap(),
        1
    );
    let known = repository.attachment_dirs_known().unwrap();
    assert!(known.chats.contains(&chat_id));
    assert!(known.attachments.contains(&fresh.attachment.id));
    assert!(known.attachments.contains(&sent.attachment.id));
    assert!(!known.attachments.contains(&old.attachment.id));

    // Deleting the chat takes every row with it.
    repository.delete_chat(chat_id).unwrap();
    assert!(
        repository
            .attachment_dirs_known()
            .unwrap()
            .attachments
            .is_empty()
    );
}

#[test]
fn a_branch_carries_its_copied_attachments_under_new_ids() {
    let (mut repository, chat_id) = repository_with_chat();
    let sent = staged(chat_id, "sent.png", 1);
    repository.stage_attachment(&sent).unwrap();
    let mut pending = turn(chat_id, Uuid::new_v4(), 10);
    pending.attachment_ids = vec![sent.attachment.id];
    repository.begin_turn(&pending).unwrap();

    let branch_id = ChatId::new();
    let mut copied_user = pending.user_message.clone();
    copied_user.id = MessageId::new();
    copied_user.chat_id = branch_id;
    copied_user.origin_message_id = Some(pending.user_message.id);
    let mut copy = repository.attachment(sent.attachment.id).unwrap().unwrap();
    copy.chat_id = branch_id;
    copy.attachment.id = koru_ai_core::ChatAttachmentId::new();
    copy.attachment.message_id = Some(copied_user.id);
    let mut branch = chat(branch_id, 20);
    branch.branched_from = Some(BranchOrigin {
        chat_id,
        through_message_id: pending.user_message.id,
    });
    repository
        .create_branch(&BranchCopy {
            chat: branch,
            tool_policy: ChatToolPolicy {
                chat_id: branch_id,
                mode: ToolMode::Manual,
                workspace_ids: Vec::new(),
            },
            messages: vec![copied_user.clone()],
            blocks: Vec::new(),
            citations: Vec::new(),
            pinned_documents: Vec::new(),
            attachments: vec![copy.clone()],
        })
        .unwrap();
    let listed = repository.attachments_for_chat(branch_id).unwrap();
    assert_eq!(listed, vec![copy.attachment]);
    assert_eq!(listed[0].message_id, Some(copied_user.id));
    // The source keeps its own row.
    assert_eq!(repository.attachments_for_chat(chat_id).unwrap().len(), 1);
}

/// **A message steered into a running reply is recorded after it**: after
/// every message the chat has, the pending reply included, with no schema
/// change; recording it again answers what was recorded.
#[test]
fn a_steered_message_is_recorded_after_the_running_reply() {
    let (mut repository, chat_id) = repository_with_chat();
    let pending = turn(chat_id, Uuid::new_v4(), 10);
    repository.begin_turn(&pending).unwrap();
    let steered = Message {
        id: MessageId::new(),
        chat_id,
        ordinal: 0,
        role: ChatRole::User,
        state: MessageState::Complete,
        markdown: "Also check the tests".into(),
        origin_message_id: None,
        provider: None,
        requested_model: None,
        resolved_model: None,
        error_class: None,
        created_at: at(20),
        completed_at: Some(at(20)),
    };
    let stored = repository.append_user_message(&steered).unwrap();
    assert_eq!(stored.ordinal, 2, "after the pending reply's ordinal 1");
    let messages = repository.messages(chat_id).unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.role, message.state))
            .collect::<Vec<_>>(),
        [
            (ChatRole::User, MessageState::Complete),
            (ChatRole::Assistant, MessageState::Pending),
            (ChatRole::User, MessageState::Complete),
        ]
    );
    let again = repository.append_user_message(&steered).unwrap();
    assert_eq!(again, stored, "idempotent by id");
    assert_eq!(repository.messages(chat_id).unwrap().len(), 3);
    // Only the person's complete message is recorded this way.
    let mut reply = steered.clone();
    reply.id = MessageId::new();
    reply.role = ChatRole::Assistant;
    assert!(repository.append_user_message(&reply).is_err());
}
