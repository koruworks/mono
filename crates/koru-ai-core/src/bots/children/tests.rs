// SPDX-License-Identifier: MPL-2.0
//! Cohorts `m6_child_policy` and `m6_may_ask` (the wire half), and the stable
//! tokens of the child reads. Pure values: no repository, no worker.
use super::*;
use koru_delegate_core::{
    BotAccess as CapturedAccess, CapturedBotPolicy, ConnectionGrantRef, ConnectionId,
    PrivateOperationKind,
};
use std::collections::BTreeSet;

fn id<T: serde::de::DeserializeOwned>(n: u8) -> T {
    serde_json::from_value(serde_json::json!(format!(
        "00000000-0000-4000-8000-0000000000{n:02x}"
    )))
    .unwrap()
}

fn grant(n: u8) -> ConnectionGrantRef {
    ConnectionGrantRef {
        connection: id::<ConnectionId>(n),
        operation: PrivateOperationKind::ReadItem,
    }
}

fn policy(access: CapturedAccess, pages: &[u8], grants: &[u8]) -> CapturedBotPolicy {
    let workspace: WorkspaceId = id(1);
    CapturedBotPolicy {
        workspace,
        destination: workspace,
        access,
        selected_pages: pages.iter().map(|n| id(*n)).collect(),
        connections: grants.iter().map(|n| grant(*n)).collect(),
    }
}

fn grants(list: &[u8]) -> BTreeSet<ConnectionGrantRef> {
    list.iter().map(|n| grant(*n)).collect()
}

// ---- m6_child_policy ---------------------------------------------------------

#[test]
fn m6_child_policy_is_the_recipients_with_connections_intersected() {
    let recipient = policy(
        CapturedAccess::Workspace,
        &[0x21, 0x22],
        &[0x31, 0x32, 0x33],
    );
    let parent = policy(CapturedAccess::Default, &[0x29], &[0x32, 0x33, 0x34]);
    let child = bot_child_policy(&recipient, &parent, &parent.connections);
    // Only the grants both hold are passed on.
    assert_eq!(child.connections, grants(&[0x32, 0x33]));
    // Pages, the write list, the workspace and the destination are the
    // recipient's, unchanged: the parent's are never taken.
    assert_eq!(child.access, recipient.access);
    assert_eq!(child.selected_pages, recipient.selected_pages);
    assert_eq!(child.workspace, recipient.workspace);
    assert_eq!(child.destination, recipient.destination);
    assert!(child.valid());
    // The reduction never adds: every grant of the child is the parent's,
    // which is the rule the delegate checks.
    assert!(
        child
            .connections
            .iter()
            .all(|g| parent.grants_connection(g))
    );
    assert!(child.connections.is_subset(&recipient.connections));
}

#[test]
fn m6_child_policy_a_grant_revoked_from_the_parent_since_its_run_started_is_not_passed_on() {
    let recipient = policy(CapturedAccess::Default, &[], &[0x31, 0x32]);
    // The parent Run captured both; the Bot has since lost one.
    let captured = policy(CapturedAccess::Default, &[], &[0x31, 0x32]);
    let child = bot_child_policy(&recipient, &captured, &grants(&[0x32]));
    assert_eq!(child.connections, grants(&[0x32]));
    // A grant the parent Bot gained after its Run started is not passed on
    // either: the Run's own tools are not checked against it.
    let child = bot_child_policy(
        &recipient,
        &policy(CapturedAccess::Default, &[], &[0x32]),
        &grants(&[0x31, 0x32]),
    );
    assert_eq!(child.connections, grants(&[0x32]));
}

