// SPDX-License-Identifier: MPL-2.0
use super::*;
use serde_json::{Value, json};

fn ready(authored: bool) -> BotCreationRecord {
    let mut c = super::super::tests::authored(BotCreationPhase::RoleVerified);
    if !authored {
        c.phase = BotCreationPhase::FolderReady;
        c.role = BotCreationRole::Existing {
            page: c.role_target(),
        };
        c.role_page = None;
    }
    c.validate().unwrap();
    assert!(c.ready());
    c
}
fn legacy() -> Value {
    serde_json::to_value(BotReceiptOutcome::Configuration {
        version: 1,
        bot: ready(false).bot(9).unwrap(),
    })
    .unwrap()
}
fn project(value: &Value) -> Result<BotCreationIdentity, BotError> {
    bot_creation_identity(&value.to_string())
}

#[test]
fn creation_identity_projection_matches_full_v1_and_both_v2_ready_receipts() {
    let values = [
        legacy(),
        serde_json::to_value(BotReceiptOutcome::CreationV2 {
            version: 2,
            creation: ready(false),
        })
        .unwrap(),
        serde_json::to_value(BotReceiptOutcome::CreationV2 {
            version: 2,
            creation: ready(true),
        })
        .unwrap(),
    ];
    for value in values {
        let full: BotReceiptOutcome = serde_json::from_value(value.clone()).unwrap();
        full.validate().unwrap();
        let bot = match full {
            BotReceiptOutcome::Configuration { bot, .. } => bot,
            BotReceiptOutcome::CreationV2 { creation, .. } => creation.bot(9).unwrap(),
            _ => unreachable!(),
        };
        let projected = project(&value).unwrap();
        assert_eq!(projected.bot, bot.id);
        assert_eq!(
            projected.owner,
            BotOwner {
                user_scope: bot.user_scope,
                device: bot.owner_device
            }
        );
        assert_eq!(projected.home, bot.home_workspace);
        assert_eq!(projected.main_chat, bot.main_chat);
    }
}

#[test]
fn creation_identity_projection_rejects_each_missing_malformed_and_oversize_identity() {
    for v2 in [false, true] {
        let original = if v2 {
            serde_json::to_value(BotReceiptOutcome::CreationV2 {
                version: 2,
                creation: ready(false),
            })
            .unwrap()
        } else {
            legacy()
        };
        let key = if v2 { "creation" } else { "bot" };
        let bot_key = if v2 { "bot" } else { "id" };
        for field in [
            bot_key,
            "user_scope",
            "owner_device",
            "home_workspace",
            "main_chat",
        ] {
            let mut missing = original.clone();
            missing[key].as_object_mut().unwrap().remove(field);
            assert!(project(&missing).is_err(), "missing {field}");
            for invalid in [
                Value::Null,
                json!(4),
                json!([1]),
                json!("x".repeat(36)),
                json!("x".repeat(65536)),
            ] {
                let mut changed = original.clone();
                changed[key][field] = invalid;
                assert!(project(&changed).is_err(), "invalid {field}");
            }
        }
    }
}

#[test]
fn creation_identity_projection_checks_kind_version_revision_and_duplicate_identity() {
    let original = legacy();
    for (field, bad) in [
        ("kind", json!("access_policy")),
        ("version", json!(2)),
        ("version", json!("1")),
        ("version", json!(1.5)),
    ] {
        let mut value = original.clone();
        value[field] = bad;
        assert!(project(&value).is_err());
    }
    for revision in [json!(0), json!(2), json!("1"), json!(1.5)] {
        let mut value = original.clone();
        value["bot"]["revision"] = revision;
        assert!(project(&value).is_err());
    }
    let mut missing = original.clone();
    missing["bot"].as_object_mut().unwrap().remove("revision");
    assert!(project(&missing).is_err());
    let mut extra = original.clone();
    extra["creation"] = Value::Null;
    assert!(project(&extra).is_err());
    let json = original.to_string();
    let duplicate = json.replacen("\"revision\":1", "\"revision\":1,\"revision\":1", 1);
    assert_ne!(duplicate, json);
    assert!(bot_creation_identity(&duplicate).is_err());
    assert!(bot_creation_identity("{").is_err());
    assert!(bot_creation_identity(&" ".repeat(BOT_RECEIPT_OUTCOME_LIMIT + 1)).is_err());
}

#[test]
fn creation_identity_projection_preserves_existing_id_parser_domains() {
    let mut value = legacy();
    // Store Chat/Workspace IDs accept non-v4 UUIDs; BotId remains strict v4.
    let nil = uuid::Uuid::nil().to_string();
    value["bot"]["home_workspace"] = json!(nil);
    value["bot"]["main_chat"] = json!(nil);
    assert!(project(&value).is_ok());
    value["bot"]["id"] = json!(nil);
    assert!(project(&value).is_err());
}

#[test]
fn creation_identity_projection_keeps_v2_readiness_and_abandonment_truthful() {
    for authored in [false, true] {
        let original = ready(authored);
        for phase in [
            BotCreationPhase::Intent,
            BotCreationPhase::FolderReady,
            BotCreationPhase::RolePending,
            BotCreationPhase::RoleVerified,
        ] {
            let mut c = original.clone();
            c.phase = phase;
            let expected = c.ready() && !c.abandoned();
            let value = serde_json::to_value(BotReceiptOutcome::CreationV2 {
                version: 2,
                creation: c,
            })
            .unwrap();
            assert_eq!(project(&value).is_ok(), expected);
        }
        let mut c = original.clone();
        c.abandoned = true;
        assert!(
            project(
                &serde_json::to_value(BotReceiptOutcome::CreationV2 {
                    version: 2,
                    creation: c
                })
                .unwrap()
            )
            .is_err()
        );
        let mut value = serde_json::to_value(BotReceiptOutcome::CreationV2 {
            version: 2,
            creation: original,
        })
        .unwrap();
        value["creation"]["reason"] = json!("store_unavailable");
        assert!(project(&value).is_err());
        value["creation"]["reason"] = Value::Null;
        value["creation"]["role"]["kind"] = json!("other");
        assert!(project(&value).is_err());
    }
}

#[test]
fn creation_identity_projection_intentionally_skips_execution_configuration() {
    let mut value = legacy();
    let expected = project(&value).unwrap();
    value["bot"]["config"] = json!({"model":{"unrecognized":"not current authority"}});
    assert_eq!(project(&value).unwrap(), expected);
    assert!(serde_json::from_value::<BotReceiptOutcome>(value).is_err());
}
