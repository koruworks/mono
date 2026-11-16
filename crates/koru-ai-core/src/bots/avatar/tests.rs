// SPDX-License-Identifier: MPL-2.0
use super::super::*;
use crate::{DocumentId, WorkspaceId};

fn id(text: &str) -> BotId {
    text.parse().unwrap()
}

fn config(home: WorkspaceId, avatar: Option<u8>) -> BotConfig {
    BotConfig {
        name: "Scout".into(),
        role: BotPage {
            workspace: home,
            document: DocumentId::new(),
        },
        working_folder: if cfg!(windows) { "D:/work" } else { "/work" }.into(),
        model: ModelSelection {
            provider: crate::AiProviderId::parse("agent-codex").unwrap(),
            requested_model: "fixture".into(),
            default_reasoning: crate::ReasoningChoice::Default,
        },
        mode: None,
        fast: None,
        avatar,
    }
}

#[test]
fn the_derived_face_of_an_id_is_pinned_for_ever() {
    // FNV-1a 64 over the id's 16 bytes, modulo 24, computed outside this
    // crate. A change to any of these changes the face of Bots people
    // already know: it needs a stored value for every Bot first.
    for (bot, face) in [
        ("00000000-0000-4000-8000-000000000000", 13),
        ("11111111-2222-4333-8444-555555555555", 21),
        ("ffffffff-ffff-4fff-bfff-ffffffffffff", 13),
        ("6f1d2c3b-4a59-4e68-9777-8695a4b3c2d1", 8),
        ("0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d", 5),
    ] {
        assert_eq!(derived_avatar(id(bot)), face, "{bot}");
        // Stable: asked again, and through the Bot-facing rule.
        assert_eq!(derived_avatar(id(bot)), face);
        assert_eq!(bot_avatar(id(bot), None), face);
    }
}

#[test]
fn every_derived_face_is_inside_the_table_and_every_face_is_reached() {
    let mut seen = [false; BOT_AVATAR_COUNT as usize];
    for _ in 0..2000 {
        let face = derived_avatar(BotId::mint());
        assert!(face < BOT_AVATAR_COUNT);
        seen[usize::from(face)] = true;
    }
    assert!(seen.iter().all(|seen| *seen), "{seen:?}");
}

#[test]
fn a_stored_choice_wins_and_one_outside_the_table_falls_back_to_the_derived_face() {
    let bot = id("6f1d2c3b-4a59-4e68-9777-8695a4b3c2d1");
    for face in 0..BOT_AVATAR_COUNT {
        assert_eq!(bot_avatar(bot, Some(face)), face);
        assert_eq!(validate_avatar(Some(face)), Ok(()));
    }
    assert_eq!(validate_avatar(None), Ok(()));
    for outside in [BOT_AVATAR_COUNT, 100, u8::MAX] {
        assert_eq!(validate_avatar(Some(outside)), Err(BotError::Invalid));
        assert_eq!(bot_avatar(bot, Some(outside)), 8);
    }
}

#[test]
fn a_config_without_a_face_has_the_fields_it_had_before_the_field_existed() {
    let home = WorkspaceId::new();
    let plain = config(home, None);
    let json = serde_json::to_value(&plain).unwrap();
    // Exactly the six fields of ai23 and before: nothing named avatar.
    let mut keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["fast", "mode", "model", "name", "role", "working_folder"]
    );
    // What an older build stored or sent reads back with no face picked.
    assert_eq!(serde_json::from_value::<BotConfig>(json).unwrap(), plain);
    assert_eq!(validate_config(&plain, home), Ok(()));

    let picked = config(home, Some(23));
    let json = serde_json::to_value(&picked).unwrap();
    assert_eq!(json["avatar"], 23);
    assert_eq!(
        serde_json::from_value::<BotConfig>(json.clone()).unwrap(),
        picked
    );
    assert_eq!(validate_config(&picked, home), Ok(()));
    // An explicit null is the same as absent; other fields stay refused.
    let mut null = json.clone();
    null["avatar"] = serde_json::Value::Null;
    assert_eq!(
        serde_json::from_value::<BotConfig>(null).unwrap().avatar,
        None
    );
    let mut unknown = json.clone();
    unknown["face"] = serde_json::json!(1);
    assert!(serde_json::from_value::<BotConfig>(unknown).is_err());
    for wrong in [
        serde_json::json!(-1),
        serde_json::json!(256),
        serde_json::json!("7"),
    ] {
        let mut bad = json.clone();
        bad["avatar"] = wrong;
        assert!(serde_json::from_value::<BotConfig>(bad).is_err());
    }
    // In range for a byte, outside the table: decoded, then refused.
    assert_eq!(
        validate_config(&config(home, Some(BOT_AVATAR_COUNT)), home),
        Err(BotError::Invalid)
    );
}

#[test]
fn an_update_carries_the_face_and_one_without_it_serializes_as_before() {
    let home = WorkspaceId::new();
    let request = BotRequestId::mint();
    let bot = BotId::mint();
    let base = config(home, None);
    let command = |avatar| BotCommand::Update {
        request_id: request,
        bot_id: bot,
        expected_revision: 1,
        config: BotConfig {
            avatar,
            ..base.clone()
        },
    };
    let plain = serde_json::to_string(&command(None)).unwrap();
    assert!(!plain.contains("avatar"), "{plain}");
    let picked = serde_json::to_string(&command(Some(4))).unwrap();
    assert!(picked.contains(r#""avatar":4"#), "{picked}");
    assert_eq!(
        serde_json::from_str::<BotCommand>(&picked).unwrap(),
        command(Some(4))
    );
    assert_eq!(
        serde_json::from_str::<BotCommand>(&plain).unwrap(),
        command(None)
    );
}
