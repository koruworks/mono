// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::BotBootAdmission;
use rusqlite::types::Value;

fn fixture() -> (Repository, Bot, BotOwner, BotRequestId) {
    let mut repo = Repository::in_memory().unwrap();
    let bot = super::super::tests::fixture_bot();
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let request = BotRequestId::mint();
    repo.save_bot(&bot, ProfileId::new(), request, "create", None)
        .unwrap();
    (repo, bot, owner, request)
}
fn selected(
    repo: &Repository,
    owner: BotOwner,
    bot: BotId,
) -> Result<koru_delegate_core::BotAdmissionBarrier, BotError> {
    repo.bot_admission_barrier(
        &repo.bot_policy_metadata(owner, bot)?,
        &BotBootAdmission::Missing {},
    )
}
fn corrupt(repo: &Repository, bot: BotId, column: &str, value: Value) {
    repo.connection
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    repo.connection
        .execute(
            &format!("UPDATE bots SET {column}=?2 WHERE bot_id=?1"),
            params![bot.to_storage(), value],
        )
        .unwrap();
}

#[test]
fn metadata_reads_canonical_policy_without_execution_configuration() {
    let (repo, bot, owner, request) = fixture();
    let initial = repo.bot_policy_metadata(owner, bot.id).unwrap();
    assert_eq!(initial.policy(), bot_native_policy(&bot));
    assert_eq!(initial.main_chat, bot.main_chat);
    assert_eq!(initial.lifecycle, BotLifecycle::Active);
    for column in [
        "name",
        "role_workspace_id",
        "role_document_id",
        "working_folder",
        "provider",
        "model",
        "reasoning",
        "mode",
        "fast",
        "folder_lock_request_id",
        "folder_frozen_at_ms",
    ] {
        corrupt(&repo, bot.id, column, Value::Blob(vec![0; 65536]));
    }
    assert_eq!(repo.bot_policy_metadata(owner, bot.id).unwrap(), initial);
    assert_eq!(selected(&repo, owner, bot.id).unwrap().request, request);
    assert!(
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {})
            .is_err()
    );
    assert!(!format!("{initial:?}").contains(&bot.config.name));
}

#[test]
fn metadata_rejects_scalar_sql_types_bounds_and_foreign_owner() {
    for column in [
        "user_scope_id",
        "owner_device_id",
        "home_workspace_id",
        "revision",
        "policy_revision",
        "paused",
        "lifecycle",
        "access",
    ] {
        for invalid in [Value::Blob(vec![b'x'; 36]), Value::Text("x".repeat(65536))] {
            let (repo, bot, owner, _) = fixture();
            corrupt(&repo, bot.id, column, invalid);
            assert!(
                matches!(
                    repo.bot_policy_metadata(owner, bot.id),
                    Err(BotError::Storage)
                ),
                "{column}"
            );
        }
    }
    for column in ["revision", "policy_revision"] {
        for invalid in [
            Value::Integer(0),
            Value::Integer(-1),
            Value::Real(1.5),
            Value::Real(i64::MAX as f64 * 2.0),
        ] {
            let (repo, bot, owner, _) = fixture();
            corrupt(&repo, bot.id, column, invalid);
            assert!(
                matches!(
                    repo.bot_policy_metadata(owner, bot.id),
                    Err(BotError::Storage)
                ),
                "{column}"
            );
        }
    }
    for invalid in [Value::Integer(2), Value::Integer(-1), Value::Real(0.5)] {
        let (repo, bot, owner, _) = fixture();
        corrupt(&repo, bot.id, "paused", invalid);
        assert!(matches!(
            repo.bot_policy_metadata(owner, bot.id),
            Err(BotError::Storage)
        ));
    }
    let (repo, bot, owner, _) = fixture();
    assert!(matches!(
        repo.bot_policy_metadata(
            BotOwner {
                device: DeviceId::new(),
                ..owner
            },
            bot.id
        ),
        Err(BotError::Forbidden)
    ));
    assert!(matches!(
        repo.bot_policy_metadata(owner, BotId::mint()),
        Err(BotError::NotFound)
    ));
    corrupt(
        &repo,
        bot.id,
        "home_workspace_id",
        Value::Text("x".repeat(36)),
    );
    assert!(matches!(
        repo.bot_policy_metadata(owner, bot.id),
        Err(BotError::Storage)
    ));
}

#[test]
fn metadata_rejects_malformed_or_missing_main_identity() {
    for value in [
        Value::Blob(vec![0; 36]),
        Value::Text("x".repeat(36)),
        Value::Text("x".repeat(65536)),
    ] {
        let (repo, bot, owner, _) = fixture();
        repo.connection
            .execute_batch("PRAGMA foreign_keys=OFF")
            .unwrap();
        repo.connection
            .execute(
                "UPDATE bot_chats SET chat_id=?2 WHERE bot_id=?1 AND purpose='main'",
                params![bot.id.to_storage(), value],
            )
            .unwrap();
        assert!(matches!(
            repo.bot_policy_metadata(owner, bot.id),
            Err(BotError::Storage)
        ));
    }
    let (repo, bot, owner, _) = fixture();
    repo.connection
        .execute(
            "DELETE FROM bot_chats WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert!(matches!(
        repo.bot_policy_metadata(owner, bot.id),
        Err(BotError::Storage)
    ));
}

