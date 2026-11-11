// SPDX-License-Identifier: MPL-2.0
use super::super::tests::{chat, complete_turn};
use super::*;
pub(super) mod branch;
mod conversations;
use koru_ai_core::{
    AiSearchQuery, ChatAccessLens, ListChatsToolRequest, ReadChatToolRequest,
    SearchChatsToolRequest,
};

#[test]
fn shared_settings_patch_requires_explicit_bot_read_access_before_mutation() {
    let mut repo = Repository::in_memory().unwrap();
    let bot = fixture_bot();
    repo.save_bot(&bot, ProfileId::new(), BotRequestId::mint(), "create", None)
        .unwrap();
    let access = ChatReadAccess::authorized(vec![bot.main_chat]);
    let original = repo
        .chat_with_access(bot.main_chat, &access)
        .unwrap()
        .unwrap();
    assert!(
        repo.patch_chat_agent_settings(
            bot.main_chat,
            Some("safe"),
            Some(true),
            &ChatReadAccess::ordinary(),
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        repo.chat_with_access(bot.main_chat, &access).unwrap(),
        Some(original.clone())
    );
    let changed = repo
        .patch_chat_agent_settings(bot.main_chat, Some("safe"), Some(true), &access)
        .unwrap()
        .unwrap();
    assert_eq!(changed.agent_mode.as_deref(), Some("safe"));
    assert_eq!(changed.agent_fast, Some(true));
    assert_eq!(changed.updated_at, original.updated_at);
}

#[test]
fn populated_bot_history_is_filtered_before_search_paging_context_tools_and_usage() {
    let mut repo = Repository::in_memory().unwrap();
    let bot = fixture_bot();
    repo.save_bot(&bot, ProfileId::new(), BotRequestId::mint(), "create", None)
        .unwrap();
    let ordinary = ChatId::new();
    repo.create_chat(&chat(ordinary, 0)).unwrap();
    let hidden = complete_turn(&mut repo, bot.main_chat, 0, "sharedneedle hidden", 1);
    let visible = complete_turn(&mut repo, ordinary, 0, "sharedneedle visible", 10);
    let access = ChatReadAccess::authorized(vec![bot.main_chat]);
    assert_eq!(repo.chat_overviews().unwrap()[0].message_count, 2);
    assert_eq!(repo.chat_overviews_with_access(&access).unwrap().len(), 2);
    let query = AiSearchQuery {
        text: "sharedneedle".into(),
        limit: 1,
        cursor: None,
        visibility: ChatAccessLens::UserSurface,
        profile: None,
        removed_profiles: vec![],
    };
    let page = repo.search(&query).unwrap();
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].message_id, visible.assistant_message.id);
    assert!(
        page.next_cursor.is_none(),
        "hidden rows must not produce a next page"
    );
    let mut both = query.clone();
    both.limit = 10;
    assert_eq!(
        repo.search_with_access(&both, &access).unwrap().hits.len(),
        2
    );
    let block = repo.blocks(bot.main_chat).unwrap()[0].id;
    assert!(repo.message_block_context(block).unwrap().is_none());
    assert!(
        repo.message_block_context_with_access(block, &access)
            .unwrap()
            .is_some()
    );
    let lens = ChatAccessLens::ToolSurface { calling_chat: None };
    let listed =
        koru_ai_core::list_chats(&repo, lens, ListChatsToolRequest::default(), &[]).unwrap();
    assert_eq!(listed.chats.len(), 1);
    assert_eq!(listed.chats[0].chat_id, ordinary);
    let searched = koru_ai_core::search_chats(
        &repo,
        lens,
        SearchChatsToolRequest {
            query: "sharedneedle".into(),
            cursor: None,
            page_size: Some(1),
        },
        &[],
    )
    .unwrap();
    assert_eq!(searched.hits.len(), 1);
    assert_eq!(searched.hits[0].chat_id, ordinary);
    assert!(
        koru_ai_core::read_chat(
            &repo,
            lens,
            ReadChatToolRequest {
                chat_id: bot.main_chat,
                cursor: None,
                page_size: None
            }
        )
        .is_err()
    );
    assert!(repo.usage(Some(bot.main_chat)).is_err());
    assert_eq!(
        repo.usage(None)
            .unwrap()
            .groups
            .iter()
            .map(|g| g.totals.attempts)
            .sum::<u64>(),
        1
    );
    assert_eq!(
        repo.usage_with_access(None, &access)
            .unwrap()
            .groups
            .iter()
            .map(|g| g.totals.attempts)
            .sum::<u64>(),
        2
    );
    repo.clear_usage().unwrap();
    assert!(repo.attempt(hidden.attempt.id).unwrap().is_some());
    assert!(repo.attempt(visible.attempt.id).unwrap().is_none());
    assert_eq!(repo.clear_history().unwrap(), 1);
    assert_eq!(repo.messages(bot.main_chat).unwrap().len(), 2);
}

