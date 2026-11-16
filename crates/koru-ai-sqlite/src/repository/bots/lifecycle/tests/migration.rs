// SPDX-License-Identifier: MPL-2.0
//! ai17 → ai18 over frozen ai17 admission SQL and unchanged historical helpers.
use super::*;
use crate::repository::tests::complete_turn;
use koru_ai_core::bots::BotSubmissionIntent;
use koru_ai_core::{AttachmentRecord, Timestamp};
use koru_delegate_core as d;
use rusqlite::types::Value;
use std::collections::BTreeMap;
mod ai17;

const AI18_TABLES: [&str; 5] = [
    "bot_lifecycle_intents",
    "bot_lifecycle_members",
    "bot_chat_trash",
    "bot_purge_tombstones",
    "bot_historical_run_refs",
];

fn intent(
    bot: &Bot,
    chat: ChatId,
    turn: Option<&koru_ai_core::PendingTurn>,
) -> BotSubmissionIntent {
    let workspace = bot.home_workspace;
    BotSubmissionIntent {
        version: 1,
        submission: d::BotSubmission {
            provenance: d::BotProvenance {
                parent: None,
                routine: None,
                owner: d::BotOwner {
                    user_scope: bot.user_scope,
                    device: bot.owner_device,
                },
                bot: bot.id,
                submission: BotRequestId::mint(),
                chat_id: chat.as_uuid(),
                ai_attempt_id: turn.map(|t| t.attempt.id.as_uuid()),
                assistant_message_id: turn.map(|t| t.assistant_message.id.as_uuid()),
                config_revision: bot.revision,
                policy_revision: bot.policy_revision,
                continued_from: None,
            },
            policy: d::CapturedBotPolicy {
                workspace,
                destination: workspace,
                access: d::BotAccess::Default,
                selected_pages: Default::default(),
                connections: Default::default(),
            },
            request: d::DelegationRequest {
                run: d::RunId::mint(),
                requested_by: d::ActorRef::Person {
                    profile: ProfileId::new(),
                },
                objective: d::Objective::new("Fixture task").unwrap(),
                agent: d::SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                requested_model: None,
                scope: d::WorkspaceScope::BotFolder {
                    folder: d::BotWorkingFolder::new(bot.config.working_folder.clone()).unwrap(),
                },
                context: vec![],
                grants: [workspace].into_iter().collect(),
                executing_device: bot.owner_device,
                origin: d::RunOrigin::Manual,
            },
            workspace,
            input: d::PendingInput {
                idempotency_key: d::IdempotencyKey::mint(),
                text: "Fixture task".into(),
                attachment_ids: vec![],
                references: vec![],
                mode: None,
                fast: None,
                reasoning: None,
                skills: vec![],
                model: None,
            },
        },
    }
}

fn new_bot(repo: &mut Repository) -> (Bot, BotOwner, ProfileId) {
    let bot = fixture_bot();
    let profile = ProfileId::new();
    repo.save_bot(&bot, profile, BotRequestId::mint(), "create", None)
        .unwrap();
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    (bot, owner, profile)
}

fn view(intent: &BotSubmissionIntent, state: d::BotAdmissionState) -> d::BotAdmissionView {
    let p = &intent.submission.provenance;
    d::BotAdmissionView {
        key: d::BotSubmissionKey {
            owner: p.owner,
            bot: p.bot,
            request: p.submission,
        },
        run: intent.submission.request.run,
        state,
        queue_sequence: 1,
        fence: None,
    }
}

fn staged(chat: ChatId) -> AttachmentRecord {
    AttachmentRecord {
        chat_id: chat,
        attachment: koru_ai_core::ChatAttachment {
            id: koru_ai_core::ChatAttachmentId::new(),
            message_id: None,
            ordinal: None,
            kind: koru_ai_core::AttachmentKind::Image,
            source: koru_ai_core::AttachmentSource::Clipboard,
            file_name: "kept.png".into(),
            media_type: "image/png".into(),
            byte_count: 3,
            dimensions: Some((1, 1)),
            origin: None,
            reference_path: None,
            created_at: Timestamp::from_millis(3),
        },
        has_sent_copy: false,
        content_blake3: "c".repeat(64),
    }
}

