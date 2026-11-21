// SPDX-License-Identifier: MPL-2.0
//! Pure values: tokens, bounds, redaction, and the rule that a policy, a Bot
//! and an access command without a connection grant keep their pre-M5 bytes.
use super::*;
use crate::bots::{
    Bot, BotAccess, BotCommand, BotConfig, BotId, BotPage, DeviceId, UserScopeId,
    bot_native_policy, bot_native_policy_from,
};
use crate::{ChatId, DocumentId, ModelSelection, WorkspaceId};

fn id(n: u8) -> uuid::Uuid {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}
fn grant(n: u8) -> ConnectionGrantRef {
    ConnectionGrantRef {
        connection: ConnectionId::from_uuid(id(n)),
        operation: PrivateOperationKind::ReadItem,
    }
}
fn bot() -> Bot {
    let workspace = WorkspaceId::from_uuid(id(1));
    Bot {
        may_ask: true,
        summary: None,
        id: BotId::from_uuid(id(2)),
        home_workspace: workspace,
        user_scope: UserScopeId::from_uuid(id(3)),
        owner_device: DeviceId::from_uuid(id(4)),
        config: BotConfig {
            name: "fixture".into(),
            role: BotPage {
                workspace,
                document: DocumentId::from_uuid(id(5)),
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
        selected_pages: vec![BotPage {
            workspace,
            document: DocumentId::from_uuid(id(6)),
        }],
        revision: 1,
        policy_revision: 1,
        main_chat: ChatId::from_uuid(id(7)),
        paused: false,
        folder_lock_request: None,
        folder_frozen_at_ms: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        lifecycle: Default::default(),
        open_operation: None,
        held_by: None,
        pending_change: None,
        connections: Default::default(),
    }
}

/// The bytes a policy had before M5, authored by hand: four fields, in this
/// order, and nothing else.
const PRE_M5_POLICY: &str = r#"{"workspace":"00000000-0000-4000-8000-000000000001","access":"default","selected_pages":["00000000-0000-4000-8000-000000000006"],"destination":"00000000-0000-4000-8000-000000000001"}"#;

/// The digest every owner computes over a policy, through the one shared
/// helper, under a test domain.
fn digest(policy: &koru_delegate_core::CapturedBotPolicy) -> [u8; 32] {
    *koru_delegate_core::routine_digest_json(b"koru.test.policy", policy, 1 << 20)
        .unwrap()
        .as_bytes()
}

#[test]
fn m5_ai_policy_bytes_without_a_grant_are_the_pre_m5_bytes_and_digest() {
    let bot = bot();
    let policy = bot_native_policy(&bot);
    assert_eq!(serde_json::to_string(&policy).unwrap(), PRE_M5_POLICY);
    // The same bytes whichever constructor built it, so every digest over
    // them (both barriers, a Routine grant, a binding, a stored submission)
    // is the one it was.
    let direct = bot_native_policy_from(
        bot.home_workspace,
        bot.access,
        &bot.selected_pages,
        &Default::default(),
    );
    assert_eq!(serde_json::to_string(&direct).unwrap(), PRE_M5_POLICY);
    // An old stored policy decodes to no grant, and to the same digest.
    let decoded: koru_delegate_core::CapturedBotPolicy =
        serde_json::from_str(PRE_M5_POLICY).unwrap();
    assert!(decoded.connections.is_empty());
    assert_eq!(decoded, policy);
    assert_eq!(digest(&policy), digest(&decoded));
}

#[test]
fn m5_ai_policy_bytes_with_a_grant_append_one_field_and_change_the_digest() {
    let mut bot = bot();
    let plain = bot_native_policy(&bot);
    bot.connections = [grant(9), grant(8)].into();
    let policy = bot_native_policy(&bot);
    let expected = format!(
        r#"{},"connections":[{{"connection":"00000000-0000-4000-8000-000000000008","operation":"read_item"}},{{"connection":"00000000-0000-4000-8000-000000000009","operation":"read_item"}}]}}"#,
        PRE_M5_POLICY.strip_suffix('}').unwrap()
    );
    assert_eq!(serde_json::to_string(&policy).unwrap(), expected);
    assert_ne!(digest(&policy), digest(&plain));
    assert!(policy.valid());
    assert!(policy.grants_connection(&grant(8)));
    assert!(!plain.grants_connection(&grant(8)));
    // Seventeen grants are not a policy.
    bot.connections = (10..27).map(grant).collect();
    assert!(!bot_native_policy(&bot).valid());
}

#[test]
fn m5_ai_bot_without_a_grant_serializes_and_decodes_exactly_as_before() {
    let mut bot = bot();
    let plain = serde_json::to_value(&bot).unwrap();
    assert!(plain.get("connections").is_none());
    // A Bot stored in a receipt before M5 has no such field and still reads.
    let decoded: Bot = serde_json::from_value(plain.clone()).unwrap();
    assert_eq!(decoded, bot);
    bot.connections = [grant(8)].into();
    let granted = serde_json::to_value(&bot).unwrap();
    assert_eq!(
        granted["connections"],
        serde_json::json!([{"connection":"00000000-0000-4000-8000-000000000008","operation":"read_item"}])
    );
    let mut without = granted.clone();
    without.as_object_mut().unwrap().remove("connections");
    assert_eq!(without, plain);
    assert_eq!(serde_json::from_value::<Bot>(granted).unwrap(), bot);
}

#[test]
fn m5_ai_update_access_fingerprint_covers_connections_only_when_present() {
    let command = |connections| BotCommand::UpdateAccess {
        may_ask: None,
        request_id: crate::bots::BotRequestId::from_uuid(id(0x20)),
        bot_id: BotId::from_uuid(id(2)),
        expected_revision: 1,
        expected_policy_revision: 1,
        access: BotAccess::Workspace,
        selected_pages: vec![],
        connections,
    };
    // What every caller sends today, authored by hand.
    let before = r#"{"command":"update_access","request_id":"00000000-0000-4000-8000-000000000020","bot_id":"00000000-0000-4000-8000-000000000002","expected_revision":1,"expected_policy_revision":1,"access":"workspace","selected_pages":[]}"#;
    let absent = command(None);
    assert_eq!(serde_json::to_string(&absent).unwrap(), before);
    assert_eq!(serde_json::from_str::<BotCommand>(before).unwrap(), absent);
    // Present and empty is another command: it removes every grant.
    let empty = serde_json::to_string(&command(Some(Default::default()))).unwrap();
    assert_eq!(
        empty,
        format!(
            r#"{},"connections":[]}}"#,
            before.strip_suffix('}').unwrap()
        )
    );
    let one = command(Some([grant(8)].into()));
    let encoded = serde_json::to_string(&one).unwrap();
    assert!(encoded.ends_with(
        r#","connections":[{"connection":"00000000-0000-4000-8000-000000000008","operation":"read_item"}]}"#
    ));
    assert_eq!(serde_json::from_str::<BotCommand>(&encoded).unwrap(), one);
    // Unknown fields stay refused, inside a grant too.
    assert!(
        serde_json::from_str::<BotCommand>(&encoded.replace(
            r#""operation":"read_item""#,
            r#""operation":"read_item","account":"x""#
        ))
        .is_err()
    );
}

#[test]
fn m5_ai_connection_tokens_round_trip_and_nothing_else_parses() {
    macro_rules! closed {
        ($kind:ty, $count:expr) => {
            assert_eq!(<$kind>::ALL.len(), $count);
            for value in <$kind>::ALL {
                assert_eq!(<$kind>::parse(value.token()), Some(*value));
            }
            assert_eq!(<$kind>::parse(""), None);
            assert_eq!(<$kind>::parse("Connected"), None);
            assert_eq!(<$kind>::parse("other"), None);
        };
    }
    closed!(ConnectionState, 4);
    closed!(VendorRevoke, 3);
    closed!(ConnectionItemState, 2);
    closed!(ConnectionCommandKind, 3);
    closed!(ConnectionReceiptStage, 3);
    closed!(ConnectionFailure, 13);
    for token in vendors::QUALIFIED {
        assert_eq!(ProviderToken::parse(token).unwrap().as_str(), *token);
    }
    assert_eq!(ProviderToken::parse("fake"), None);
    assert_eq!(ProviderToken::parse(""), None);
}

fn account() -> ConnectedAccount {
    ConnectedAccount {
        provider: ProviderToken::parse(vendors::QUALIFIED[0]).unwrap(),
        service_origin: "https://vendor.example".into(),
        name: "MARK-NAME".into(),
        account_subject_digest: [1; 32],
        account_label: "MARK-LABEL@example.test".into(),
        scopes: vec!["MARK-SCOPE".into()],
        credential_slot_digest: [2; 32],
        items: vec![PickedConnectionItem {
            item_ref: "MARK-REF".into(),
            item_ref_digest: [3; 32],
            identity_digest: [4; 32],
            title: "MARK-TITLE".into(),
        }],
    }
}

#[test]
fn m5_ai_connection_account_bounds_are_the_tables_bounds() {
    account().validate().unwrap();
    let refused = |change: fn(&mut ConnectedAccount)| {
        let mut account = account();
        change(&mut account);
        assert_eq!(account.validate(), Err(ConnectionError::Invalid));
    };
    refused(|a| a.name.clear());
    refused(|a| a.name = " padded".into());
    refused(|a| a.name = "line\nbreak".into());
    refused(|a| a.name = "n".repeat(257));
    refused(|a| a.service_origin = "http://vendor.example".into());
    refused(|a| a.service_origin = format!("https://{}", "v".repeat(250)));
    refused(|a| a.account_label.clear());
    refused(|a| a.account_label = "l".repeat(321));
    refused(|a| a.scopes = vec![String::new()]);
    refused(|a| a.scopes = vec!["s".repeat(5000)]);
    refused(|a| a.items[0].item_ref.clear());
    refused(|a| a.items[0].item_ref = "r".repeat(1025));
    refused(|a| a.items[0].title.clear());
    refused(|a| a.items[0].title = "t".repeat(513));
    refused(|a| {
        let twice = a.items[0].clone();
        a.items.push(twice)
    });
    refused(|a| {
        a.items = (0..257u16)
            .map(|n| {
                let mut digest = [0; 32];
                digest[..2].copy_from_slice(&n.to_be_bytes());
                PickedConnectionItem {
                    item_ref: format!("ref-{n}"),
                    item_ref_digest: digest,
                    identity_digest: [4; 32],
                    title: "title".into(),
                }
            })
            .collect()
    });
    assert!(valid_connection_fingerprint(&"a".repeat(64)));
    for bad in [
        String::new(),
        "A".repeat(64),
        "g".repeat(64),
        "a".repeat(63),
    ] {
        assert!(!valid_connection_fingerprint(&bad));
    }
}

#[test]
fn m5_ai_redaction_no_private_text_in_any_debug_or_error() {
    let account = account();
    let owner = crate::bots::BotOwner {
        user_scope: UserScopeId::from_uuid(id(3)),
        device: DeviceId::from_uuid(id(4)),
    };
    let connection = Connection {
        id: ConnectionId::from_uuid(id(8)),
        owner,
        provider: account.provider,
        service_origin: "https://MARK-ORIGIN.example".into(),
        name: account.name.clone(),
        account_subject_digest: [1; 32],
        account_label: account.account_label.clone(),
        scopes: account.scopes.clone(),
        credential_slot_digest: [2; 32],
        state: ConnectionState::Connected,
        vendor_revoke: None,
        revision: 1,
        connected_at_ms: 1,
        refreshed_at_ms: None,
        disconnected_at_ms: None,
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    let item = ConnectionItem {
        id: ConnectionItemId::from_uuid(id(9)),
        connection: connection.id,
        item_ref: "MARK-REF".into(),
        item_ref_digest: [3; 32],
        identity_digest: [4; 32],
        title: "MARK-TITLE".into(),
        state: ConnectionItemState::Available,
        added_at_ms: 1,
        changed_at_ms: None,
        updated_at_ms: 1,
    };
    let printed = [
        format!("{account:?}"),
        format!("{connection:?}"),
        format!("{item:?}"),
        format!("{:?}", account.items[0]),
        format!("{:#?}", (&connection, &item)),
    ]
    .join("\n");
    assert!(!printed.contains("MARK"), "{printed}");
    assert!(printed.contains("Connection"));
    // The read grant is a reference: an id and an operation.
    assert_eq!(connection.read_grant(), grant(8));
    // Every refusal is one fixed lowercase token.
    let errors = [
        ConnectionError::Invalid,
        ConnectionError::NotFound,
        ConnectionError::Forbidden,
        ConnectionError::Conflict,
        ConnectionError::RevisionConflict,
        ConnectionError::Busy,
        ConnectionError::ConnectionLimit,
        ConnectionError::ItemLimit,
        ConnectionError::NameInUse,
        ConnectionError::AccountInUse,
        ConnectionError::WrongAccount,
        ConnectionError::Unavailable,
        ConnectionError::Storage,
    ];
    let mut codes = std::collections::BTreeSet::new();
    for error in errors {
        let text = error.to_string();
        assert_eq!(text, error.code());
        assert!(
            text.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{text}"
        );
        assert!(codes.insert(text));
    }
    assert_eq!(
        crate::bots::BotError::ConnectionUnavailable.code(),
        "connection_unavailable"
    );
}