#[test]
fn m6_child_policy_an_empty_parent_set_gives_an_empty_child_set() {
    let recipient = policy(CapturedAccess::Workspace, &[0x21], &[0x31, 0x32]);
    let without = policy(CapturedAccess::Workspace, &[], &[]);
    for (captured, current) in [
        (&without, grants(&[0x31, 0x32])),
        (&recipient, grants(&[])),
        (&without, grants(&[])),
    ] {
        let child = bot_child_policy(&recipient, captured, &current);
        assert!(child.connections.is_empty());
        assert_eq!(child.selected_pages, recipient.selected_pages);
        assert_eq!(child.access, recipient.access);
    }
    // A recipient without a grant stays without one, whatever the parent holds.
    let plain = policy(CapturedAccess::Default, &[0x21], &[]);
    let child = bot_child_policy(&plain, &recipient, &recipient.connections);
    assert_eq!(child, plain);
    // And then the policy keeps its bytes of before M5: no `connections` key.
    let bytes = serde_json::to_string(&child).unwrap();
    assert!(!bytes.contains("connections"), "{bytes}");
}

#[test]
fn m6_child_policy_same_connection_other_operation_is_another_grant() {
    // A grant is in both sets only when connection and operation are equal.
    let recipient = policy(CapturedAccess::Default, &[], &[0x31]);
    let parent = policy(CapturedAccess::Default, &[], &[0x32]);
    assert!(
        bot_child_policy(&recipient, &parent, &parent.connections)
            .connections
            .is_empty()
    );
    assert_eq!(grant(0x31), grant(0x31));
    assert_ne!(grant(0x31), grant(0x32));
}

// ---- m6_may_ask --------------------------------------------------------------

/// `update_access` as a caller that does not know `may_ask` sends it, and as
/// this build serialized it before the field existed. Authored, not printed.
const UPDATE_ACCESS_BEFORE: &str = r#"{"command":"update_access","request_id":"00000000-0000-4000-8000-000000000041","bot_id":"00000000-0000-4000-8000-000000000042","expected_revision":3,"expected_policy_revision":2,"access":"workspace","selected_pages":[{"workspace":"00000000-0000-4000-8000-000000000001","document":"00000000-0000-4000-8000-000000000021"}]}"#;

fn update_access(may_ask: Option<bool>) -> BotCommand {
    BotCommand::UpdateAccess {
        request_id: id(0x41),
        bot_id: id(0x42),
        expected_revision: 3,
        expected_policy_revision: 2,
        access: BotAccess::Workspace,
        selected_pages: vec![BotPage {
            workspace: id(1),
            document: id(0x21),
        }],
        connections: None,
        may_ask,
    }
}

#[test]
fn m6_may_ask_update_access_without_it_keeps_todays_bytes_and_fingerprint() {
    // The request fingerprint is a digest of the serialized command. A
    // command that does not mention `may_ask` serializes to its earlier
    // bytes, so its fingerprint, and every stored receipt's, is unchanged.
    let absent = update_access(None);
    assert_eq!(
        serde_json::to_string(&absent).unwrap(),
        UPDATE_ACCESS_BEFORE
    );
    assert_eq!(
        serde_json::from_str::<BotCommand>(UPDATE_ACCESS_BEFORE).unwrap(),
        absent
    );
    // Present, it is one more key, and both values are distinct requests.
    let off = serde_json::to_string(&update_access(Some(false))).unwrap();
    let on = serde_json::to_string(&update_access(Some(true))).unwrap();
    assert_eq!(
        off,
        format!(
            "{},\"may_ask\":false}}",
            UPDATE_ACCESS_BEFORE.strip_suffix('}').unwrap()
        )
    );
    assert_ne!(on, UPDATE_ACCESS_BEFORE);
    assert_ne!(on, off);
    assert_eq!(
        serde_json::from_str::<BotCommand>(&off).unwrap(),
        update_access(Some(false))
    );
    // The field takes a boolean and nothing else; unknown fields stay refused.
    assert!(serde_json::from_str::<BotCommand>(&off.replace("false", "\"no\"")).is_err());
    assert!(serde_json::from_str::<BotCommand>(&off.replace("may_ask", "may_tell")).is_err());
}

