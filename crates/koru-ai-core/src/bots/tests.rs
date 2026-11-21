// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn retired_bot_commands_are_not_wire_variants() {
    for command in [
        "test_routine",
        "read_memory",
        "ensure_memory",
        "retry_memory",
        "abandon_memory",
        "list_memory",
        "read_memory_fact",
        "list_task_bots",
        "read_task_answer",
        "start",
        "continue",
        "list_linked_owners",
        "list_linked_bots",
        "create",
        "purge_bot",
        "purge_chat",
        "list_child_requests",
        "read_child_origin",
        "read_run",
        "stop_task",
    ] {
        let error = serde_json::from_value::<BotCommand>(serde_json::json!({"command":command}))
            .unwrap_err();
        assert!(
            error.to_string().contains("unknown variant"),
            "{command}: {error}"
        );
    }
}
#[test]
fn bot_model_rejects_settings_it_does_not_understand() {
    let selection = ModelSelection {
        provider: crate::AiProviderId::parse("agent-codex").unwrap(),
        requested_model: "fixture".into(),
        default_reasoning: crate::ReasoningChoice::Default,
    };
    let workspace = WorkspaceId::new();
    let config = BotConfig {
        name: "fixture".into(),
        role: BotPage {
            workspace,
            document: DocumentId::new(),
        },
        working_folder: "/fixture".into(),
        model: selection.clone(),
        mode: None,
        fast: None,
        avatar: None,
    };
    let mut encoded = serde_json::to_value(&config).unwrap();
    assert_eq!(
        serde_json::from_value::<BotConfig>(encoded.clone()).unwrap(),
        config
    );
    encoded["model"]["invented"] = serde_json::json!(true);
    assert!(serde_json::from_value::<BotConfig>(encoded.clone()).is_err());
    assert!(
        serde_json::from_value::<ModelSelection>(encoded["model"].clone()).is_ok(),
        "ordinary Chat compatibility stays unchanged"
    );
}

#[test]
fn a_conversation_page_request_is_closed_and_its_reply_is_additive() {
    let request = BotCommand::ListBotChats {
        bot_id: BotId::mint(),
        cursor: Some(BotChatCursor {
            main: false,
            updated_at_ms: 7,
            chat_id: ChatId::new(),
        }),
        limit: 20,
    };
    let mut json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["command"], "list_bot_chats");
    assert_eq!(
        serde_json::from_value::<BotCommand>(json.clone()).unwrap(),
        request
    );
    // The first page omits its cursor.
    let mut first = json.clone();
    first.as_object_mut().unwrap().remove("cursor");
    assert!(matches!(
        serde_json::from_value::<BotCommand>(first).unwrap(),
        BotCommand::ListBotChats { cursor: None, .. }
    ));
    json["cursor"]["offset"] = serde_json::json!(3);
    assert!(
        serde_json::from_value::<BotCommand>(json).is_err(),
        "a cursor is exactly what a page returned"
    );

    let reply = serde_json::json!({
        "result": "chats",
        "page": { "chats": [], "next_cursor": null, "added_later": true },
        "added_later": true,
    });
    assert_eq!(
        serde_json::from_value::<BotResponse>(reply).unwrap(),
        BotResponse::Chats {
            page: BotChatPage {
                chats: vec![],
                next_cursor: None
            }
        },
        "a reply field a later build adds is ignored"
    );
}

#[test]
fn the_activity_read_names_only_a_workspace_and_two_states() {
    let workspace = WorkspaceId::new();
    let request = BotCommand::ListActivity {
        workspace,
        cursor: None,
    };
    let mut json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["command"], "list_activity");
    assert_eq!(
        serde_json::from_value::<BotCommand>(json.clone()).unwrap(),
        request
    );
    json["bot_id"] = serde_json::json!(BotId::mint());
    assert!(serde_json::from_value::<BotCommand>(json).is_err());
    assert_eq!(
        serde_json::to_value([BotActivityState::Running, BotActivityState::NeedsYou]).unwrap(),
        serde_json::json!(["running", "needs_you"])
    );
}

/// `Chat` rejects no unknown field, so a build before the label reads an
/// answer that carries it, and a newer reader of an older answer gets none.
#[test]
fn the_chat_label_is_optional_both_ways() {
    #[derive(Deserialize)]
    struct OlderChat {
        id: ChatId,
        title: String,
    }
    let label = BotChatLabel {
        bot_id: BotId::mint(),
        bot_name: "Scout".into(),
        purpose: BotChatPurpose::Main,
    };
    let mut json = serde_json::json!({
        "id": ChatId::new(),
        "title": "Scout",
        "current_model": {
            "provider": "agent-codex",
            "requested_model": "fixture",
            "default_reasoning": "default"
        },
        "branched_from": null,
        "created_at": 1,
        "updated_at": 1,
    });
    let older_answer: Chat = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(older_answer.bot, None);
    json["bot"] = serde_json::to_value(&label).unwrap();
    let labelled: Chat = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(labelled.bot, Some(label));
    let older: OlderChat = serde_json::from_value(json).unwrap();
    assert_eq!(older.title, "Scout");
    let _ = older.id;
}

