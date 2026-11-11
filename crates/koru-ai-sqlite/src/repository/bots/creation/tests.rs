// SPDX-License-Identifier: MPL-2.0
use super::super::tests::fixture_bot;
use super::*;

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn root() -> &'static str {
    if cfg!(windows) {
        "D:/koru/ai"
    } else {
        "/koru/ai"
    }
}

fn record(authored: bool) -> BotCreationRecord {
    let bot = BotId::mint();
    let home = WorkspaceId::new();
    let path = koru_ai_core::bots::managed_folder(std::path::Path::new(root()), bot)
        .to_str()
        .unwrap()
        .to_owned();
    BotCreationRecord {
        bot,
        main_chat: ChatId::new(),
        user_scope: UserScopeId::new(),
        owner_device: DeviceId::from_uuid(Uuid::new_v4()),
        home_workspace: home,
        profile: ProfileId::new(),
        config: BotCreationConfig {
            name: "Researcher".into(),
            model: ModelSelection {
                provider: AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: Some("plan".into()),
            fast: Some(true),
        },
        access: BotAccess::Default,
        selected_pages: vec![BotPage {
            workspace: home,
            document: DocumentId::new(),
        }],
        role: if authored {
            BotCreationRole::Authored {
                store_request: Uuid::new_v4(),
                candidate: DocumentId::new(),
                title: "Role".into(),
                seed: Some(BotSeedBytes(vec![9; 32])),
                seed_hash: HASH.into(),
                request_hash: HASH.into(),
            }
        } else {
            BotCreationRole::Existing {
                page: BotPage {
                    workspace: home,
                    document: DocumentId::new(),
                },
            }
        },
        folder: BotCreationFolder::Managed {
            root: root().into(),
            path,
        },
        phase: BotCreationPhase::Intent,
        reason: None,
        folder_binding: None,
        role_page: None,
        created_at_ms: 5,
    }
}

fn folder_ready(record: &BotCreationRecord) -> BotCreationRecord {
    let mut next = record.clone();
    next.phase = BotCreationPhase::FolderReady;
    next.folder_binding = Some(root().into());
    next
}

fn verified(record: &BotCreationRecord) -> BotCreationRecord {
    let mut next = folder_ready(record);
    next.phase = BotCreationPhase::RoleVerified;
    next.role_page = Some(record.role_target());
    next
}

fn receipts(repo: &Repository) -> i64 {
    repo.connection
        .query_row("SELECT COUNT(*) FROM bot_command_receipts", [], |r| {
            r.get(0)
        })
        .unwrap()
}

fn counts(repo: &Repository, bot: BotId) -> (i64, i64, i64) {
    repo.connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bots WHERE bot_id=?1),
                    (SELECT COUNT(*) FROM bot_chats WHERE bot_id=?1),
                    (SELECT COUNT(*) FROM chats)",
            [bot.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

fn tombstone(repo: &Repository, scope: UserScopeId, bot: BotId) {
    let operation = BotRequestId::mint().to_storage();
    repo.connection.execute(
        "INSERT INTO bot_lifecycle_intents(user_scope_id,operation_id,bot_id,owner_device_id,kind,fingerprint,expected_revision,prior_lifecycle,stage,requested_at_ms,updated_at_ms,completed_at_ms) VALUES(?1,?2,?3,?4,'trash',?5,1,'archived','completed',1,1,1)",
        params![scope.to_storage(), operation, bot.to_storage(), Uuid::new_v4().to_string(), HASH],
    ).unwrap();
    repo.connection.execute(
        "INSERT INTO bot_purge_tombstones(user_scope_id,subject_kind,subject_id,bot_id,owner_device_id,operation_id,chat_purpose,purged_at_ms) VALUES(?1,'bot',?2,?2,?3,?4,NULL,2)",
        params![scope.to_storage(), bot.to_storage(), Uuid::new_v4().to_string(), operation],
    ).unwrap();
}

#[test]
fn the_intent_is_recorded_alone_and_an_exact_repeat_returns_it() {
    let mut repo = Repository::in_memory().unwrap();
    let intent = record(true);
    let request = BotRequestId::mint();
    let first = repo.begin_bot_creation(request, &intent, HASH, 10).unwrap();
    assert_eq!(first.stage, BotReceiptStage::Prepared);
    assert_eq!(first.record, intent);
    // No Bot, no Chat, no association: only the receipt.
    assert_eq!(counts(&repo, intent.bot), (0, 0, 0));
    assert_eq!(receipts(&repo), 1);
    assert_eq!(
        repo.begin_bot_creation(request, &intent, HASH, 11).unwrap(),
        first
    );
    // Changed input under the same request conflicts and writes nothing.
    let mut changed = intent.clone();
    changed.config.name = "Other".into();
    let other = HASH.replace('0', "1");
    assert_eq!(
        repo.begin_bot_creation(request, &changed, &other, 12),
        Err(BotError::Conflict)
    );
    assert_eq!(
        repo.bot_creation(intent.user_scope, request).unwrap(),
        Some(first)
    );
    assert_eq!(receipts(&repo), 1);
}

#[test]
fn one_bot_id_is_reserved_against_new_legacy_live_and_purged_identities() {
    let mut repo = Repository::in_memory().unwrap();
    let pending = record(true);
    repo.begin_bot_creation(BotRequestId::mint(), &pending, HASH, 1)
        .unwrap();
    // Another new request for the reserved BotId.
    let mut rival = record(false);
    rival.bot = pending.bot;
    rival.user_scope = pending.user_scope;
    rival.folder = pending.folder.clone();
    assert_eq!(
        repo.begin_bot_creation(BotRequestId::mint(), &rival, HASH, 2),
        Err(BotError::Conflict)
    );
    // A legacy Create for the pending identity.
    let mut legacy = fixture_bot();
    legacy.id = pending.bot;
    legacy.user_scope = pending.user_scope;
    assert_eq!(
        repo.save_bot(
            &legacy,
            ProfileId::new(),
            BotRequestId::mint(),
            "legacy",
            None
        ),
        Err(BotError::Conflict)
    );
    // After a restart the reservation is the same durable row.
    let reopened_rival = repo.begin_bot_creation(BotRequestId::mint(), &rival, HASH, 3);
    assert_eq!(reopened_rival, Err(BotError::Conflict));
    // A live Bot created by legacy Create reserves its BotId for creation.
    let live = fixture_bot();
    repo.save_bot(
        &live,
        ProfileId::new(),
        BotRequestId::mint(),
        "legacy",
        None,
    )
    .unwrap();
    let mut onto_live = record(true);
    onto_live.bot = live.id;
    onto_live.user_scope = live.user_scope;
    onto_live.folder = BotCreationFolder::Managed {
        root: root().into(),
        path: koru_ai_core::bots::managed_folder(std::path::Path::new(root()), live.id)
            .to_str()
            .unwrap()
            .into(),
    };
    assert_eq!(
        repo.begin_bot_creation(BotRequestId::mint(), &onto_live, HASH, 4),
        Err(BotError::Conflict)
    );
    // A purged BotId is never reusable, by either Create.
    let mut purged = record(true);
    tombstone(&repo, purged.user_scope, purged.bot);
    assert_eq!(
        repo.begin_bot_creation(BotRequestId::mint(), &purged, HASH, 5),
        Err(BotError::Conflict)
    );
    let mut legacy_purged = fixture_bot();
    legacy_purged.id = purged.bot;
    legacy_purged.user_scope = purged.user_scope;
    assert_eq!(
        repo.save_bot(
            &legacy_purged,
            ProfileId::new(),
            BotRequestId::mint(),
            "x",
            None
        ),
        Err(BotError::Conflict)
    );
    // A rejected creation frees its BotId.
    purged = record(true);
    let request = BotRequestId::mint();
    repo.begin_bot_creation(request, &purged, HASH, 6).unwrap();
    let mut occupied = purged.clone();
    occupied.reason = Some(BotCreationReason::FolderOccupied);
    let rejected = repo
        .advance_bot_creation(
            purged.user_scope,
            request,
            &purged,
            &occupied,
            BotReceiptStage::Rejected,
            7,
        )
        .unwrap();
    assert_eq!(rejected.stage, BotReceiptStage::Rejected);
    assert!(matches!(
        rejected.record.role,
        BotCreationRole::Authored { seed: None, .. }
    ));
    let mut again = fixture_bot();
    again.id = purged.bot;
    again.user_scope = purged.user_scope;
    repo.save_bot(&again, ProfileId::new(), BotRequestId::mint(), "y", None)
        .unwrap();
}

#[test]
fn steps_move_forward_from_exactly_the_stored_record() {
    let mut repo = Repository::in_memory().unwrap();
    let intent = record(true);
    let scope = intent.user_scope;
    let request = BotRequestId::mint();
    repo.begin_bot_creation(request, &intent, HASH, 1).unwrap();
    let ready = folder_ready(&intent);
    let at_ready = repo
        .advance_bot_creation(
            scope,
            request,
            &intent,
            &ready,
            BotReceiptStage::Prepared,
            2,
        )
        .unwrap();
    assert_eq!(at_ready.record, ready);
    // A stale step (from the intent again) writes nothing.
    assert_eq!(
        repo.advance_bot_creation(
            scope,
            request,
            &intent,
            &ready,
            BotReceiptStage::Prepared,
            3
        ),
        Err(BotError::Conflict)
    );
    // Backwards, or a changed immutable field, refuses.
    assert_eq!(
        repo.advance_bot_creation(
            scope,
            request,
            &ready,
            &intent,
            BotReceiptStage::Prepared,
            3
        ),
        Err(BotError::Invalid)
    );
    let mut renamed = verified(&intent);
    renamed.config.name = "Other".into();
    assert_eq!(
        repo.advance_bot_creation(
            scope,
            request,
            &ready,
            &renamed,
            BotReceiptStage::Accepted,
            3
        ),
        Err(BotError::Invalid)
    );
    // A verified Role is only ever `accepted`, never still `prepared`.
    assert_eq!(
        repo.advance_bot_creation(
            scope,
            request,
            &ready,
            &verified(&intent),
            BotReceiptStage::Prepared,
            3
        ),
        Err(BotError::Invalid)
    );
    let accepted = repo
        .advance_bot_creation(
            scope,
            request,
            &ready,
            &verified(&intent),
            BotReceiptStage::Accepted,
            4,
        )
        .unwrap();
    assert_eq!(accepted.stage, BotReceiptStage::Accepted);
    // Parking keeps the phase and records the reason.
    let mut parked = accepted.record.clone();
    parked.reason = Some(BotCreationReason::OwnerUnavailable);
    let parked = repo
        .advance_bot_creation(
            scope,
            request,
            &accepted.record,
            &parked,
            BotReceiptStage::Accepted,
            5,
        )
        .unwrap();
    assert_eq!(parked.view(None).stage, BotCreationStage::Incomplete);
    assert_eq!(parked.view(None).role_page, Some(intent.role_target()));
}

#[test]
fn the_final_transaction_writes_one_bot_one_main_chat_and_owns_the_role() {
    let mut repo = Repository::in_memory().unwrap();
    let intent = record(true);
    let scope = intent.user_scope;
    let request = BotRequestId::mint();
    repo.begin_bot_creation(request, &intent, HASH, 1).unwrap();
    let ready = folder_ready(&intent);
    repo.advance_bot_creation(
        scope,
        request,
        &intent,
        &ready,
        BotReceiptStage::Prepared,
        2,
    )
    .unwrap();
    // Not yet verified: the final transaction refuses.
    assert_eq!(
        repo.complete_bot_creation(scope, request, &ready, ProfileId::new(), 3)
            .map(|_| ()),
        Err(BotError::Conflict)
    );
    let done = verified(&intent);
    repo.advance_bot_creation(scope, request, &ready, &done, BotReceiptStage::Accepted, 3)
        .unwrap();
    let profile = ProfileId::new();
    let (receipt, bot) = repo
        .complete_bot_creation(scope, request, &done, profile, 9)
        .unwrap();
    assert_eq!(receipt.stage, BotReceiptStage::Completed);
    assert!(matches!(
        receipt.record.role,
        BotCreationRole::Authored { seed: None, ref seed_hash, .. } if seed_hash == HASH
    ));
    assert_eq!(receipt.record.folder_binding, done.folder_binding);
    assert_eq!(counts(&repo, intent.bot), (1, 1, 1));
    assert_eq!(repo.bot(intent.bot).unwrap(), Some(bot.clone()));
    assert_eq!(bot.config.role, intent.role_target());
    assert_eq!(bot.selected_pages, intent.selected_pages);
    let (owned, by): (bool, Option<String>) = repo
        .connection
        .query_row(
            "SELECT role_owned,role_creation_request_id FROM bots WHERE bot_id=?1",
            [intent.bot.to_storage()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(owned);
    assert_eq!(by, Some(request.to_storage()));
    let chat = repo
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    assert_eq!(chat.profile_id, Some(profile));
    assert_eq!(chat.agent_working_folder, done.folder_binding);
    assert_eq!(
        (chat.agent_mode.as_deref(), chat.agent_fast),
        (Some("plan"), Some(true))
    );
    // Completed: a repeat of the step refuses and changes nothing.
    assert_eq!(
        repo.complete_bot_creation(scope, request, &done, profile, 10)
            .map(|_| ()),
        Err(BotError::Conflict)
    );
    assert_eq!(counts(&repo, intent.bot), (1, 1, 1));
}

#[test]
fn an_existing_role_completes_from_folder_ready_and_stays_shared() {
    let mut repo = Repository::in_memory().unwrap();
    let intent = record(false);
    let scope = intent.user_scope;
    let request = BotRequestId::mint();
    repo.begin_bot_creation(request, &intent, HASH, 1).unwrap();
    let ready = folder_ready(&intent);
    repo.advance_bot_creation(
        scope,
        request,
        &intent,
        &ready,
        BotReceiptStage::Prepared,
        2,
    )
    .unwrap();
    let (_, bot) = repo
        .complete_bot_creation(scope, request, &ready, ProfileId::new(), 3)
        .unwrap();
    let owned: bool = repo
        .connection
        .query_row(
            "SELECT role_owned FROM bots WHERE bot_id=?1",
            [bot.id.to_storage()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!owned);
}

#[test]
fn a_late_final_failure_rolls_everything_back_and_the_step_repeats_alone() {
    let mut repo = Repository::in_memory().unwrap();
    let intent = record(true);
    let scope = intent.user_scope;
    let request = BotRequestId::mint();
    repo.begin_bot_creation(request, &intent, HASH, 1).unwrap();
    let ready = folder_ready(&intent);
    repo.advance_bot_creation(
        scope,
        request,
        &intent,
        &ready,
        BotReceiptStage::Prepared,
        2,
    )
    .unwrap();
    let done = verified(&intent);
    let accepted = repo
        .advance_bot_creation(scope, request, &ready, &done, BotReceiptStage::Accepted, 3)
        .unwrap();
    // The last write of the transaction fails, after the Bot and Chat rows.
    repo.fixture_execute_batch(
        "CREATE TRIGGER fail_selected BEFORE INSERT ON bot_selected_pages BEGIN SELECT RAISE(ABORT,'injected'); END;",
    )
    .unwrap();
    assert_eq!(
        repo.complete_bot_creation(scope, request, &done, ProfileId::new(), 4)
            .map(|_| ()),
        Err(BotError::Storage)
    );
    assert_eq!(counts(&repo, intent.bot), (0, 0, 0));
    assert_eq!(repo.bot_creation(scope, request).unwrap(), Some(accepted));
    repo.fixture_execute_batch("DROP TRIGGER fail_selected;")
        .unwrap();
    repo.complete_bot_creation(scope, request, &done, ProfileId::new(), 5)
        .unwrap();
    assert_eq!(counts(&repo, intent.bot), (1, 1, 1));
}

#[test]
fn unknown_versions_and_other_kinds_fail_closed() {
    let mut repo = Repository::in_memory().unwrap();
    let intent = record(true);
    let request = BotRequestId::mint();
    repo.begin_bot_creation(request, &intent, HASH, 1).unwrap();
    repo.connection
        .execute(
            "UPDATE bot_command_receipts SET outcome_json=json_set(outcome_json,'$.version',3) WHERE request_id=?1",
            [request.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.bot_creation(intent.user_scope, request),
        Err(BotError::Storage)
    );
    // A legacy Create's receipt is not read as a creation.
    let legacy = fixture_bot();
    let legacy_request = BotRequestId::mint();
    repo.save_bot(&legacy, ProfileId::new(), legacy_request, "legacy", None)
        .unwrap();
    assert_eq!(
        repo.bot_creation(legacy.user_scope, legacy_request),
        Err(BotError::Conflict)
    );
    // Nor a receipt of another command kind.
    let chat = BotRequestId::mint();
    repo.create_bot_chat(
        &legacy,
        ProfileId::new(),
        chat,
        HASH,
        Timestamp::from_millis(3),
    )
    .unwrap();
    assert_eq!(
        repo.bot_creation(legacy.user_scope, chat),
        Err(BotError::Conflict)
    );
}

#[test]
fn the_startup_scan_finds_unfinished_creations_in_bounded_steps() {
    let mut repo = Repository::in_memory().unwrap();
    // Settled history the scan must step over without reading outcomes.
    let tx = repo.connection.transaction().unwrap();
    for n in 0..500 {
        let kind = ["update", "create_chat", "update_access", "create"][n % 4];
        tx.execute(
            "INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,'completed',?6,?6)",
            params![UserScopeId::new().to_storage(), BotRequestId::mint().to_storage(), BotId::mint().to_storage(), HASH, kind, n as i64],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let mut open = std::collections::BTreeSet::new();
    for n in 0..3 {
        let intent = record(n != 1);
        let request = BotRequestId::mint();
        repo.begin_bot_creation(request, &intent, HASH, n).unwrap();
        open.insert((intent.user_scope, request));
    }
    // A completed and a rejected one are not unfinished.
    let rejected = record(true);
    let rejected_request = BotRequestId::mint();
    repo.begin_bot_creation(rejected_request, &rejected, HASH, 9)
        .unwrap();
    let mut occupied = rejected.clone();
    occupied.reason = Some(BotCreationReason::FolderOccupied);
    repo.advance_bot_creation(
        rejected.user_scope,
        rejected_request,
        &rejected,
        &occupied,
        BotReceiptStage::Rejected,
        10,
    )
    .unwrap();
    let mut found = std::collections::BTreeSet::new();
    let mut after = String::new();
    let mut steps = 0;
    loop {
        let step = repo.bot_creation_scan(&after, 16).unwrap();
        assert!(step.skipped.is_empty());
        found.extend(step.found);
        steps += 1;
        match step.next {
            Some(next) => after = next,
            None => break,
        }
    }
    assert_eq!(found, open);
    // 504 distinct BotIds, at most 16 per step.
    assert!(steps >= 504 / 16, "{steps}");
}

fn plan(conn: &Connection, sql: &str) -> String {
    let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let count = statement.parameter_count();
    let args: Vec<String> = (0..count).map(|_| String::new()).collect();
    statement
        .query_map(rusqlite::params_from_iter(args.iter()), |r| {
            r.get::<_, String>(3)
        })
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>()
        .join(" | ")
}

#[test]
fn every_creation_lookup_reads_the_guard_index_and_never_the_table() {
    let repo = Repository::in_memory().unwrap();
    let c = &repo.connection;
    let next = plan(c, CREATION_NEXT_BOT_SQL);
    assert!(
        next.contains(
            "SEARCH bot_command_receipts USING COVERING INDEX bot_command_guard (bot_id>?)"
        ),
        "{next}"
    );
    let open = plan(c, CREATION_OPEN_SQL);
    assert!(
        open.contains("USING INDEX bot_command_guard (bot_id=? AND command_kind=? AND stage=?)"),
        "{open}"
    );
    let reserved = plan(c, CREATION_RESERVED_SQL);
    assert!(
        reserved.contains(
            "USING COVERING INDEX bot_command_guard (bot_id=? AND command_kind=? AND stage=?)"
        ),
        "{reserved}"
    );
    for sql in [
        CREATION_NEXT_BOT_SQL,
        CREATION_OPEN_SQL,
        CREATION_RESERVED_SQL,
    ] {
        let shown = plan(c, sql);
        assert!(!shown.contains("SCAN bot_command_receipts"), "{shown}");
    }
}

#[test]
fn a_row_the_scan_cannot_parse_is_skipped_and_named_and_the_scan_goes_on() {
    let mut repo = Repository::in_memory().unwrap();
    let good = record(false);
    let request = BotRequestId::mint();
    repo.begin_bot_creation(request, &good, HASH, 1).unwrap();
    repo.fixture_execute_batch(
        "INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,outcome_json,created_at_ms,updated_at_ms)
         VALUES('damaged','not-a-request','00000000-0000-4000-8000-000000000000','x','create','prepared','{}',1,1)",
    )
    .unwrap();
    let step = repo.bot_creation_scan("", 16).unwrap();
    assert_eq!(step.found, vec![(good.user_scope, request)]);
    assert_eq!(
        step.skipped,
        vec!["00000000-0000-4000-8000-000000000000".to_owned()]
    );
}
