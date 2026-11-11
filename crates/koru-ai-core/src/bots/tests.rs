// SPDX-License-Identifier: MPL-2.0
use super::*;
#[test]
fn additional_chat_contract_is_closed_and_receipt_versioned() {
    let request = BotCommand::CreateChat {
        request_id: BotRequestId::mint(),
        bot_id: BotId::mint(),
    };
    let mut json = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<BotCommand>(json.clone()).unwrap(),
        request
    );
    json["chat_id"] = serde_json::json!(ChatId::new());
    assert!(serde_json::from_value::<BotCommand>(json).is_err());
    let reference = BotChatReference {
        bot_id: BotId::mint(),
        chat_id: ChatId::new(),
    };
    let outcome = BotReceiptOutcome::ChatCreated {
        version: 1,
        reference: reference.clone(),
    };
    assert!(outcome.validate().is_ok());
    assert_eq!(
        BotReceiptOutcome::ChatCreated {
            version: 2,
            reference
        }
        .validate(),
        Err(BotError::Invalid)
    );
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
    let request = BotCommand::ListActivity { workspace };
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
