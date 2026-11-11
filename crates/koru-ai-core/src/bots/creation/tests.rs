// SPDX-License-Identifier: MPL-2.0
use super::*;

fn model() -> ModelSelection {
    ModelSelection {
        provider: crate::AiProviderId::parse("agent-codex").unwrap(),
        requested_model: "fixture".into(),
        default_reasoning: crate::ReasoningChoice::Default,
    }
}

fn root() -> &'static str {
    if cfg!(windows) {
        "D:/koru/ai"
    } else {
        "/koru/ai"
    }
}

pub(crate) fn authored(phase: BotCreationPhase) -> BotCreationRecord {
    let bot = BotId::mint();
    let home = WorkspaceId::new();
    let candidate = DocumentId::new();
    let path = managed_folder(std::path::Path::new(root()), bot)
        .to_str()
        .unwrap()
        .to_owned();
    BotCreationRecord {
        bot,
        main_chat: ChatId::new(),
        user_scope: UserScopeId::new(),
        owner_device: DeviceId::from_uuid(uuid::Uuid::new_v4()),
        home_workspace: home,
        profile: ProfileId::new(),
        config: BotCreationConfig {
            name: "Researcher".into(),
            model: model(),
            mode: None,
            fast: None,
        },
        access: BotAccess::Default,
        selected_pages: vec![],
        role: BotCreationRole::Authored {
            store_request: uuid::Uuid::new_v4(),
            candidate,
            title: "Role".into(),
            seed: Some(BotSeedBytes(vec![0, 1, 0xab, 0xff])),
            seed_hash: "a".repeat(64),
            request_hash: "b".repeat(64),
        },
        folder: BotCreationFolder::Managed {
            root: root().into(),
            path: path.clone(),
        },
        phase,
        reason: None,
        folder_binding: (phase >= BotCreationPhase::FolderReady).then_some(path),
        role_page: (phase == BotCreationPhase::RoleVerified).then_some(BotPage {
            workspace: home,
            document: candidate,
        }),
        created_at_ms: 7,
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
fn creation_v2_decodes_and_v1_unknown_and_other_kinds_do_not_become_it() {
    let record = authored(BotCreationPhase::Intent);
    let json = record.encode().unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["kind"], "creation_v2");
    assert_eq!(value["version"], 2);
    // The seed is lowercase hex inside the JSON receipt.
    assert_eq!(value["creation"]["role"]["seed"], "0001abff");
    assert_eq!(
        BotCreationRecord::decode(&json).unwrap(),
        Some(record.clone())
    );

    for version in [1, 3, 0] {
        let mut changed = value.clone();
        changed["version"] = serde_json::json!(version);
        assert_eq!(
            BotCreationRecord::decode(&changed.to_string()),
            Err(BotError::Storage),
            "version {version}"
        );
    }
    let mut unknown = value.clone();
    unknown["kind"] = serde_json::json!("creation_v3");
    assert_eq!(
        BotCreationRecord::decode(&unknown.to_string()),
        Err(BotError::Storage)
    );
    let mut extra = value.clone();
    extra["creation"]["invented"] = serde_json::json!(true);
    assert_eq!(
        BotCreationRecord::decode(&extra.to_string()),
        Err(BotError::Storage)
    );
    let mut bad_hex = value.clone();
    bad_hex["creation"]["role"]["seed"] = serde_json::json!("0G");
    assert_eq!(
        BotCreationRecord::decode(&bad_hex.to_string()),
        Err(BotError::Storage)
    );
    // A v1 outcome of another kind is not a creation, and says so.
    let chat = BotReceiptOutcome::ChatCreated {
        version: 1,
        reference: BotChatReference {
            bot_id: record.bot,
            chat_id: record.main_chat,
        },
    };
    assert_eq!(
        BotCreationRecord::decode(&serde_json::to_string(&chat).unwrap()),
        Ok(None)
    );
    // The v1 Role creation coordination value keeps its own decoder.
    let legacy = serde_json::json!({
        "kind": "role_creation", "version": 1, "bot": record.bot,
        "store_request": uuid::Uuid::new_v4(), "destination": record.home_workspace,
        "request_fingerprint": "c".repeat(64), "verified_page": null, "verified_version": null
    });
    let decoded: BotReceiptOutcome = serde_json::from_value(legacy).unwrap();
    assert!(decoded.validate().is_ok());
    assert!(matches!(decoded, BotReceiptOutcome::RoleCreation { .. }));
}

#[test]
fn the_whole_outcome_is_bounded_before_any_effect() {
    let mut record = authored(BotCreationPhase::Intent);
    // Hex doubles the seed: 600 KiB of seed exceeds the 1 MiB receipt.
    if let BotCreationRole::Authored { seed, .. } = &mut record.role {
        *seed = Some(BotSeedBytes(vec![7; 600 * 1024]));
    }
    assert_eq!(record.encode(), Err(BotError::Invalid));
    if let BotCreationRole::Authored { seed, .. } = &mut record.role {
        *seed = Some(BotSeedBytes(vec![7; 400 * 1024]));
    }
    let json = record.encode().unwrap();
    assert!(json.len() <= BOT_RECEIPT_OUTCOME_LIMIT);
    let oversize = format!(
        "{json}{}",
        " ".repeat(BOT_RECEIPT_OUTCOME_LIMIT + 1 - json.len())
    );
    assert_eq!(BotCreationRecord::decode(&oversize), Err(BotError::Storage));
}

#[test]
fn groups_must_agree_with_the_phase_and_the_managed_path_with_the_bot() {
    for phase in [
        BotCreationPhase::Intent,
        BotCreationPhase::FolderPending,
        BotCreationPhase::FolderReady,
        BotCreationPhase::RolePending,
        BotCreationPhase::RoleVerified,
    ] {
        assert_eq!(authored(phase).validate(), Ok(()), "{phase:?}");
    }
    let mut early_binding = authored(BotCreationPhase::FolderPending);
    early_binding.folder_binding = Some(root().into());
    assert_eq!(early_binding.validate(), Err(BotError::Invalid));
    let mut unverified_page = authored(BotCreationPhase::RolePending);
    unverified_page.role_page = Some(unverified_page.role_target());
    assert_eq!(unverified_page.validate(), Err(BotError::Invalid));
    // A managed path is the BotId's under the root, never anything else.
    let mut named = authored(BotCreationPhase::Intent);
    if let BotCreationFolder::Managed { path, .. } = &mut named.folder {
        path.push_str("-Researcher");
    }
    assert_eq!(named.validate(), Err(BotError::Invalid));
    let mut other = authored(BotCreationPhase::Intent);
    other.bot = BotId::mint();
    assert_eq!(other.validate(), Err(BotError::Invalid));
    // An existing Role never reaches a Role phase.
    let mut existing = authored(BotCreationPhase::RolePending);
    existing.role = BotCreationRole::Existing {
        page: BotPage {
            workspace: existing.home_workspace,
            document: DocumentId::new(),
        },
    };
    existing.role_page = None;
    assert_eq!(existing.validate(), Err(BotError::Invalid));
    // Selected pages are the home workspace's, sorted and unique.
    let mut foreign = authored(BotCreationPhase::Intent);
    foreign.selected_pages = vec![BotPage {
        workspace: WorkspaceId::new(),
        document: DocumentId::new(),
    }];
    assert_eq!(foreign.validate(), Err(BotError::Invalid));
}

#[test]
fn only_the_phase_reason_and_evidence_may_change_and_the_phase_only_forward() {
    let intent = authored(BotCreationPhase::Intent);
    let mut ready = intent.clone();
    ready.phase = BotCreationPhase::FolderReady;
    ready.folder_binding = Some(root().into());
    assert!(intent.same_intent(&ready));
    assert!(!ready.same_intent(&intent));
    let mut renamed = ready.clone();
    renamed.config.name = "Other".into();
    assert!(!intent.same_intent(&renamed));
    let mut new_candidate = ready.clone();
    if let BotCreationRole::Authored { candidate, .. } = &mut new_candidate.role {
        *candidate = DocumentId::new();
    }
    assert!(!intent.same_intent(&new_candidate));
    // A seed may be dropped (completion, rejection), never replaced.
    assert!(intent.same_intent(&intent.without_seed()));
    let mut new_seed = intent.clone();
    if let BotCreationRole::Authored { seed, .. } = &mut new_seed.role {
        *seed = Some(BotSeedBytes(vec![9]));
    }
    assert!(!intent.same_intent(&new_seed));
    // Completion drops only the seed.
    let done = authored(BotCreationPhase::RoleVerified);
    let kept = done.without_seed();
    let BotCreationRole::Authored {
        seed, seed_hash, ..
    } = &kept.role
    else {
        panic!("authored")
    };
    assert!(seed.is_none());
    assert_eq!(seed_hash, &"a".repeat(64));
    assert_eq!(kept.validate(), Ok(()));
}

#[test]
fn the_bot_is_built_from_the_record_alone() {
    let record = authored(BotCreationPhase::RoleVerified);
    assert!(record.ready());
    let bot = record.bot(11).unwrap();
    assert_eq!(bot.id, record.bot);
    assert_eq!(bot.main_chat, record.main_chat);
    assert_eq!(bot.config.role, record.role_target());
    assert_eq!(
        Some(&bot.config.working_folder),
        record.folder_binding.as_ref()
    );
    assert_eq!((bot.revision, bot.policy_revision), (1, 1));
    let mut parked = record.clone();
    parked.reason = Some(BotCreationReason::StorageFailed);
    assert!(!parked.ready());
    assert!(!authored(BotCreationPhase::RolePending).ready());
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
        page_live: true,
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