#[test]
fn m6_may_ask_a_bot_that_may_ask_serializes_as_before_the_field_existed() {
    let stored = serde_json::json!({
        "id": id::<BotId>(0x42),
        "home_workspace": id::<WorkspaceId>(1),
        "user_scope": id::<UserScopeId>(2),
        "owner_device": id::<DeviceId>(3),
        "config": {
            "name": "Scout",
            "role": {"workspace": id::<WorkspaceId>(1), "document": id::<DocumentId>(0x21)},
            "working_folder": "D:/scout",
            "model": {"provider": "agent-codex", "requested_model": "default", "default_reasoning": "default"},
            "mode": null,
            "fast": null
        },
        "access": "default",
        "selected_pages": [],
        "revision": 1,
        "policy_revision": 1,
        "main_chat": id::<ChatId>(4),
        "paused": false,
        "folder_lock_request": null,
        "folder_frozen_at_ms": null,
        "created_at_ms": 1,
        "updated_at_ms": 1
    });
    // A Bot stored before ai.db 26 has no `may_ask`: it reads as on.
    let bot: Bot = serde_json::from_value(stored.clone()).unwrap();
    assert!(bot.may_ask);
    assert!(may_ask_default());
    // While it is on the field is omitted, so the Bot's bytes are the stored
    // ones; turned off, it is one more key.
    assert_eq!(serde_json::to_value(&bot).unwrap(), stored);
    let mut off = bot.clone();
    off.may_ask = false;
    let mut expected = stored;
    expected["may_ask"] = serde_json::json!(false);
    assert_eq!(serde_json::to_value(&off).unwrap(), expected);
    assert_eq!(serde_json::from_value::<Bot>(expected).unwrap(), off);
    // The switch is not policy: the captured policy of both is identical.
    assert_eq!(bot_native_policy(&bot), bot_native_policy(&off));
}

// ---- the child reads ----------------------------------------------------------