pub(super) fn fixture_bot() -> Bot {
    let workspace = WorkspaceId::new();
    Bot {
        id: BotId::mint(),
        home_workspace: workspace,
        user_scope: UserScopeId::new(),
        owner_device: DeviceId::from_uuid(Uuid::new_v4()),
        config: BotConfig {
            name: "Fixture Bot".into(),
            role: BotPage {
                workspace,
                document: DocumentId::new(),
            },
            working_folder: if cfg!(windows) {
                "D:/disposable"
            } else {
                "/disposable"
            }
            .into(),
            model: ModelSelection {
                provider: AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
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
        lifecycle: Default::default(),
        open_operation: None,
        held_by: None,
        pending_change: None,
    }
}

#[test]
fn bot_creation_receipt_and_main_chat_are_atomic_and_cas_is_strict() {
    let mut repo = Repository::in_memory().unwrap();
    let mut bot = fixture_bot();
    let request = BotRequestId::mint();
    let profile = ProfileId::new();
    assert_eq!(
        repo.save_bot(&bot, profile, request, "one", None).unwrap(),
        bot
    );
    assert_eq!(
        repo.save_bot(&bot, profile, request, "one", None).unwrap(),
        bot
    );
    assert_eq!(
        repo.bot_receipt(bot.user_scope, request, "different"),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.bot_receipt(UserScopeId::new(), request, "one")
            .unwrap(),
        None
    );
    assert!(repo.chat(bot.main_chat).unwrap().is_none());
    let access = ChatReadAccess::authorized(vec![bot.main_chat]);
    assert_eq!(
        repo.chat_with_access(bot.main_chat, &access)
            .unwrap()
            .unwrap()
            .id,
        bot.main_chat
    );
    assert!(repo.chats().unwrap().is_empty());
    assert!(repo.chat_overviews().unwrap().is_empty());
    assert_eq!(repo.chats_with_access(&access).unwrap().len(), 1);
    assert_eq!(
        repo.chat_overviews_with_access(&access).unwrap()[0].message_count,
        0
    );
    bot.config.name = "Changed".into();
    bot.revision = 2;
    assert!(
        repo.save_bot(&bot, profile, BotRequestId::mint(), "two", Some(1))
            .is_ok()
    );
    assert_eq!(
        repo.save_bot(&bot, profile, BotRequestId::mint(), "stale", Some(1)),
        Err(BotError::RevisionConflict)
    );
    assert_eq!(repo.bot(bot.id).unwrap().unwrap().config.name, "Changed");
    let mut conflict = fixture_bot();
    conflict.main_chat = bot.main_chat;
    assert!(
        repo.save_bot(&conflict, profile, BotRequestId::mint(), "conflict", None)
            .is_err()
    );
    assert!(repo.bot(conflict.id).unwrap().is_none());
}

#[test]
fn uncertain_first_submission_keeps_folder_fenced_until_definite_settlement() {
    let mut repo = Repository::in_memory().unwrap();
    let mut bot = fixture_bot();
    let profile = ProfileId::new();
    repo.save_bot(&bot, profile, BotRequestId::mint(), "create", None)
        .unwrap();
    let request = BotRequestId::mint();
    repo.lock_bot_folder(bot.id, 1, request).unwrap();
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_lock_request,
        Some(request)
    );
    bot.config.working_folder.push_str("/different");
    bot.revision = 2;
    assert_eq!(
        repo.save_bot(&bot, profile, BotRequestId::mint(), "edit", Some(1)),
        Err(BotError::FolderLocked)
    );
    repo.settle_bot_folder(bot.id, BotRequestId::mint(), None)
        .unwrap();
    assert!(
        repo.bot(bot.id)
            .unwrap()
            .unwrap()
            .folder_lock_request
            .is_some()
    );
    repo.settle_bot_folder(bot.id, request, None).unwrap();
    repo.save_bot(&bot, profile, BotRequestId::mint(), "edit", Some(1))
        .unwrap();
    repo.lock_bot_folder(bot.id, 2, request).unwrap();
    repo.settle_bot_folder(bot.id, request, Some(12)).unwrap();
    let stored = repo.bot(bot.id).unwrap().unwrap();
    assert_eq!(stored.folder_frozen_at_ms, Some(12));
    repo.settle_bot_folder(bot.id, request, None).unwrap();
    assert_eq!(
        repo.bot(bot.id).unwrap().unwrap().folder_frozen_at_ms,
        Some(12)
    );
}

#[test]
fn bulk_history_and_usage_do_not_destroy_bot_associations() {
    let mut repo = Repository::in_memory().unwrap();
    let bot = fixture_bot();
    repo.save_bot(&bot, ProfileId::new(), BotRequestId::mint(), "create", None)
        .unwrap();
    assert_eq!(repo.clear_history().unwrap(), 0);
    repo.clear_usage().unwrap();
    assert_eq!(repo.bot_for_chat(bot.main_chat).unwrap(), Some(bot.id));
    assert!(repo.delete_chat(bot.main_chat).is_err());
}

#[test]
fn receipt_nullable_groups_refuse_every_missing_execution_member() {
    let repo = Repository::in_memory().unwrap();
    let insert = "INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,captured_config_revision,captured_policy_revision,submission_version,submission_json,created_at_ms,updated_at_ms) VALUES('s',?1,'future','f','start','prepared',?2,?3,?4,?5,?6,1,1)";
    for missing in 0..5 {
        let members: [Option<&str>; 5] = [Some("run"), Some("1"), Some("1"), Some("1"), Some("{}")];
        let mut v = members;
        v[missing] = None;
        assert!(
            repo.connection
                .execute(
                    insert,
                    params![format!("bad-{missing}"), v[0], v[1], v[2], v[3], v[4]]
                )
                .is_err(),
            "missing member {missing}"
        );
    }
    assert!(
        repo.connection
            .execute(insert, params!["valid", "run", 1, 1, 1, "{}"])
            .is_ok()
    );
    assert!(
        repo.connection
            .execute(insert, params!["duplicate", "run", 1, 1, 1, "{}"])
            .is_err()
    );
    // SQL shape only: a prepared Role receipt may precede its Bot row. Typed payload validation is separate.
    assert!(repo.connection.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms) VALUES('s','role','future','f','create','prepared','{}',1,1)",[]).is_ok());
    assert!(repo.connection.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,desired_pause,expected_revision,pause_ack_stage,created_at_ms,updated_at_ms) VALUES('s','pause','future','f','pause','prepared',0,1,'pending',1,1)",[]).is_err());
}

#[test]
fn receipt_failure_rolls_back_late_create_and_update_writes() {
    let mut repo = Repository::in_memory().unwrap();
    let mut bot = fixture_bot();
    let profile = ProfileId::new();
    repo.connection.execute_batch("CREATE TRIGGER fail_bot_receipt BEFORE INSERT ON bot_command_receipts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert!(
        repo.save_bot(&bot, profile, BotRequestId::mint(), "create", None)
            .is_err()
    );
    for table in [
        "bots",
        "chats",
        "bot_chats",
        "bot_selected_pages",
        "bot_command_receipts",
    ] {
        assert_eq!(
            repo.connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    repo.connection
        .execute_batch("DROP TRIGGER fail_bot_receipt;")
        .unwrap();
    repo.save_bot(&bot, profile, BotRequestId::mint(), "create", None)
        .unwrap();
    let old = bot.clone();
    repo.connection.execute_batch("CREATE TRIGGER fail_bot_receipt BEFORE INSERT ON bot_command_receipts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    bot.config.name = "changed".into();
    bot.revision = 2;
    bot.selected_pages.push(BotPage {
        workspace: bot.home_workspace,
        document: DocumentId::new(),
    });
    assert!(
        repo.save_bot(&bot, profile, BotRequestId::mint(), "update", Some(1))
            .is_err()
    );
    assert_eq!(repo.bot(bot.id).unwrap().unwrap(), old);
    let access = ChatReadAccess::authorized(vec![bot.main_chat]);
    assert_eq!(
        repo.chat_with_access(bot.main_chat, &access)
            .unwrap()
            .unwrap()
            .title,
        old.config.name
    );
}

#[test]
fn changing_role_reference_never_transfers_owned_proof() {
    let mut repo = Repository::in_memory().unwrap();
    let mut bot = fixture_bot();
    let profile = ProfileId::new();
    let receipt = BotRequestId::mint();
    repo.save_bot(&bot, profile, receipt, "create", None)
        .unwrap();
    // Future verified owned creation is represented here; P1.2 exposes no caller-owned flag.
    repo.connection
        .execute(
            "UPDATE bots SET role_owned=1,role_creation_request_id=?2 WHERE bot_id=?1",
            params![bot.id.to_storage(), receipt.to_storage()],
        )
        .unwrap();
    bot.config.role.document = DocumentId::new();
    bot.revision = 2;
    repo.save_bot(&bot, profile, BotRequestId::mint(), "change-role", Some(1))
        .unwrap();
    let proof: (bool, Option<String>) = repo
        .connection
        .query_row(
            "SELECT role_owned,role_creation_request_id FROM bots WHERE bot_id=?1",
            [bot.id.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(proof, (false, None));
    assert!(
        repo.bot_receipt(bot.user_scope, receipt, "create")
            .unwrap()
            .is_some()
    );
}