#[test]
fn metadata_page_bound_is_1024_with_the_1025th_row_only_a_sentinel() {
    let (repo, bot, owner, _) = fixture();
    for _ in 0..1024 {
        repo.connection
            .execute(
                "INSERT INTO bot_selected_pages VALUES(?1,?2,?3,1)",
                params![
                    bot.id.to_storage(),
                    bot.home_workspace.to_storage(),
                    DocumentId::new().to_storage()
                ],
            )
            .unwrap();
    }
    assert_eq!(
        repo.bot_policy_metadata(owner, bot.id)
            .unwrap()
            .selected_pages
            .len(),
        1024
    );
    repo.connection
        .execute(
            "INSERT INTO bot_selected_pages VALUES(?1,?2,?3,1)",
            params![
                bot.id.to_storage(),
                bot.home_workspace.to_storage(),
                DocumentId::new().to_storage()
            ],
        )
        .unwrap();
    assert!(matches!(
        repo.bot_policy_metadata(owner, bot.id),
        Err(BotError::Storage)
    ));
}

#[test]
fn metadata_pages_refuse_foreign_malformed_and_duplicate_typed_identities() {
    for (workspace, document) in [
        (
            Value::Text(WorkspaceId::new().to_storage()),
            Value::Text(DocumentId::new().to_storage()),
        ),
        (
            Value::Blob(vec![0; 36]),
            Value::Text(DocumentId::new().to_storage()),
        ),
        (Value::Text("home".into()), Value::Text("x".repeat(65536))),
    ] {
        let (repo, bot, owner, _) = fixture();
        let workspace = if workspace == Value::Text("home".into()) {
            Value::Text(bot.home_workspace.to_storage())
        } else {
            workspace
        };
        repo.connection
            .execute(
                "INSERT INTO bot_selected_pages VALUES(?1,?2,?3,1)",
                params![bot.id.to_storage(), workspace, document],
            )
            .unwrap();
        assert!(matches!(
            repo.bot_policy_metadata(owner, bot.id),
            Err(BotError::Storage)
        ));
    }
    let (repo, bot, owner, _) = fixture();
    let lower = "abcdefab-0000-4000-8000-000000000001";
    for document in [lower.to_owned(), lower.to_uppercase()] {
        repo.connection
            .execute(
                "INSERT INTO bot_selected_pages VALUES(?1,?2,?3,1)",
                params![
                    bot.id.to_storage(),
                    bot.home_workspace.to_storage(),
                    document
                ],
            )
            .unwrap();
    }
    assert!(matches!(
        repo.bot_policy_metadata(owner, bot.id),
        Err(BotError::Storage)
    ));
}

#[test]
fn metadata_selector_rechecks_complete_snapshot_and_matches_boot_keys() {
    let (mut repo, bot, owner, request) = fixture();
    let metadata = repo.bot_policy_metadata(owner, bot.id).unwrap();
    let first = selected(&repo, owner, bot.id).unwrap();
    assert_eq!(first.request, request);
    assert_eq!(
        first,
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {})
            .unwrap()
            .admission
    );
    let mut changed = metadata.clone();
    changed.paused = true;
    assert!(matches!(
        repo.bot_admission_barrier(&changed, &BotBootAdmission::Missing {}),
        Err(BotError::RevisionConflict)
    ));
    let pause = repo
        .prepare_bot_pause(
            owner,
            bot.id,
            BotRequestId::mint(),
            1,
            true,
            &"a".repeat(64),
            2,
        )
        .unwrap();
    let current = repo.bot_policy_metadata(owner, bot.id).unwrap();
    assert_eq!(
        current.pending_change,
        Some((pause.intent.barrier.request, BotOperationKind::Pause))
    );
    assert!(matches!(
        repo.bot_admission_barrier(&metadata, &BotBootAdmission::Missing {}),
        Err(BotError::RevisionConflict)
    ));
    assert_eq!(
        selected(&repo, owner, bot.id).unwrap(),
        pause.intent.barrier
    );
    assert_eq!(
        selected(&repo, owner, bot.id).unwrap(),
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {})
            .unwrap()
            .admission
    );
    let mut other = pause.intent.barrier.clone();
    other.request = BotRequestId::mint();
    assert!(matches!(
        repo.bot_admission_barrier(&current, &BotBootAdmission::Present { barrier: other }),
        Err(BotError::Conflict)
    ));
    assert!(matches!(
        repo.bot_admission_barrier(&current, &BotBootAdmission::Unavailable {}),
        Err(BotError::Unavailable)
    ));
}