/// A stop code is one of the engine's closed codes or nothing, and an error
/// without one serializes exactly as before (O36).
#[test]
fn a_stop_code_is_closed_and_absent_by_default() {
    let plain = crate::AiError::new(crate::AiErrorClass::Provider, "", false);
    assert_eq!(plain.stop_code, None);
    assert!(
        serde_json::to_value(&plain)
            .unwrap()
            .get("stop_code")
            .is_none()
    );
    let coded = plain.clone().with_stop_code("bot_input_too_large");
    assert_eq!(coded.stop_code.as_deref(), Some("bot_input_too_large"));
    assert_eq!(plain.clone().with_stop_code("max_tokens").stop_code, None);
    #[derive(Deserialize)]
    struct OlderError {
        retryable: bool,
    }
    let older: OlderError = serde_json::from_value(serde_json::to_value(&coded).unwrap()).unwrap();
    assert!(!older.retryable);
    for refusal in koru_delegate_core::BotInputRefusal::ALL {
        assert!(crate::is_engine_stop_code(refusal.code()));
    }
}

#[test]
fn an_active_bot_reads_and_stores_exactly_as_before_and_lifecycle_appears_only_when_set() {
    let workspace = WorkspaceId::new();
    let bot = Bot {
        may_ask: true,
        summary: None,
        id: BotId::mint(),
        home_workspace: workspace,
        user_scope: UserScopeId::new(),
        owner_device: DeviceId::new(),
        config: BotConfig {
            name: "fixture".into(),
            role: BotPage {
                workspace,
                document: DocumentId::new(),
            },
            working_folder: "/fixture".into(),
            model: ModelSelection {
                provider: crate::AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture".into(),
                default_reasoning: crate::ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
            avatar: None,
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
        lifecycle: BotLifecycle::Active,
        open_operation: None,
        held_by: None,
        pending_change: None,
        connections: Default::default(),
    };
    // Active with nothing open: no lifecycle field is written, so receipts
    // that store a Bot and older readers see the same bytes as before P1.7.
    let active = serde_json::to_value(&bot).unwrap();
    assert!(active.get("lifecycle").is_none());
    assert!(active.get("open_operation").is_none());
    assert!(active.get("held_by").is_none());
    assert_eq!(serde_json::from_value::<Bot>(active.clone()).unwrap(), bot);
    let mut archived = bot.clone();
    archived.lifecycle = BotLifecycle::Archived;
    archived.open_operation = Some(BotOpenLifecycle {
        request_id: BotRequestId::mint(),
        kind: BotLifecycleKind::Archive,
        chat_id: None,
        stage: BotLifecycleStage::Incomplete,
        refusal: Some(BotLifecycleRefusal::PersonHolding),
    });
    let wire = serde_json::to_value(&archived).unwrap();
    assert_eq!(wire["lifecycle"], "archived");
    assert_eq!(wire["open_operation"]["kind"], "archive");
    assert_eq!(wire["open_operation"]["refusal"], "person_holding");
    assert_eq!(
        serde_json::from_value::<Bot>(wire.clone()).unwrap(),
        archived
    );
    let mut unknown = wire;
    unknown["open_operation"]["fingerprint"] = serde_json::json!("guessed");
    assert!(serde_json::from_value::<Bot>(unknown).is_err());
}

/// The exact bytes of an active Bot with nothing open, held or pending, as
/// every build since P1.3 writes it on the wire and inside a stored receipt
/// outcome (`configuration`, `access_policy`). Read-only fields are omitted
/// while empty, so these bytes stay what an older build wrote and reads.
const UNTOUCHED_BOT: &str = concat!(
    r#"{"id":"00000000-0000-4000-8000-000000000001","#,
    r#""home_workspace":"00000000-0000-4000-8000-000000000002","#,
    r#""user_scope":"00000000-0000-4000-8000-000000000003","#,
    r#""owner_device":"00000000-0000-4000-8000-000000000004","#,
    r#""config":{"name":"fixture","role":{"workspace":"00000000-0000-4000-8000-000000000002","#,
    r#""document":"00000000-0000-4000-8000-000000000005"},"working_folder":"/fixture","#,
    r#""model":{"provider":"agent-codex","requested_model":"fixture","default_reasoning":"default"},"#,
    r#""mode":null,"fast":null},"access":"default","selected_pages":[],"revision":3,"#,
    r#""policy_revision":1,"main_chat":"00000000-0000-4000-8000-000000000006","paused":true,"#,
    r#""folder_lock_request":null,"folder_frozen_at_ms":null,"created_at_ms":1,"updated_at_ms":2}"#
);

#[test]
fn a_pending_change_is_additive_closed_and_omits_an_unknown_answer() {
    let mut bot: Bot = serde_json::from_str(UNTOUCHED_BOT).unwrap();
    let request = BotRequestId::mint();
    bot.pending_change = Some(BotPendingChange {
        request_id: request,
        kind: BotOperationKind::Pause,
        since_ms: 42,
        last_answer: None,
    });
    let wire = serde_json::to_value(&bot).unwrap();
    assert_eq!(
        wire["pending_change"],
        serde_json::json!({"request_id": request, "kind": "pause", "since_ms": 42})
    );
    assert_eq!(serde_json::from_value::<Bot>(wire.clone()).unwrap(), bot);
    bot.pending_change.as_mut().unwrap().last_answer = Some("bot_unavailable".into());
    let wire = serde_json::to_value(&bot).unwrap();
    assert_eq!(wire["pending_change"]["last_answer"], "bot_unavailable");
    assert_eq!(serde_json::from_value::<Bot>(wire.clone()).unwrap(), bot);
    let mut unknown = wire;
    unknown["pending_change"]["stage"] = serde_json::json!("prepared");
    assert!(serde_json::from_value::<Bot>(unknown).is_err());
}

#[test]
fn the_operation_holding_an_archived_bot_is_additive_and_closed() {
    let workspace = WorkspaceId::new();
    let mut bot: Bot = serde_json::from_value(serde_json::json!({
        "id": BotId::mint(), "home_workspace": workspace, "user_scope": UserScopeId::new(),
        "owner_device": DeviceId::new().to_string(),
        "config": {"name": "fixture", "role": {"workspace": workspace, "document": DocumentId::new()},
            "working_folder": "/fixture",
            "model": {"provider": "agent-codex", "requested_model": "fixture", "default_reasoning": "default"},
            "mode": null, "fast": null},
        "access": "default", "selected_pages": [], "revision": 3, "policy_revision": 1,
        "main_chat": ChatId::new(), "paused": true, "folder_lock_request": null,
        "folder_frozen_at_ms": null, "created_at_ms": 1, "updated_at_ms": 1,
        "lifecycle": "archived"
    }))
    .unwrap();
    assert_eq!(bot.held_by, None);
    let request = BotRequestId::mint();
    bot.held_by = Some(BotLifecycleHolder {
        request_id: request,
        kind: BotLifecycleKind::Archive,
        completed_at_ms: 42,
    });
    let wire = serde_json::to_value(&bot).unwrap();
    assert_eq!(
        wire["held_by"],
        serde_json::json!({"request_id": request, "kind": "archive", "completed_at_ms": 42})
    );
    assert_eq!(serde_json::from_value::<Bot>(wire.clone()).unwrap(), bot);
    let mut unknown = wire;
    unknown["held_by"]["stage"] = serde_json::json!("completed");
    assert!(serde_json::from_value::<Bot>(unknown).is_err());
}

#[test]
fn a_trash_page_request_is_closed_and_names_the_restore_each_entry_needs() {
    let request = BotCommand::ListTrashedBotChats {
        workspace: WorkspaceId::new(),
        cursor: Some(BotTrashedChatCursor {
            chat_id: ChatId::new(),
        }),
        limit: 20,
    };
    let mut json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["command"], "list_trashed_bot_chats");
    assert_eq!(
        serde_json::from_value::<BotCommand>(json.clone()).unwrap(),
        request
    );
    json["cursor"]["offset"] = serde_json::json!(3);
    assert!(serde_json::from_value::<BotCommand>(json).is_err());
    assert_eq!(
        serde_json::to_value(BotChatRestore::Conversation).unwrap(),
        "conversation"
    );
    assert_eq!(serde_json::to_value(BotChatRestore::Bot).unwrap(), "bot");
    let reply = serde_json::json!({
        "result": "trashed_chats",
        "page": { "chats": [], "next_cursor": null },
    });
    assert!(matches!(
        serde_json::from_value::<BotResponse>(reply).unwrap(),
        BotResponse::TrashedChats { .. }
    ));
}