#[test]
fn m6_child_reads_commands_and_tokens_are_stable() {
    let list = BotCommand::ListChildRequests {
        bot_id: id(0x42),
        chat_id: id(4),
    };
    let origin = BotCommand::ReadChildOrigin {
        bot_id: id(0x42),
        chat_id: id(4),
    };
    assert_eq!(
        serde_json::to_string(&list).unwrap(),
        r#"{"command":"list_child_requests","bot_id":"00000000-0000-4000-8000-000000000042","chat_id":"00000000-0000-4000-8000-000000000004"}"#
    );
    assert_eq!(
        serde_json::to_string(&origin).unwrap(),
        r#"{"command":"read_child_origin","bot_id":"00000000-0000-4000-8000-000000000042","chat_id":"00000000-0000-4000-8000-000000000004"}"#
    );
    for command in [&list, &origin] {
        let mut json = serde_json::to_value(command).unwrap();
        assert_eq!(
            &serde_json::from_value::<BotCommand>(json.clone()).unwrap(),
            command
        );
        // Neither read is a mutation: no request id, and none is accepted.
        json["request_id"] = serde_json::json!(id::<BotRequestId>(0x41));
        assert!(serde_json::from_value::<BotCommand>(json).is_err());
        assert!(!command.is_memory());
        assert!(command.lifecycle_kind().is_none());
        assert!(command.routine_control().is_none());
    }
    let card = BotChildRequestCard {
        child_request_id: id(0x51),
        parent_assistant_message_id: id(0x52),
        recipient_bot_id: id(0x43),
        recipient_name: "Scribe".into(),
        child_chat_id: id(5),
        state: koru_delegate_core::children::ChildRequestState::Settled,
        child: BotChildProgress::Finished,
        outcome: Some(BotChildOutcome::Completed),
        clock: koru_delegate_core::children::ChildClock::Stopped,
        result: BotChildResult::Saved { pages: 2 },
        needs_you: false,
    };
    let reply = BotResponse::ChildRequests {
        requests: vec![card.clone()],
    };
    assert_eq!(
        serde_json::to_string(&reply).unwrap(),
        r#"{"result":"child_requests","requests":[{"child_request_id":"00000000-0000-4000-8000-000000000051","parent_assistant_message_id":"00000000-0000-4000-8000-000000000052","recipient_bot_id":"00000000-0000-4000-8000-000000000043","recipient_name":"Scribe","child_chat_id":"00000000-0000-4000-8000-000000000005","state":"settled","child":"finished","outcome":"completed","clock":"stopped","result":{"result":"saved","pages":2},"needs_you":false}]}"#
    );
    let mut open = card;
    open.state = koru_delegate_core::children::ChildRequestState::Open;
    open.child = BotChildProgress::NeedsYou;
    open.outcome = None;
    open.clock = koru_delegate_core::children::ChildClock::Paused;
    open.result = BotChildResult::None;
    open.needs_you = true;
    let json = serde_json::to_value(&open).unwrap();
    assert_eq!(json["state"], "open");
    assert_eq!(json["child"], "needs_you");
    assert_eq!(json["clock"], "paused");
    assert_eq!(json["result"], serde_json::json!({"result": "none"}));
    assert!(json.get("outcome").is_none());
    assert_eq!(
        serde_json::to_value(BotChildResult::NoPageSaved).unwrap(),
        serde_json::json!({"result": "no_page_saved"})
    );
    // A conversation nobody asked for has no origin, and says so with an
    // absent key.
    assert_eq!(
        serde_json::to_string(&BotResponse::ChildOrigin { origin: None }).unwrap(),
        r#"{"result":"child_origin"}"#
    );
    let asked = BotResponse::ChildOrigin {
        origin: Some(BotChildOrigin {
            child_request_id: id(0x51),
            parent_bot_id: id(0x42),
            parent_name: "Scout".into(),
            parent_chat_id: id(4),
            state: koru_delegate_core::children::ChildRequestState::Open,
            outcome: None,
        }),
    };
    assert_eq!(
        serde_json::to_string(&asked).unwrap(),
        r#"{"result":"child_origin","origin":{"child_request_id":"00000000-0000-4000-8000-000000000051","parent_bot_id":"00000000-0000-4000-8000-000000000042","parent_name":"Scout","parent_chat_id":"00000000-0000-4000-8000-000000000004","state":"open"}}"#
    );
    // Every outcome has its token, and only `completed` hands anything over.
    let outcomes = [
        (BotChildOutcome::Completed, "completed"),
        (BotChildOutcome::Failed, "failed"),
        (BotChildOutcome::NeededYou, "needed_you"),
        (BotChildOutcome::Canceled, "canceled"),
        (BotChildOutcome::TimedOut, "timed_out"),
        (BotChildOutcome::ParentStopped, "parent_stopped"),
    ];
    for (outcome, token) in outcomes {
        assert_eq!(outcome.token(), token);
        assert_eq!(
            serde_json::to_value(outcome).unwrap(),
            serde_json::json!(token)
        );
        assert_eq!(outcome.hands_over(), outcome == BotChildOutcome::Completed);
    }
    // The refusals the AI owner gives by itself.
    for (refusal, code, index) in [
        (BotChildAskRefusal::AskNotAllowed, "ask_not_allowed", None),
        (
            BotChildAskRefusal::RecipientUnavailable { index: 2 },
            "recipient_unavailable",
            Some(2),
        ),
        (
            BotChildAskRefusal::RecipientPaused { index: 1 },
            "recipient_paused",
            Some(1),
        ),
        (
            BotChildAskRefusal::SelfAsk { index: 0 },
            "self_ask",
            Some(0),
        ),
        (BotChildAskRefusal::AskKeyConflict, "ask_key_conflict", None),
    ] {
        assert_eq!((refusal.code(), refusal.index()), (code, index));
        assert_eq!(serde_json::to_value(refusal).unwrap()["code"], code);
    }
    assert_eq!(BOT_CHILD_ASK_LIMIT, 3);
}
