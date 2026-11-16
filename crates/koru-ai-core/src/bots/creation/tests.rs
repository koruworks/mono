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
            avatar: None,
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
        abandoned: false,
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
fn an_intent_that_fits_with_maximal_evidence_fits_at_every_later_step() {
    let mut record = authored(BotCreationPhase::Intent);
    let most = record.with_maximal_evidence();
    assert_eq!(most.phase, BotCreationPhase::RoleVerified);
    assert_eq!(most.role_page, Some(record.role_target()));
    assert!(most.reason.is_some());
    assert_eq!(
        most.folder_binding.as_ref().map(String::len),
        Some(BOT_FOLDER_PATH_LIMIT)
    );
    // Just under the bound at T1 as recorded, over it with the evidence a
    // later step adds: refused before any effect.
    let base = record.encode().unwrap().len();
    let grows = most.encode().map(|json| json.len()).unwrap_or(0) - base;
    assert!(grows > 6 * (BOT_FOLDER_PATH_LIMIT - 200), "{grows}");
    let seed_bytes = (BOT_RECEIPT_OUTCOME_LIMIT - base) / 2 - 1024 - 12;
    if let BotCreationRole::Authored { seed, .. } = &mut record.role {
        let mut bytes = seed.take().unwrap().0;
        bytes.extend(std::iter::repeat_n(7, seed_bytes));
        *seed = Some(BotSeedBytes(bytes));
    }
    assert!(record.encode().is_ok(), "fits as recorded");
    assert_eq!(
        record.with_maximal_evidence().encode(),
        Err(BotError::Invalid)
    );
    // Whatever binding, reason and page a later step records, an intent
    // whose maximal form fits stays within the bound.
    let mut fits = authored(BotCreationPhase::Intent);
    if let BotCreationRole::Authored { seed, .. } = &mut fits.role {
        *seed = Some(BotSeedBytes(vec![7; 300 * 1024]));
    }
    fits.with_maximal_evidence().encode().unwrap();
    let mut later = fits.clone();
    later.phase = BotCreationPhase::RoleVerified;
    later.role_page = Some(fits.role_target());
    later.folder_binding = Some(format!(
        "{}{}",
        root(),
        "\"".repeat(BOT_FOLDER_PATH_LIMIT - root().len())
    ));
    for reason in BotCreationReason::ALL {
        later.reason = Some(*reason);
        later.encode().unwrap();
    }
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

fn receipt(record: BotCreationRecord, stage: BotReceiptStage) -> BotCreationReceipt {
    BotCreationReceipt {
        request: BotRequestId::mint(),
        record,
        fingerprint: "f".repeat(64),
        stage,
    }
}

#[test]
fn a_page_that_may_exist_is_shown_unconfirmed_until_its_origin_is_verified() {
    let intent = authored(BotCreationPhase::FolderReady);
    let candidate = intent.role_target();
    // Before the Store is asked nothing of the page is shown.
    let view = receipt(intent.clone(), BotReceiptStage::Prepared).view(None);
    assert_eq!(view.role_page, None);
    assert!(!view.role_page_unconfirmed);
    assert_eq!(view.folder_binding, intent.folder_binding);
    // From `role_pending` it may exist: shown, unconfirmed, also parked.
    let mut pending = authored(BotCreationPhase::RolePending);
    pending.reason = Some(BotCreationReason::StoreUnavailable);
    let candidate_pending = pending.role_target();
    let view = receipt(pending.clone(), BotReceiptStage::Prepared).view(None);
    assert_eq!(view.role_page, Some(candidate_pending));
    assert!(view.role_page_unconfirmed);
    assert!(view.retryable);
    // A conflict means the identity holds another origin's page: not ours.
    pending.reason = Some(BotCreationReason::RoleConflict);
    let view = receipt(pending.clone(), BotReceiptStage::Prepared).view(None);
    assert_eq!(view.role_page, None);
    assert!(!view.retryable);
    // Verified: confirmed.
    let verified = authored(BotCreationPhase::RoleVerified);
    let view = receipt(verified.clone(), BotReceiptStage::Accepted).view(None);
    assert_eq!(view.role_page, Some(verified.role_target()));
    assert!(!view.role_page_unconfirmed);
    assert_ne!(candidate, verified.role_target());
}

#[test]
fn only_terminal_reasons_refuse_retry_and_an_abandoned_view_names_what_may_exist() {
    for reason in BotCreationReason::ALL {
        let terminal = matches!(
            reason,
            BotCreationReason::RoleConflict
                | BotCreationReason::RoleMissing
                | BotCreationReason::StoreRefused
                | BotCreationReason::FolderOccupied
                | BotCreationReason::FolderInvalid
        );
        assert_eq!(reason.retryable(), !terminal, "{reason:?}");
    }
    let mut ended = authored(BotCreationPhase::RolePending).without_seed();
    ended.reason = Some(BotCreationReason::StoreUnavailable);
    ended.abandoned = true;
    let view = receipt(ended.clone(), BotReceiptStage::Completed).view(None);
    assert_eq!(view.stage, BotCreationStage::Abandoned);
    assert!(!view.retryable);
    assert_eq!(view.bot, None);
    let BotCreationFolder::Managed { path, .. } = &ended.folder else {
        unreachable!()
    };
    assert_eq!(
        view.left_behind,
        Some(BotCreationLeftovers {
            role_page: Some(ended.role_target()),
            folder: Some(path.clone()),
        })
    );
    // What the disk says decides the folder; nothing found is not named.
    let seen = |seen| {
        receipt(ended.clone(), BotReceiptStage::Completed)
            .view_seen(None, seen)
            .left_behind
            .unwrap()
            .folder
    };
    assert_eq!(seen(BotFolderSeen::Present), Some(path.clone()));
    assert_eq!(seen(BotFolderSeen::Unknown), Some(path.clone()));
    assert_eq!(seen(BotFolderSeen::Absent), None);
    // A page the Store refused was never written, and one gone for good is
    // not there: neither is named.
    for reason in [
        BotCreationReason::StoreRefused,
        BotCreationReason::RoleMissing,
    ] {
        let mut gone = ended.clone();
        gone.reason = Some(reason);
        assert_eq!(
            receipt(gone, BotReceiptStage::Completed)
                .view(None)
                .left_behind
                .unwrap()
                .role_page,
            None,
            "{reason:?}"
        );
    }
    // A verified page is named, whatever stopped the creation after it.
    let mut verified = authored(BotCreationPhase::RoleVerified).without_seed();
    verified.role_page = Some(verified.role_target());
    verified.folder_binding = Some(path.clone());
    verified.reason = Some(BotCreationReason::PageUnavailable);
    verified.abandoned = true;
    assert_eq!(
        receipt(verified.clone(), BotReceiptStage::Completed)
            .view_seen(None, BotFolderSeen::Present)
            .left_behind,
        Some(BotCreationLeftovers {
            role_page: Some(verified.role_target()),
            folder: Some(path.clone()),
        })
    );
    // Stopped before its folder was marked: nothing is named.
    let mut early = authored(BotCreationPhase::Intent).without_seed();
    early.abandoned = true;
    assert_eq!(
        receipt(early, BotReceiptStage::Completed)
            .view(None)
            .left_behind,
        Some(BotCreationLeftovers {
            role_page: None,
            folder: None,
        })
    );
    // Abandonment may only be added, never taken back.
    let open = authored(BotCreationPhase::RolePending);
    let mut marked = open.without_seed();
    marked.abandoned = true;
    assert!(open.same_intent(&marked));
    assert!(!marked.same_intent(&open.without_seed()));
    // An abandoned record round-trips; an open one does not carry the field.
    assert_eq!(
        BotCreationRecord::decode(&marked.encode().unwrap()),
        Ok(Some(marked))
    );
    assert!(!open.encode().unwrap().contains("abandoned"));
}

/// A `CreateBotWithRole` with fixed identities, so its bytes can be pinned.
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

#[test]
fn a_creation_receipt_keeps_its_face_and_one_written_before_ai24_still_decodes() {
    let plain = authored(BotCreationPhase::Intent);
    let encoded = plain.encode().unwrap();
    assert!(!encoded.contains("avatar"), "{encoded}");
    assert_eq!(BotCreationRecord::decode(&encoded), Ok(Some(plain.clone())));

    let mut picked = plain.clone();
    picked.config.avatar = Some(23);
    let encoded = picked.encode().unwrap();
    assert!(encoded.contains(r#""avatar":23"#), "{encoded}");
    assert_eq!(
        BotCreationRecord::decode(&encoded),
        Ok(Some(picked.clone()))
    );
    // The face is part of the immutable intent, and it reaches the Bot.
    assert!(!plain.same_intent(&picked));
    assert!(picked.same_intent(&picked.without_seed()));
    let mut done = authored(BotCreationPhase::RoleVerified);
    done.config.avatar = Some(23);
    let bot = done.bot(9).unwrap();
    assert_eq!(bot.config.avatar, Some(23));
    assert_eq!(bot.avatar(), 23);
    let derived = authored(BotCreationPhase::RoleVerified).bot(9).unwrap();
    assert_eq!(derived.config.avatar, None);
    assert_eq!(derived.avatar(), derived_avatar(derived.id));
    // A face outside the table is refused before anything is stored.
    let mut outside = plain.clone();
    outside.config.avatar = Some(BOT_AVATAR_COUNT);
    assert_eq!(outside.config.validate(), Err(BotError::Invalid));
    assert_eq!(outside.encode(), Err(BotError::Invalid));
    // The maximal-evidence bound still holds with a face.
    assert!(picked.with_maximal_evidence().encode().is_ok());
}