/// An ai17 database holding every kind of Bot row the ai17 code writes.
fn populated_ai17() -> Repository {
    let mut repo = Repository::at_schema(17).unwrap();
    assert_eq!(repo.schema_version().unwrap(), 17);
    // Today's code names `bots.avatar` (ai24): borrowed while the rows are
    // written, returned below before anything is compared or migrated.
    let pristine = bots_definition(&repo.connection);
    repo.lend_avatar_column();
    // Bot A: main, additional and branched conversations, a bound Run with
    // its replay row, a sent attachment and a prepared Start receipt.
    let (a, _, a_profile) = new_bot(&mut repo);
    complete_turn(&mut repo, a.main_chat, 0, "kept main reply", 10);
    let additional = repo
        .create_bot_chat(
            &a,
            a_profile,
            BotRequestId::mint(),
            HASH,
            Timestamp::from_millis(20),
        )
        .unwrap();
    let source_reply = repo.messages(a.main_chat).unwrap()[1].id;
    let branch = super::super::super::tests::branch::copy(&repo, &a, a.main_chat, source_reply);
    repo.create_bot_branch(&a, &branch).unwrap();
    let attachment = staged(additional.chat_id);
    repo.stage_attachment(&attachment).unwrap();
    let mut turn = crate::repository::tests::turn(additional.chat_id, uuid::Uuid::new_v4(), 30);
    turn.attachment_ids = vec![attachment.attachment.id];
    turn.user_message.markdown = "Fixture task".into();
    let started = intent(&a, additional.chat_id, Some(&turn));
    ai17::prepare_chat(&mut repo, &turn, &started, 30);
    let fence = d::BotExecutionFence {
        owner: started.submission.provenance.owner,
        bot: a.id,
        run: started.submission.request.run,
        attempt: d::RunAttemptId::mint(),
        generation: d::AttemptGeneration::FIRST,
        session: d::SessionId::mint(),
        turn: Some(d::AgentTurnId::mint()),
    };
    repo.bind_bot_execution(turn.attempt.id, &fence, Timestamp::from_millis(31))
        .unwrap();
    // Bot B: a Start refused and rejected after its fence.
    let (b, b_owner, _) = new_bot(&mut repo);
    let refused = intent(&b, b.main_chat, None);
    let request = refused.submission.provenance.submission;
    ai17::prepare_submission(&mut repo, &refused, 40);
    let fenced = repo
        .prepare_bot_refusal_fence(
            b_owner,
            b.id,
            request,
            koru_ai_core::bots::BotAdmissionRefusal::Capacity,
            false,
            41,
        )
        .unwrap();
    let Some(koru_ai_core::bots::BotExecutionOutcome::RefusalFence { round, barrier, .. }) =
        fenced.outcome
    else {
        panic!("refusal fence")
    };
    repo.acknowledge_bot_refusal_fence(b_owner, b.id, request, round, &barrier, 42)
        .unwrap();
    repo.reject_bot_submission_after_fence(b_owner, b.id, request, round, &barrier, 43)
        .unwrap();
    // Bot C: an accepted (completed) Start, a completed and a prepared pause.
    let (c, c_owner, _) = new_bot(&mut repo);
    let accepted = intent(&c, c.main_chat, None);
    let request = accepted.submission.provenance.submission;
    ai17::prepare_submission(&mut repo, &accepted, 50);
    repo.acknowledge_bot_submission(
        c_owner,
        c.id,
        request,
        &view(&accepted, d::BotAdmissionState::Queued),
        51,
    )
    .unwrap();
    let pause = repo
        .prepare_bot_pause(
            c_owner,
            c.id,
            BotRequestId::mint(),
            c.revision,
            true,
            HASH,
            52,
        )
        .unwrap();
    repo.acknowledge_bot_pause(
        c_owner,
        c.id,
        pause.intent.barrier.request,
        &pause.intent.barrier,
        53,
    )
    .unwrap();
    repo.prepare_bot_pause(
        c_owner,
        c.id,
        BotRequestId::mint(),
        c.revision + 1,
        false,
        HASH,
        54,
    )
    .unwrap();
    // Bot D: a prepared access change.
    let (mut dbot, _, _) = new_bot(&mut repo);
    dbot.revision += 1;
    dbot.policy_revision += 1;
    dbot.access = BotAccess::Workspace;
    let barrier = d::BotPolicyBarrier {
        key: d::BotPolicyBarrierKey {
            owner: d::BotOwner {
                user_scope: dbot.user_scope,
                device: dbot.owner_device,
            },
            bot: dbot.id,
            request: d::NativePolicyRequestId::from(BotRequestId::mint()),
        },
        config_revision: dbot.revision,
        policy_revision: dbot.policy_revision,
        policy: koru_ai_core::bots::bot_native_policy(&dbot),
        effects_allowed: true,
    };
    ai17::prepare_access(&mut repo, &dbot, &barrier);
    // Every receipt stage the ai17 code writes is present ('accepted' has no writer).
    let stages: BTreeSet<String> = repo
        .connection
        .prepare("SELECT DISTINCT stage FROM bot_command_receipts")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        stages,
        ["completed", "prepared", "rejected"]
            .map(String::from)
            .into_iter()
            .collect()
    );
    repo.return_avatar_column();
    assert_eq!(bots_definition(&repo.connection), pristine);
    repo
}

/// The stored definition of `bots`, without whitespace.
fn bots_definition(conn: &Connection) -> String {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='bots'",
        [],
        |r| r.get::<_, String>(0),
    )
    .unwrap()
    .chars()
    .filter(|c| !c.is_whitespace())
    .collect()
}

type Snapshot = BTreeMap<String, (Option<String>, Vec<String>)>;

