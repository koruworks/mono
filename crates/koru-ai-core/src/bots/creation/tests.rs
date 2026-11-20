// SPDX-License-Identifier: MPL-2.0
use super::*;

fn model() -> ModelSelection {
    ModelSelection {
        provider: crate::AiProviderId::parse("agent-codex").unwrap(),
        requested_model: "fixture".into(),
        default_reasoning: crate::ReasoningChoice::Default,
    }
}

fn fixed_create(avatar: Option<u8>) -> BotCommand {
    BotCommand::CreateBotWithRole {
        request_id: "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d".parse().unwrap(),
        bot_id: "6f1d2c3b-4a59-4e68-9777-8695a4b3c2d1".parse().unwrap(),
        home_workspace: WorkspaceId::parse("11111111-2222-4333-8444-555555555555").unwrap(),
        role: BotRoleInput::Authored {
            title: "Scout".into(),
            markdown: String::new(),
        },
        folder: BotFolderChoice::Managed {},
        config: BotCreationConfig {
            name: "Scout".into(),
            model: model(),
            mode: Some("auto".into()),
            fast: None,
            avatar,
        },
        access: BotAccess::Default,
        selected_pages: vec![],
    }
}

#[test]
fn the_wire_commands_are_closed_and_strict() {
    let workspace = WorkspaceId::new();
    let command = BotCommand::CreateBotWithRole {
        request_id: BotRequestId::mint(),
        bot_id: BotId::mint(),
        home_workspace: workspace,
        role: BotRoleInput::Authored {
            title: "Role".into(),
            markdown: "Find sources.".into(),
        },
        folder: BotFolderChoice::Managed {},
        config: BotCreationConfig {
            name: "Researcher".into(),
            model: model(),
            mode: None,
            fast: Some(false),
            avatar: None,
        },
        access: BotAccess::Default,
        selected_pages: vec![],
    };
    let json = serde_json::to_value(&command).unwrap();
    assert_eq!(json["command"], "create_bot_with_role");
    assert_eq!(json["role"]["kind"], "authored");
    assert_eq!(json["folder"], serde_json::json!({"kind": "managed"}));
    assert_eq!(
        serde_json::from_value::<BotCommand>(json.clone()).unwrap(),
        command
    );
    // The request names choices, never identities the owner derives.
    for (path, field) in [
        (&["folder"][..], "path"),
        (&["role"][..], "candidate"),
        (&["config"][..], "working_folder"),
        (&[][..], "user_scope"),
    ] {
        let mut changed = json.clone();
        let mut target = &mut changed;
        for key in path {
            target = &mut target[*key];
        }
        target[field] = serde_json::json!("x");
        assert!(
            serde_json::from_value::<BotCommand>(changed).is_err(),
            "{field}"
        );
    }
    let mut unknown_role = json.clone();
    unknown_role["role"] = serde_json::json!({"kind": "imported", "page": "x"});
    assert!(serde_json::from_value::<BotCommand>(unknown_role).is_err());
    let read = BotCommand::ReadBotCreation {
        request_id: BotRequestId::mint(),
    };
    assert_eq!(
        serde_json::to_value(&read).unwrap()["command"],
        "read_bot_creation"
    );
    let list = BotCommand::ListBotCreations {
        workspace,
        cursor: None,
        limit: 20,
    };
    assert_eq!(
        serde_json::to_value(&list).unwrap()["command"],
        "list_bot_creations"
    );
    // Retry and Abandon name the request alone, and nothing else.
    let request_id = BotRequestId::mint();
    for (command, tag) in [
        (
            BotCommand::RetryBotCreation { request_id },
            "retry_bot_creation",
        ),
        (
            BotCommand::AbandonBotCreation { request_id },
            "abandon_bot_creation",
        ),
    ] {
        let mut json = serde_json::to_value(&command).unwrap();
        assert_eq!(json["command"], tag);
        assert_eq!(
            serde_json::from_value::<BotCommand>(json.clone()).unwrap(),
            command
        );
        json["bot_id"] = serde_json::json!(BotId::mint());
        assert!(serde_json::from_value::<BotCommand>(json).is_err(), "{tag}");
    }
    // Debug never carries the authored text or a path.
    let shown = format!(
        "{:?}{:?}",
        BotRoleInput::Authored {
            title: "secret title".into(),
            markdown: "secret body".into()
        },
        BotFolderChoice::Selected {
            path: "/secret/path".into()
        }
    );
    assert!(!shown.contains("secret"), "{shown}");
}