fn capture(bot: &Bot, owner: BotOwner, kind: BotLifecycleKind) -> LifecycleCapture {
    LifecycleCapture {
        operation: BotRequestId::mint(),
        owner,
        bot: bot.id,
        kind,
        chat: kind.names_chat().then_some(bot.main_chat),
        fingerprint: "a".repeat(64),
        expected_revision: bot.revision,
        source: None,
        members: vec![],
        now: 3,
    }
}
#[test]
fn metadata_whole_bot_pending_and_chat_only_lifecycle_preserve_boot_selection() {
    for kind in [BotLifecycleKind::Archive, BotLifecycleKind::TrashChat] {
        let (mut repo, bot, owner, create) = fixture();
        let operation = repo
            .capture_bot_lifecycle(&capture(&bot, owner, kind))
            .unwrap();
        let metadata = repo.bot_policy_metadata(owner, bot.id).unwrap();
        assert_eq!(
            metadata.pending_lifecycle,
            (!kind.names_chat()).then_some(operation.operation)
        );
        let admission = selected(&repo, owner, bot.id).unwrap();
        assert_eq!(
            admission,
            repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {})
                .unwrap()
                .admission
        );
        assert_eq!(admission.request, operation.pause_request.unwrap_or(create));
    }
}

#[test]
fn metadata_pause_projection_omits_folder_but_full_boot_keeps_guard_errors() {
    for column in [
        "working_folder",
        "folder_lock_request_id",
        "folder_frozen_at_ms",
    ] {
        let (mut repo, bot, owner, _) = fixture();
        let pause = repo
            .prepare_bot_pause(
                owner,
                bot.id,
                BotRequestId::mint(),
                1,
                true,
                &"a".repeat(64),
                2,
            )
            .unwrap();
        corrupt(&repo, bot.id, column, Value::Blob(vec![0; 36]));
        assert_eq!(
            selected(&repo, owner, bot.id).unwrap(),
            pause.intent.barrier
        );
        assert!(matches!(
            admission::pause_on(
                &repo.connection,
                owner.user_scope,
                pause.intent.barrier.request
            ),
            Err(BotError::Storage)
        ));
        assert!(
            repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {})
                .is_err()
        );
    }
    let (mut repo, bot, owner, _) = fixture();
    let pause = repo
        .prepare_bot_pause(
            owner,
            bot.id,
            BotRequestId::mint(),
            1,
            true,
            &"a".repeat(64),
            2,
        )
        .unwrap();
    repo.fixture_set_bot_lifecycle(bot.id, BotLifecycle::Archived)
        .unwrap();
    assert!(matches!(
        selected(&repo, owner, bot.id),
        Err(BotError::Forbidden)
    ));
    assert!(matches!(
        admission::pause_on(
            &repo.connection,
            owner.user_scope,
            pause.intent.barrier.request
        ),
        Err(BotError::Forbidden)
    ));
}

#[test]
fn metadata_creation_projection_refuses_foreign_duplicate_and_oversize_receipts() {
    let (repo, bot, owner, request) = fixture();
    let original: String = repo
        .connection
        .query_row(
            "SELECT outcome_json FROM bot_command_receipts WHERE request_id=?1",
            [request.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    for field in [
        "id",
        "user_scope",
        "owner_device",
        "home_workspace",
        "main_chat",
    ] {
        let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
        value["bot"][field] = serde_json::json!(uuid::Uuid::new_v4().to_string());
        repo.connection
            .execute(
                "UPDATE bot_command_receipts SET outcome_json=?2 WHERE request_id=?1",
                params![request.to_storage(), value.to_string()],
            )
            .unwrap();
        assert!(
            matches!(selected(&repo, owner, bot.id), Err(BotError::Storage)),
            "{field}"
        );
    }
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET outcome_json=?2 WHERE request_id=?1",
            params![request.to_storage(), original],
        )
        .unwrap();
    let duplicate = BotRequestId::mint();
    repo.connection.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms) SELECT user_scope_id,?2,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms FROM bot_command_receipts WHERE request_id=?1",params![request.to_storage(),duplicate.to_storage()]).unwrap();
    assert!(matches!(
        selected(&repo, owner, bot.id),
        Err(BotError::Conflict)
    ));
    repo.connection
        .execute(
            "DELETE FROM bot_command_receipts WHERE request_id=?1",
            [duplicate.to_storage()],
        )
        .unwrap();
    repo.connection
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    for invalid in ["{".to_owned(), " ".repeat(1048577)] {
        repo.connection
            .execute(
                "UPDATE bot_command_receipts SET outcome_json=?2 WHERE request_id=?1",
                params![request.to_storage(), invalid],
            )
            .unwrap();
        assert!(matches!(
            selected(&repo, owner, bot.id),
            Err(BotError::Storage)
        ));
    }
}

#[test]
fn metadata_identity_only_creation_does_not_weaken_full_boot_validation() {
    let (repo, bot, owner, request) = fixture();
    let json: String = repo
        .connection
        .query_row(
            "SELECT outcome_json FROM bot_command_receipts WHERE request_id=?1",
            [request.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value["bot"]["config"]["working_folder"] = serde_json::json!("");
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET outcome_json=?2 WHERE request_id=?1",
            params![request.to_storage(), value.to_string()],
        )
        .unwrap();
    assert_eq!(selected(&repo, owner, bot.id).unwrap().request, request);
    assert!(matches!(
        repo.bot_boot_snapshot(owner, bot.id, &BotBootAdmission::Missing {}),
        Err(BotError::Storage)
    ));
}