/// Every schema object's definition and every table row, value for value.
fn snapshot(conn: &Connection) -> Snapshot {
    let objects: Vec<(String, String, Option<String>)> = conn
        .prepare(
            "SELECT type,name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = Snapshot::new();
    for (kind, name, sql) in objects {
        let mut rows = Vec::new();
        if kind == "table" {
            let mut query = conn.prepare(&format!("SELECT * FROM \"{name}\"")).unwrap();
            let width = query.column_count();
            let mut cursor = query.query([]).unwrap();
            while let Some(row) = cursor.next().unwrap() {
                let values: Vec<Value> = (0..width).map(|i| row.get(i).unwrap()).collect();
                rows.push(format!("{values:?}"));
            }
            rows.sort();
        }
        out.insert(format!("{kind}:{name}"), (sql, rows));
    }
    out
}

#[test]
fn populated_ai17_survives_18_value_for_value_and_repeated_preparation() {
    let mut repo = populated_ai17();
    let before = snapshot(&repo.connection);
    assert!(before.values().map(|(_, rows)| rows.len()).sum::<usize>() > 40);
    for _ in 0..2 {
        // This fixture proves the historical ai18 step independently of later
        // migrations; ai19 has its own populated parent-rebuild fixtures.
        koru_sqlite::prepare(
            &koru_sqlite::Schema {
                database: crate::DATABASE_FILE,
                understood: 18,
                migrations: &crate::db::MIGRATIONS[..18],
            },
            &mut repo.connection,
        )
        .unwrap();
        assert_eq!(repo.schema_version().unwrap(), 18);
        let after = snapshot(&repo.connection);
        for (name, kept) in &before {
            assert_eq!(after.get(name), Some(kept), "{name} changed");
        }
        for table in AI18_TABLES {
            let (_, rows) = &after[&format!("table:{table}")];
            assert!(rows.is_empty(), "{table} needs no backfill");
        }
        let added: Vec<_> = after.keys().filter(|k| !before.contains_key(*k)).collect();
        // Fifteen indexes, then the holder and Trash-owner indexes of the
        // amendment made before any release.
        assert_eq!(added.len(), AI18_TABLES.len() + 17, "{added:?}");
        for index in ["index:bot_lifecycle_held", "index:bot_chat_trash_owner"] {
            assert!(after.contains_key(index), "{index}");
        }
    }
    // The upgraded database still reads through the current code.
    let bots = repo
        .connection
        .query_row("SELECT count(*) FROM bots", [], |r| r.get::<_, i64>(0))
        .unwrap();
    assert_eq!(bots, 4);
}

#[test]
fn a_failed_18_leaves_the_ai17_database_exactly_as_it_was() {
    let mut repo = populated_ai17();
    // An object in the way makes the step fail midway through its batch.
    repo.connection
        .execute_batch("CREATE TABLE bot_chat_trash(occupied TEXT);")
        .unwrap();
    let before = snapshot(&repo.connection);
    assert!(crate::db::prepare(&mut repo.connection).is_err());
    assert_eq!(repo.schema_version().unwrap(), 17);
    assert_eq!(snapshot(&repo.connection), before);
}

/// ai18 was amended before any release (the holder and Trash-owner indexes,
/// the `reversed` stage). A development database an unreleased build left
/// at the unamended 18 has the same stamp and the older shape, and would
/// fail a CHECK the first time a Restore marks its source: it is refused at
/// open, by name, before anything in it is read or written.
#[test]
fn a_database_left_at_the_unamended_18_is_refused_by_name_and_left_untouched() {
    let mut repo = populated_ai17();
    repo.connection
        .execute_batch(include_str!(
            "../../../../fixtures/ai18-before-amendment.sql"
        ))
        .unwrap();
    repo.connection
        .execute_batch("PRAGMA user_version = 18;")
        .unwrap();
    let before = snapshot(&repo.connection);
    let refused = crate::db::prepare(&mut repo.connection)
        .expect_err("the unamended ai18 shape must not open");
    let text = refused.to_string();
    assert!(text.contains("superseded development schema"), "{text}");
    assert!(text.contains("recreate"), "{text}");
    assert_eq!(snapshot(&repo.connection), before);
    let OpenError::Storage {
        operation, detail, ..
    } = refused
    else {
        panic!("{refused:?}")
    };
    assert_eq!(operation, crate::SUPERSEDED_DEVELOPMENT_SCHEMA);
    for missing in [
        "bot_lifecycle_held",
        "bot_chat_trash_owner",
        "bot_lifecycle_intents 'reversed'",
    ] {
        assert!(detail.contains(missing), "{detail}");
    }
    // Every later open refuses it the same way, and the amended 18 opens.
    assert!(crate::db::prepare(&mut repo.connection).is_err());
    assert!(Repository::in_memory().is_ok());
}

#[test]
fn a_newer_stamp_is_refused_without_touching_rows() {
    let mut repo = Repository::in_memory().unwrap();
    repo.connection
        .pragma_update(None, "user_version", crate::SCHEMA_VERSION + 1)
        .unwrap();
    let before = snapshot(&repo.connection);
    assert!(matches!(
        crate::db::prepare(&mut repo.connection),
        Err(OpenError::SchemaTooNew {
            discovered,
            supported,
            ..
        }) if discovered == crate::SCHEMA_VERSION + 1 && supported == crate::SCHEMA_VERSION
    ));
    assert_eq!(snapshot(&repo.connection), before);
}