#[test]
fn a_role_proof_matches_only_the_exact_origin() {
    let seed = BotRoleSeed {
        owner: BotOwner {
            user_scope: UserScopeId::new(),
            device: DeviceId::from_uuid(uuid::Uuid::new_v4()),
        },
        workspace: WorkspaceId::new(),
        profile: ProfileId::new(),
        title: "Role".into(),
        candidate: DocumentId::new(),
        request_key: uuid::Uuid::new_v4().to_string(),
        request_hash: "d".repeat(64),
        seed: vec![1, 2, 3],
    };
    let proof = BotRoleProof {
        scope: BOT_ROLE_SCOPE.into(),
        request_key: seed.request_key.clone(),
        request_hash: seed.request_hash.clone(),
        initial_update: seed.seed.clone(),
        placement: BotRolePlacement::Live,
    };
    assert!(proof.matches(&seed));
    for changed in [
        BotRoleProof {
            scope: "koru-native".into(),
            ..proof.clone()
        },
        BotRoleProof {
            request_key: uuid::Uuid::new_v4().to_string(),
            ..proof.clone()
        },
        BotRoleProof {
            request_hash: "e".repeat(64),
            ..proof.clone()
        },
        BotRoleProof {
            initial_update: vec![1, 2],
            ..proof.clone()
        },
    ] {
        assert!(!changed.matches(&seed));
    }
}

#[test]
fn a_creation_request_without_a_face_has_exactly_the_bytes_it_had_before_ai24() {
    // The owner fingerprints the request's JSON (blake3 over these bytes),
    // and a replay must match the fingerprint stored with the receipt. This
    // is the request as a build before the avatar field serialized it: if
    // it moves, every creation in flight across an upgrade is refused as
    // `request_conflict`.
    let plain = serde_json::to_string(&fixed_create(None)).unwrap();
    assert_eq!(
        plain,
        r#"{"command":"create_bot_with_role","request_id":"0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d","bot_id":"6f1d2c3b-4a59-4e68-9777-8695a4b3c2d1","home_workspace":"11111111-2222-4333-8444-555555555555","role":{"kind":"authored","title":"Scout","markdown":""},"folder":{"kind":"managed"},"config":{"name":"Scout","model":{"provider":"agent-codex","requested_model":"fixture","default_reasoning":"default"},"mode":"auto","fast":null},"access":"default","selected_pages":[]}"#
    );
    // Replayed, the same request gives the same bytes; a face changes them,
    // so the same request id with another face is a different request.
    assert_eq!(plain, serde_json::to_string(&fixed_create(None)).unwrap());
    let picked = serde_json::to_string(&fixed_create(Some(7))).unwrap();
    assert_eq!(
        picked,
        plain.replace(r#""fast":null}"#, r#""fast":null,"avatar":7}"#)
    );
    assert_eq!(
        picked,
        serde_json::to_string(&fixed_create(Some(7))).unwrap()
    );
    assert_ne!(
        picked,
        serde_json::to_string(&fixed_create(Some(8))).unwrap()
    );
    // Both read back as themselves, and a request that predates the field
    // reads back with no face.
    for command in [fixed_create(None), fixed_create(Some(7))] {
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(serde_json::from_str::<BotCommand>(&json).unwrap(), command);
    }
}
