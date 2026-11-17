// SPDX-License-Identifier: MPL-2.0
//! The owner feed at the repository: the audit, the markers, the pages'
//! subjects, the starts, and who is never sent.
//!
//! In-memory databases, and for the restore cases a copy of one in a
//! temporary file. No worker, Store, delegate or provider.
//!
//! Cohorts `m8_feed_audit`, `m8_feed_pages`, `m8_feed_starts`,
//! `m8_feed_withheld` and `m8_feed_local_owner_only`; the worker's half of
//! the last four is in `apps/koru-ai-server`.
use std::collections::BTreeSet;

use super::super::tests::branch::copy;
use super::super::tests::fixture_bot;
use super::*;
use crate::repository::tests::{complete_turn, turn};
use koru_ai_core::{AgentNoticeId, FeedbackEventId};

fn owner_of(bot: &Bot) -> BotOwner {
    BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}

fn at(offset: i64) -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000 + offset)
}

/// A database with one Bot, created as the product creates it.
fn with_bot() -> (Repository, Bot) {
    let mut repo = Repository::in_memory().unwrap();
    let bot = fixture_bot();
    repo.save_bot(&bot, ProfileId::new(), BotRequestId::mint(), "create", None)
        .unwrap();
    (repo, bot)
}

/// Every marker above `cursor`, with the Bot's home copied.
fn scan(repo: &Repository, bot: &Bot, cursor: u64) -> FeedScan {
    repo.feed_scan(
        owner_of(bot),
        cursor,
        64,
        usize::MAX,
        &BTreeSet::from([bot.home_workspace]),
    )
    .unwrap()
}

fn seq_of(repo: &Repository, kind: &str, subject: &str) -> Option<u64> {
    repo.connection
        .query_row(
            "SELECT seq FROM bot_client_feed WHERE kind = ?1 AND subject_id = ?2",
            [kind, subject],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

fn marker_count(repo: &Repository) -> i64 {
    repo.connection
        .query_row("SELECT count(*) FROM bot_client_feed", [], |row| row.get(0))
        .unwrap()
}

/// The three counters against a count of the markers they stand for.
fn assert_counters(repo: &Repository) -> FeedCounts {
    let counted = |kind: &str| -> u32 {
        repo.connection
            .query_row(
                "SELECT count(*) FROM bot_client_feed WHERE kind = ?1 AND removed = 0 AND withheld = 0",
                [kind],
                |row| row.get(0),
            )
            .unwrap()
    };
    let counts = repo.feed_counts().unwrap();
    assert_eq!(
        counts,
        FeedCounts {
            bots: counted("bot"),
            chats: counted("chat"),
            messages: counted("message"),
        }
    );
    counts
}

fn agent_tool(pending: &PendingTurn) -> ToolActivity {
    ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: pending.attempt.id,
        chat_id: pending.user_message.chat_id,
        assistant_message_id: pending.assistant_message.id,
        tool_name: AgentToolKind::Tool.as_token().into(),
        read_only: false,
        status: ToolActivityStatus::Proposed,
        approval: ToolApproval::NotRequired,
        proposed_at: at(3),
        started_at: None,
        completed_at: None,
        result_bytes: None,
        error_code: None,
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: Some("Run the script".into()),
        agent_summary: Some("May it run?".into()),
        agent_input: Some("secret-arguments --token hunter2".into()),
        agent_output: None,
        text_offset: None,
        display_target: None,
        live: None,
    }
}

fn body_of<'a>(scan: &'a FeedScan, kind: FeedKind, subject: &str) -> &'a FeedBody {
    &scan
        .items
        .iter()
        .find(|item| item.marker.kind == kind && item.marker.subject_id == subject)
        .unwrap_or_else(|| panic!("no {kind:?} entry for {subject}"))
        .body
}

// ---------------------------------------------------------------- the audit

#[test]
fn m8_feed_audit_runs_at_every_commit_and_refuses_a_deliberately_unmarked_write() {
    let (mut repo, bot) = with_bot();
    // Every commit of a write path is looked at, and none of these was let
    // through as a fixture's.
    let (before, exempt) = repo.feed_audit_commits();
    assert!(before >= 1);
    assert_eq!(exempt, 0);
    repo.rename_chat(bot.main_chat, "Renamed").unwrap();
    complete_turn(&mut repo, bot.main_chat, 0, "A reply", 10);
    let (after, exempt) = repo.feed_audit_commits();
    // Rename, begin, one delta, completion.
    assert_eq!(after, before + 4);
    assert_eq!(exempt, 0);
    assert!(repo.feed_audit_refusals().is_empty());

    // A write path that forgot its marker: the commit is refused and rolled
    // back, and the refusal names the subject and the table, nothing else.
    let rename = format!(
        "UPDATE bots SET name = 'Forgotten' WHERE bot_id = '{}'",
        bot.id.to_storage()
    );
    let error = audit::unmarked_write(&repo, &rename).unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("constraint"),
        "{error}"
    );
    assert_eq!(
        repo.feed_audit_refusals(),
        [format!("bot {} (bots)", bot.id.to_storage())]
    );
    assert_eq!(
        repo.bot_any_lifecycle(bot.id)
            .unwrap()
            .unwrap()
            .0
            .config
            .name,
        "Fixture Bot"
    );
    // The same for a conversation's row, a message and a tool call's status.
    let mut second = turn(bot.main_chat, Uuid::new_v4(), 20);
    second.user_message.ordinal = 2;
    second.assistant_message.ordinal = 3;
    repo.begin_turn(&second).unwrap();
    let tool = agent_tool(&second);
    repo.create_tool_activity(&tool).unwrap();
    for (sql, subject) in [
        (
            format!(
                "UPDATE chats SET title = 'Forgotten' WHERE id = '{}'",
                bot.main_chat.to_storage()
            ),
            format!("chat {} (chats)", bot.main_chat.to_storage()),
        ),
        (
            format!(
                "UPDATE messages SET state = 'interrupted', completed_at_ms = created_at_ms WHERE id = '{}'",
                second.assistant_message.id.to_storage()
            ),
            format!("chat {} (messages)", bot.main_chat.to_storage()),
        ),
        (
            format!(
                "UPDATE tool_invocations SET status = 'approval_required', approval = 'pending' WHERE id = '{}'",
                tool.id.to_storage()
            ),
            format!("chat {} (tool_invocations)", bot.main_chat.to_storage()),
        ),
        (
            format!(
                "INSERT INTO feedback_events(id, chat_id, message_id, value, surface, created_at_ms) VALUES('f', '{}', '{}', 'good', 'standalone', 1)",
                bot.main_chat.to_storage(),
                second.user_message.id.to_storage()
            ),
            format!(
                "message {} (feedback_events)",
                second.user_message.id.to_storage()
            ),
        ),
    ] {
        let refused = repo.feed_audit_refusals().len();
        audit::unmarked_write(&repo, &sql).unwrap_err();
        let refusals = repo.feed_audit_refusals();
        assert_eq!(refusals.len(), refused + 1, "{sql}");
        assert!(refusals[refused].contains(&subject), "{refusals:?}");
    }
    // What a record does not show names nobody: a tool call's output, a
    // chat's tool mode, and anything of an ordinary chat.
    let ordinary = ChatId::new();
    repo.create_chat(&crate::repository::tests::chat(ordinary, 0))
        .unwrap();
    for sql in [
        format!(
            "UPDATE tool_invocations SET agent_output = 'progress' WHERE id = '{}'",
            tool.id.to_storage()
        ),
        format!(
            "UPDATE chats SET tool_mode = 'manual' WHERE id = '{}'",
            bot.main_chat.to_storage()
        ),
        format!(
            "UPDATE chats SET title = 'Mine' WHERE id = '{}'",
            ordinary.to_storage()
        ),
    ] {
        audit::unmarked_write(&repo, &sql).unwrap();
    }

    // A fixture's own SQL, written in a test's source file, is exempt, and
    // counted as such; the write path that follows it is held again.
    let (_, exempt) = repo.feed_audit_commits();
    repo.connection.execute_batch(&rename).unwrap();
    assert_eq!(repo.feed_audit_commits().1, exempt + 1);
    let refused = repo.feed_audit_refusals().len();
    audit::unmarked_write(&repo, &rename.replace("Forgotten", "Again")).unwrap_err();
    assert_eq!(repo.feed_audit_refusals().len(), refused + 1);
}

#[test]
fn m8_feed_audit_a_database_opened_from_a_file_reports_an_unmarked_write_and_lets_it_through() {
    let directory = tempfile::tempdir().unwrap();
    let mut repo = Repository::open(&directory.path().join("ai.db")).unwrap();
    let bot = fixture_bot();
    repo.save_bot(&bot, ProfileId::new(), BotRequestId::mint(), "create", None)
        .unwrap();
    let (commits, exempt) = repo.feed_audit_commits();
    assert!(commits >= 1);
    assert_eq!(exempt, 0);
    assert!(repo.feed_audit_refusals().is_empty());
    assert!(seq_of(&repo, "bot", &bot.id.to_storage()).is_some());
    // The same forgotten marker as over the in-memory database: named, and
    // here not refused.
    audit::unmarked_write(
        &repo,
        &format!(
            "UPDATE bots SET name = 'Forgotten' WHERE bot_id = '{}'",
            bot.id.to_storage()
        ),
    )
    .unwrap();
    assert_eq!(
        repo.feed_audit_refusals(),
        [format!("bot {} (bots)", bot.id.to_storage())]
    );
    assert_eq!(
        repo.bot_any_lifecycle(bot.id)
            .unwrap()
            .unwrap()
            .0
            .config
            .name,
        "Forgotten"
    );
    // A database migrated to an older version carries no audit at all.
    let older = Repository::at_schema(27).unwrap();
    assert_eq!(older.feed_audit_commits(), (0, 0));
}

#[test]
fn m8_feed_audit_a_declared_table_without_an_audit_trigger_fails() {
    // Every table a record reads is watched: the shared constructor opens.
    assert!(FEED_RECORD_TABLES.len() >= 19);
    Repository::in_memory().unwrap();
    // A builder that grew a read of a table nothing watches cannot open it.
    let mut connection = crate::db::open_in_memory().unwrap();
    let mut declared = FEED_RECORD_TABLES.to_vec();
    declared.push("bot_routines");
    let error = match audit::install(&mut connection, &declared, true) {
        Ok(_) => panic!("an unwatched table was accepted"),
        Err(error) => error,
    };
    assert!(error.contains("bot_routines"), "{error}");
    // And the declared list is exactly what the triggers watch.
    let mut connection = crate::db::open_in_memory().unwrap();
    audit::install(&mut connection, FEED_RECORD_TABLES, true).unwrap();
    let mut watched: Vec<String> = connection
        .prepare(
            "SELECT DISTINCT tbl_name FROM sqlite_temp_master
              WHERE type = 'trigger' AND name LIKE 'feed_audit_%' AND tbl_name <> 'bot_client_feed'",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    watched.sort();
    let mut declared: Vec<String> = FEED_RECORD_TABLES.iter().map(|t| (*t).to_owned()).collect();
    declared.sort();
    assert_eq!(watched, declared);
}

#[test]
fn m8_feed_audit_a_branch_marks_its_copied_messages() {
    let (mut repo, bot) = with_bot();
    let source = complete_turn(&mut repo, bot.main_chat, 0, "Bot reply", 10);
    let head = repo.feed_head().unwrap();
    let branch = copy(&repo, &bot, bot.main_chat, source.assistant_message.id);
    assert_eq!(branch.messages.len(), 2);
    repo.create_bot_branch(&bot, &branch).unwrap();
    assert!(repo.feed_audit_refusals().is_empty());
    // The branch's messages were inserted before the conversation was a
    // Bot's; each has a marker all the same, above the head before it.
    let chat = seq_of(&repo, "chat", &branch.chat.id.to_storage()).unwrap();
    assert!(chat > head);
    for message in &branch.messages {
        assert!(seq_of(&repo, "message", &message.id.to_storage()).unwrap() > chat);
    }
    let page = scan(&repo, &bot, head);
    assert_eq!(page.items.len(), 3);
    assert!(matches!(
        body_of(&page, FeedKind::Chat, &branch.chat.id.to_storage()),
        FeedBody::Chat(chat) if chat.purpose == BotChatPurpose::Additional && chat.chat.branched_from.is_some()
    ));
    for message in &branch.messages {
        assert!(matches!(
            body_of(&page, FeedKind::Message, &message.id.to_storage()),
            FeedBody::Message(record) if record.message.markdown == message.markdown
        ));
    }
    // A reply the branch copied names the Run it was copied from, as the
    // transcript does; here no Run wrote the source, so it names none.
    assert_counters(&repo);
}

#[test]
fn m8_feed_audit_feedback_on_a_final_message_marks_it() {
    let (mut repo, bot) = with_bot();
    let done = complete_turn(&mut repo, bot.main_chat, 0, "Bot reply", 10);
    let reply = done.assistant_message.id;
    let before = seq_of(&repo, "message", &reply.to_storage()).unwrap();
    let chat_before = seq_of(&repo, "chat", &bot.main_chat.to_storage()).unwrap();
    let event = FeedbackEvent {
        id: FeedbackEventId::new(),
        chat_id: Some(bot.main_chat),
        message_id: Some(reply),
        attempt_id: Some(done.attempt.id),
        value: FeedbackValue::Good,
        surface: AiSurface::Standalone,
        created_at: at(50),
    };
    repo.append_feedback(&event).unwrap();
    let after = seq_of(&repo, "message", &reply.to_storage()).unwrap();
    assert!(after > before);
    // Feedback is the message's: the conversation's record is as it was.
    assert_eq!(
        seq_of(&repo, "chat", &bot.main_chat.to_storage()),
        Some(chat_before)
    );
    let page = scan(&repo, &bot, before);
    let FeedBody::Message(record) = body_of(&page, FeedKind::Message, &reply.to_storage()) else {
        panic!("the reply is not sent");
    };
    assert_eq!(record.feedback.len(), 1);
    assert_eq!(record.feedback[0].value, FeedbackValue::Good);
    // The same event again records nothing and moves nothing.
    repo.append_feedback(&event).unwrap();
    assert_eq!(seq_of(&repo, "message", &reply.to_storage()), Some(after));
    // Cleared feedback is sent as none.
    repo.append_feedback(&FeedbackEvent {
        id: FeedbackEventId::new(),
        value: FeedbackValue::Cleared,
        created_at: at(60),
        ..event
    })
    .unwrap();
    let page = scan(&repo, &bot, after);
    assert!(matches!(
        body_of(&page, FeedKind::Message, &reply.to_storage()),
        FeedBody::Message(record) if record.feedback.is_empty()
    ));
    assert!(repo.feed_audit_refusals().is_empty());
}

#[test]
fn m8_feed_audit_a_tool_call_entering_and_leaving_approval_required_marks_the_chat() {
    let (mut repo, bot) = with_bot();
    let pending = turn(bot.main_chat, Uuid::new_v4(), 1);
    repo.begin_turn(&pending).unwrap();
    let chat = bot.main_chat.to_storage();
    let reply = pending.assistant_message.id.to_storage();
    let needs_you = |repo: &Repository, bot: &Bot| -> bool {
        let page = scan(repo, bot, 0);
        match body_of(&page, FeedKind::Chat, &bot.main_chat.to_storage()) {
            FeedBody::Chat(chat) => {
                assert!(chat.running);
                chat.needs_you
            }
            other => panic!("{other:?}"),
        }
    };

    // The call exists: the reply's record moves, the conversation's does not.
    let chat_seq = seq_of(&repo, "chat", &chat).unwrap();
    let reply_seq = seq_of(&repo, "message", &reply).unwrap();
    let mut tool = agent_tool(&pending);
    repo.create_tool_activity(&tool).unwrap();
    assert!(seq_of(&repo, "message", &reply).unwrap() > reply_seq);
    assert_eq!(seq_of(&repo, "chat", &chat), Some(chat_seq));
    assert!(!needs_you(&repo, &bot));

    // It needs the person: both move.
    let reply_seq = seq_of(&repo, "message", &reply).unwrap();
    tool.status = ToolActivityStatus::ApprovalRequired;
    tool.approval = ToolApproval::Pending;
    repo.advance_agent_activity(&tool).unwrap();
    let asked_chat = seq_of(&repo, "chat", &chat).unwrap();
    assert!(asked_chat > chat_seq);
    assert!(seq_of(&repo, "message", &reply).unwrap() > reply_seq);
    assert!(needs_you(&repo, &bot));

    // The same state written again, with more of its wording: nothing moves.
    let reply_seq = seq_of(&repo, "message", &reply).unwrap();
    tool.agent_summary = Some("May it run now?".into());
    repo.advance_agent_activity(&tool).unwrap();
    assert_eq!(seq_of(&repo, "chat", &chat), Some(asked_chat));
    assert_eq!(seq_of(&repo, "message", &reply), Some(reply_seq));

    // It was answered: both move again, and the conversation needs nobody.
    tool.status = ToolActivityStatus::Running;
    tool.approval = ToolApproval::Approved;
    tool.started_at = Some(at(5));
    repo.advance_agent_activity(&tool).unwrap();
    let answered_chat = seq_of(&repo, "chat", &chat).unwrap();
    assert!(answered_chat > asked_chat);
    assert!(seq_of(&repo, "message", &reply).unwrap() > reply_seq);
    assert!(!needs_you(&repo, &bot));

    // It finished: the reply's record moves, the conversation's does not.
    let reply_seq = seq_of(&repo, "message", &reply).unwrap();
    tool.status = ToolActivityStatus::Complete;
    tool.completed_at = Some(at(6));
    tool.agent_output = Some("the output, with a secret in it".into());
    repo.advance_agent_activity(&tool).unwrap();
    assert!(seq_of(&repo, "message", &reply).unwrap() > reply_seq);
    assert_eq!(seq_of(&repo, "chat", &chat), Some(answered_chat));
    assert!(repo.feed_audit_refusals().is_empty());

    // What is sent of the call holds its label and its question, and never
    // what it was asked to run or what it produced.
    let page = scan(&repo, &bot, 0);
    let FeedBody::Message(record) = body_of(&page, FeedKind::Message, &reply) else {
        panic!("the reply is not sent");
    };
    assert_eq!(record.tool_activities.len(), 1);
    let sent = &record.tool_activities[0];
    assert_eq!(sent.id, tool.id);
    assert_eq!(sent.status, ToolActivityStatus::Complete);
    assert_eq!(sent.agent_label.as_deref(), Some("Run the script"));
    assert_eq!(sent.agent_summary.as_deref(), Some("May it run now?"));
    assert_eq!(sent.agent_input, None);
    assert_eq!(sent.agent_output, None);
    // The row itself still holds both, for the computer that ran it.
    let stored = repo.tool_activity(tool.id).unwrap().unwrap();
    assert!(stored.agent_input.is_some() && stored.agent_output.is_some());
}

#[test]
fn m8_feed_audit_a_team_change_marks_no_bot() {
    let (mut repo, bot) = with_bot();
    complete_turn(&mut repo, bot.main_chat, 0, "Bot reply", 10);
    let owner = owner_of(&bot);
    let head = repo.feed_head().unwrap();
    let markers = marker_count(&repo);
    repo.change_team_pause(owner, BotRequestId::mint(), &"a".repeat(64), 0, true, 100)
        .unwrap();
    // One new marker, the team's, and nothing else moved.
    assert_eq!(marker_count(&repo), markers + 1);
    let page = scan(&repo, &bot, head);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].marker.kind, FeedKind::TeamPause);
    assert_eq!(page.items[0].marker.subject_id, owner.device.to_storage());
    assert_eq!(page.items[0].marker.bot_id, None);
    let FeedBody::Team(team) = &page.items[0].body else {
        panic!("the team pause is not sent");
    };
    assert!(team.paused);
    assert_eq!(team.revision, 1);
    assert_eq!(team.stage, TeamPauseStage::Pending);
    // The acknowledgement and the resume each move the team's marker alone.
    let barrier = repo.team_pause_barrier(owner).unwrap();
    let pending = repo.feed_head().unwrap();
    repo.acknowledge_team_pause(owner, &barrier, 101).unwrap();
    let page = scan(&repo, &bot, pending);
    assert_eq!(page.items.len(), 1);
    assert!(
        matches!(&page.items[0].body, FeedBody::Team(team) if team.stage == TeamPauseStage::Acknowledged)
    );
    // An echo that changes nothing moves nothing.
    let acknowledged = repo.feed_head().unwrap();
    repo.acknowledge_team_pause(owner, &barrier, 102).unwrap();
    assert_eq!(repo.feed_head().unwrap(), acknowledged);
    repo.change_team_pause(owner, BotRequestId::mint(), &"b".repeat(64), 1, false, 103)
        .unwrap();
    assert_eq!(marker_count(&repo), markers + 1);
    // The team is not counted with the Bots, conversations and messages.
    assert_eq!(
        assert_counters(&repo),
        FeedCounts {
            bots: 1,
            chats: 1,
            messages: 2
        }
    );
    // Another computer's team row is not this owner's to send.
    let other = BotOwner {
        user_scope: owner.user_scope,
        device: DeviceId::from_uuid(Uuid::new_v4()),
    };
    let page = repo
        .feed_scan(other, acknowledged, 64, usize::MAX, &BTreeSet::new())
        .unwrap();
    assert!(matches!(page.items[0].body, FeedBody::Removed));
    assert!(repo.feed_audit_refusals().is_empty());
}

#[test]
fn m8_feed_audit_a_thousand_text_deltas_move_no_marker() {
    let (mut repo, bot) = with_bot();
    let pending = turn(bot.main_chat, Uuid::new_v4(), 1);
    repo.begin_turn(&pending).unwrap();
    let head = repo.feed_head().unwrap();
    let markers = marker_count(&repo);
    let (commits, _) = repo.feed_audit_commits();
    for index in 0..1_000 {
        repo.apply_event(
            &AiEvent::TextDelta {
                attempt_id: pending.attempt.id,
                text: "word ".into(),
                at: at(2 + index),
            },
            &[],
        )
        .unwrap();
    }
    // Usage and a context report into the pending reply move nothing either.
    repo.apply_event(
        &AiEvent::ContextUsage {
            attempt_id: pending.attempt.id,
            used: 10,
            size: 100,
        },
        &[],
    )
    .unwrap();
    // Every one of those commits was looked at, and none needed a marker.
    assert_eq!(repo.feed_audit_commits().0, commits + 1_001);
    assert_eq!(repo.feed_head().unwrap(), head);
    assert_eq!(marker_count(&repo), markers);
    assert!(scan(&repo, &bot, head).items.is_empty());
    assert!(repo.feed_audit_refusals().is_empty());
    // The reply's text is in the database, and is not read into a record
    // while the reply is pending.
    assert_eq!(
        repo.messages(bot.main_chat).unwrap()[1].markdown.len(),
        5_000
    );
    let page = scan(&repo, &bot, 0);
    let FeedBody::Message(record) = body_of(
        &page,
        FeedKind::Message,
        &pending.assistant_message.id.to_storage(),
    ) else {
        panic!("the pending reply is not sent");
    };
    assert_eq!(record.message.state, MessageState::Pending);
    assert_eq!(record.message.markdown, "");
    assert!(record.blocks.is_empty() && record.sources.is_empty());
    assert!(record.attempt.is_none() && record.run.is_none());
    // The reply ends: its marker and its conversation's move once.
    repo.apply_event(
        &AiEvent::Completed {
            attempt_id: pending.attempt.id,
            stop_reason: None,
            at: at(3_000),
        },
        &[],
    )
    .unwrap();
    let page = scan(&repo, &bot, head);
    assert_eq!(page.items.len(), 2);
    let FeedBody::Message(record) = body_of(
        &page,
        FeedKind::Message,
        &pending.assistant_message.id.to_storage(),
    ) else {
        panic!("the reply is not sent");
    };
    assert_eq!(record.message.markdown.len(), 5_000);
    assert_eq!(
        record
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.context_used_tokens),
        Some(10)
    );
    assert!(matches!(
        body_of(&page, FeedKind::Chat, &bot.main_chat.to_storage()),
        FeedBody::Chat(chat) if !chat.running
    ));
}

// ---------------------------------------------------------------- the pages

#[test]
fn m8_feed_pages_are_in_cursor_order_and_a_subject_changed_during_a_scan_appears_again() {
    let (mut repo, bot) = with_bot();
    for index in 0..4u32 {
        complete_turn(
            &mut repo,
            bot.main_chat,
            index * 2,
            "A reply",
            10 + i64::from(index) * 10,
        );
    }
    // One Bot, one conversation, eight messages.
    assert_eq!(
        assert_counters(&repo),
        FeedCounts {
            bots: 1,
            chats: 1,
            messages: 8
        }
    );
    let owner = owner_of(&bot);
    let copied = BTreeSet::from([bot.home_workspace]);
    // Pages of three, each from the last entry of the one before.
    let mut cursor = 0;
    let mut seen: Vec<(u64, FeedKind, String)> = Vec::new();
    let first = repo
        .feed_scan(owner, cursor, 3, usize::MAX, &copied)
        .unwrap();
    assert_eq!(first.items.len(), 3);
    assert!(!first.exhausted);
    cursor = first.items.last().unwrap().marker.seq;
    seen.extend(first.items.iter().map(|item| {
        (
            item.marker.seq,
            item.marker.kind,
            item.marker.subject_id.clone(),
        )
    }));
    // While the linked desktop is between pages, a subject it already has
    // changes, and so does one it has not reached.
    let had = seen[0].clone();
    assert_eq!(had.1, FeedKind::Bot);
    repo.rename_chat(bot.main_chat, "Renamed mid scan").unwrap();
    let mut renamed = bot.clone();
    renamed.config.name = "Renamed mid scan".into();
    renamed.revision = 2;
    renamed.updated_at_ms = 99;
    repo.save_bot(
        &renamed,
        ProfileId::new(),
        BotRequestId::mint(),
        "update",
        Some(1),
    )
    .unwrap();
    loop {
        let page = repo
            .feed_scan(owner, cursor, 3, usize::MAX, &copied)
            .unwrap();
        assert!(page.items.len() <= 3);
        for item in &page.items {
            assert!(item.marker.seq > cursor, "entries ascend past the cursor");
            cursor = item.marker.seq;
            seen.push((
                item.marker.seq,
                item.marker.kind,
                item.marker.subject_id.clone(),
            ));
        }
        assert_eq!(page.head, repo.feed_head().unwrap());
        if page.exhausted {
            assert_eq!(cursor, page.head);
            break;
        }
    }
    // Strictly ascending across pages.
    assert!(seen.windows(2).all(|pair| pair[0].0 < pair[1].0));
    // The Bot appears again, after everything else, as it is now; the
    // conversation appears once, at its new place, with its new title.
    let bots: Vec<&(u64, FeedKind, String)> = seen
        .iter()
        .filter(|entry| entry.1 == FeedKind::Bot)
        .collect();
    assert_eq!(bots.len(), 2);
    assert_eq!(bots[1].0, cursor);
    let chats: Vec<&(u64, FeedKind, String)> = seen
        .iter()
        .filter(|entry| entry.1 == FeedKind::Chat)
        .collect();
    assert_eq!(chats.len(), 1);
    let page = scan(&repo, &renamed, chats[0].0 - 1);
    assert!(matches!(
        body_of(&page, FeedKind::Chat, &bot.main_chat.to_storage()),
        FeedBody::Chat(chat) if chat.chat.title == "Renamed mid scan" && chat.chat.bot.is_none()
    ));
    assert!(matches!(
        body_of(&page, FeedKind::Bot, &bot.id.to_storage()),
        FeedBody::Bot(sent) if sent.config.name == "Renamed mid scan" && sent.revision == 2
            && sent.open_operation.is_none() && sent.pending_change.is_none()
    ));
    // Every message was seen exactly once.
    let messages: BTreeSet<&String> = seen
        .iter()
        .filter(|entry| entry.1 == FeedKind::Message)
        .map(|entry| &entry.2)
        .collect();
    assert_eq!(messages.len(), 8);
    assert_eq!(
        seen.iter()
            .filter(|entry| entry.1 == FeedKind::Message)
            .count(),
        8
    );
    // A scan at the head is empty and says so; a rename moved no counter.
    let page = repo
        .feed_scan(owner, cursor, 3, usize::MAX, &copied)
        .unwrap();
    assert!(page.items.is_empty() && page.exhausted && !page.stale_withheld);
    assert_eq!(
        assert_counters(&repo),
        FeedCounts {
            bots: 1,
            chats: 1,
            messages: 8
        }
    );
    assert!(repo.feed_scan(owner, 0, 0, usize::MAX, &copied).is_err());
    assert!(repo.feed_audit_refusals().is_empty());
}

#[test]
fn m8_feed_pages_a_scan_stops_at_its_text_budget_and_still_answers_one_entry() {
    let (mut repo, bot) = with_bot();
    let long = "x".repeat(4_000);
    for index in 0..3u32 {
        complete_turn(
            &mut repo,
            bot.main_chat,
            index * 2,
            &long,
            10 + i64::from(index) * 10,
        );
    }
    let owner = owner_of(&bot);
    let copied = BTreeSet::from([bot.home_workspace]);
    // The Bot and the conversation hold no message text; the first reply
    // passes a small budget and ends the scan, with more to come.
    let page = repo.feed_scan(owner, 0, 64, 1_000, &copied).unwrap();
    assert!(page.items.len() < 8);
    assert!(!page.exhausted);
    assert!(matches!(
        page.items.last().unwrap().body,
        FeedBody::Message(_)
    ));
    // From a cursor just before a long reply, a zero budget still answers it.
    let cursor = page.items[page.items.len() - 2].marker.seq;
    let one = repo.feed_scan(owner, cursor, 64, 0, &copied).unwrap();
    assert_eq!(one.items.len(), 1);
    assert!(!one.exhausted);
}

#[test]
fn m8_feed_pages_a_record_carries_what_the_transcript_shows_beside_a_message() {
    let (mut repo, bot) = with_bot();
    let pending = turn(bot.main_chat, Uuid::new_v4(), 1);
    repo.begin_turn(&pending).unwrap();
    // While it runs: the conversation says so, and names no Run, because no
    // Run was bound to this reply.
    let page = scan(&repo, &bot, 0);
    assert!(matches!(
        body_of(&page, FeedKind::Chat, &bot.main_chat.to_storage()),
        FeedBody::Chat(chat) if chat.running && !chat.needs_you && chat.run.is_none()
            && chat.purpose == BotChatPurpose::Main && !chat.trashed
    ));
    let notice = AgentNotice {
        id: AgentNoticeId::new(),
        attempt_id: pending.attempt.id,
        chat_id: bot.main_chat,
        assistant_message_id: pending.assistant_message.id,
        text: "A hook failed".into(),
        text_offset: None,
        created_at: at(3),
    };
    let before = seq_of(&repo, "message", &pending.assistant_message.id.to_storage()).unwrap();
    repo.create_agent_notice(&notice).unwrap();
    assert!(seq_of(&repo, "message", &pending.assistant_message.id.to_storage()).unwrap() > before);
    repo.apply_event(
        &AiEvent::Citation {
            attempt_id: pending.attempt.id,
            source: SourceLink {
                url: "https://example.invalid/a".into(),
                title: "A".into(),
            },
            at: at(4),
        },
        &[],
    )
    .unwrap();
    repo.apply_event(
        &AiEvent::Usage {
            attempt_id: pending.attempt.id,
            usage: AiUsage {
                prompt_tokens: Some(10),
                cached_prompt_tokens: None,
                completion_tokens: Some(5),
                reasoning_tokens: None,
                total_tokens: Some(15),
                source: UsageSource::ProviderReported,
            },
        },
        &[],
    )
    .unwrap();
    repo.apply_event(
        &AiEvent::Completed {
            attempt_id: pending.attempt.id,
            stop_reason: None,
            at: at(6),
        },
        &[MessageBlock {
            id: MessageBlockId::new(),
            message_id: pending.assistant_message.id,
            ordinal: 0,
            kind: MessageBlockKind::Paragraph,
            markdown: "Done.".into(),
            plain_text: "Done.".into(),
        }],
    )
    .unwrap();
    let page = scan(&repo, &bot, 0);
    let FeedBody::Message(record) = body_of(
        &page,
        FeedKind::Message,
        &pending.assistant_message.id.to_storage(),
    ) else {
        panic!("the reply is not sent");
    };
    assert_eq!(record.message.state, MessageState::Complete);
    assert_eq!(record.blocks.len(), 1);
    assert_eq!(record.blocks[0].markdown, "Done.");
    assert_eq!(record.notices.len(), 1);
    assert_eq!(record.notices[0].text, "A hook failed");
    assert_eq!(
        record.sources,
        [SourceLink {
            url: "https://example.invalid/a".into(),
            title: "A".into()
        }]
    );
    let attempt = record.attempt.as_ref().unwrap();
    assert_eq!(attempt.id, pending.attempt.id);
    assert_eq!(attempt.usage.total_tokens, Some(15));
    assert_eq!(record.attachment_count, 0);
    // The person's own message carries nothing of an attempt.
    let FeedBody::Message(question) = body_of(
        &page,
        FeedKind::Message,
        &pending.user_message.id.to_storage(),
    ) else {
        panic!("the message is not sent");
    };
    assert_eq!(question.message.markdown, "Question");
    assert!(question.attempt.is_none() && question.tool_activities.is_empty());
    // A Run bound to a reply is named by the conversation while that reply
    // runs, and by the reply once it has ended.
    let second = {
        let mut next = turn(bot.main_chat, Uuid::new_v4(), 20);
        next.user_message.ordinal = 2;
        next.assistant_message.ordinal = 3;
        next
    };
    repo.begin_turn(&second).unwrap();
    let run = koru_delegate_core::RunId::mint();
    let record_workspace = WorkspaceId::new();
    let before = seq_of(&repo, "chat", &bot.main_chat.to_storage()).unwrap();
    repo.record_bot_chat_run_for_fixture(
        &bot,
        second.attempt.id,
        bot.main_chat,
        second.assistant_message.id,
        run,
        record_workspace,
    )
    .unwrap();
    assert!(seq_of(&repo, "chat", &bot.main_chat.to_storage()).unwrap() > before);
    let page = scan(&repo, &bot, before);
    assert!(matches!(
        body_of(&page, FeedKind::Chat, &bot.main_chat.to_storage()),
        FeedBody::Chat(chat) if chat.run == Some((run, record_workspace))
    ));
    repo.cancel_bot_chat(second.attempt.id, at(30)).unwrap();
    let page = scan(&repo, &bot, before);
    assert!(matches!(
        body_of(&page, FeedKind::Chat, &bot.main_chat.to_storage()),
        FeedBody::Chat(chat) if chat.run.is_none() && !chat.running
    ));
    assert!(matches!(
        body_of(&page, FeedKind::Message, &second.assistant_message.id.to_storage()),
        FeedBody::Message(reply) if reply.run == Some(run) && reply.message.state == MessageState::Interrupted
    ));
    assert!(repo.feed_audit_refusals().is_empty());
}

#[test]
fn m8_feed_pages_a_conversation_that_is_not_listed_is_sent_as_removed_with_its_messages() {
    let (mut repo, bot) = with_bot();
    let done = complete_turn(&mut repo, bot.main_chat, 0, "Bot reply", 10);
    assert_eq!(assert_counters(&repo).messages, 2);
    // The guard of contract 2.6: a Bot conversation cannot be private
    // through the product, and if its row says so it is never sent.
    let head = repo.feed_head().unwrap();
    repo.set_chat_private(bot.main_chat, true).unwrap();
    let page = scan(&repo, &bot, head);
    assert_eq!(page.items.len(), 3);
    assert!(page.items.iter().all(|item| item.marker.removed));
    assert!(
        page.items
            .iter()
            .all(|item| matches!(item.body, FeedBody::Removed))
    );
    assert_eq!(
        assert_counters(&repo),
        FeedCounts {
            bots: 1,
            chats: 0,
            messages: 0
        }
    );
    // A message written into it while it is private is not sent either, and
    // a record is not built even if a marker said it may be.
    let later = complete_turn(&mut repo, bot.main_chat, 2, "More", 30);
    repo.connection
        .execute_batch("UPDATE bot_client_feed SET removed = 0")
        .unwrap();
    let page = scan(&repo, &bot, 0);
    for message in [done.user_message.id, later.assistant_message.id] {
        assert!(matches!(
            body_of(&page, FeedKind::Message, &message.to_storage()),
            FeedBody::Removed
        ));
    }
    assert!(matches!(
        body_of(&page, FeedKind::Chat, &bot.main_chat.to_storage()),
        FeedBody::Removed
    ));
    // Shown again: the conversation and every message of it come back.
    repo.connection
        .execute_batch("UPDATE bot_client_feed SET removed = 1 WHERE kind <> 'bot'")
        .unwrap();
    repo.connection
        .execute_batch("UPDATE bot_client_feed_meta SET live_chats = 0, live_messages = 0")
        .unwrap();
    let head = repo.feed_head().unwrap();
    repo.set_chat_private(bot.main_chat, false).unwrap();
    let page = scan(&repo, &bot, head);
    assert_eq!(page.items.len(), 5);
    assert!(
        page.items
            .iter()
            .all(|item| !matches!(item.body, FeedBody::Removed))
    );
    assert_eq!(
        assert_counters(&repo),
        FeedCounts {
            bots: 1,
            chats: 1,
            messages: 4
        }
    );
    // A conversation in Trash stays listed, as a state of its record.
    let additional = repo
        .create_bot_chat(
            &bot,
            ProfileId::new(),
            BotRequestId::mint(),
            &"c".repeat(64),
            at(40),
        )
        .unwrap();
    let page = scan(&repo, &bot, repo.feed_head().unwrap() - 1);
    assert!(matches!(
        body_of(&page, FeedKind::Chat, &additional.chat_id.to_storage()),
        FeedBody::Chat(chat) if chat.purpose == BotChatPurpose::Additional && !chat.trashed
    ));
    assert!(repo.feed_audit_refusals().is_empty());
}

// --------------------------------------------------------------- the starts

fn boot(index: u32) -> String {
    format!("{index:032x}")
}

/// A copy of the database as a backup would hold it, reopened.
fn restored(repo: &Repository) -> (tempfile::TempDir, Repository) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("ai.db");
    repo.connection
        .execute("VACUUM INTO ?1", [path.to_str().unwrap()])
        .unwrap();
    (directory, Repository::open(&path).unwrap())
}

#[test]
fn m8_feed_starts_an_ordinary_restart_resets_nobody() {
    let (mut repo, bot) = with_bot();
    let feed = repo.feed_id().unwrap();
    assert!(feed.len() == 32 && feed.bytes().all(|byte| byte.is_ascii_hexdigit()));
    // No start is recorded yet: nothing was served, so no cursor is valid.
    assert_eq!(repo.feed_current_start().unwrap(), None);
    assert!(!repo.feed_cursor_served(&feed, &boot(1), 0).unwrap());

    let first = repo.feed_start(&boot(1)).unwrap();
    assert_eq!(first, repo.feed_head().unwrap());
    assert_eq!(repo.feed_current_start().unwrap(), Some(boot(1)));
    complete_turn(&mut repo, bot.main_chat, 0, "Bot reply", 10);
    let head = repo.feed_head().unwrap();
    assert!(head > first);
    // Under the current start every cursor up to the head is valid, and one
    // past it is not.
    for cursor in [0, first, head] {
        assert!(repo.feed_cursor_served(&feed, &boot(1), cursor).unwrap());
    }
    assert!(!repo.feed_cursor_served(&feed, &boot(1), head + 1).unwrap());

    // The worker restarts. The new start begins at the head the old one
    // ended with, so every cursor the old start served still passes.
    assert_eq!(repo.feed_start(&boot(2)).unwrap(), head);
    for cursor in [0, first, head] {
        assert!(repo.feed_cursor_served(&feed, &boot(1), cursor).unwrap());
    }
    complete_turn(&mut repo, bot.main_chat, 2, "More", 30);
    let later = repo.feed_head().unwrap();
    assert!(repo.feed_cursor_served(&feed, &boot(2), later).unwrap());
    // The old start never served anything above where the new one began.
    assert!(!repo.feed_cursor_served(&feed, &boot(1), later).unwrap());
    // Another database's feed, an unknown start and a malformed one.
    assert!(
        !repo
            .feed_cursor_served(&"0".repeat(32), &boot(2), 0)
            .unwrap()
    );
    assert!(!repo.feed_cursor_served(&feed, &boot(9), 0).unwrap());
    assert_eq!(repo.feed_start("short"), Err(BotError::Invalid));
    assert_eq!(repo.feed_start(&"z".repeat(32)), Err(BotError::Invalid));
    // A start is recorded once.
    assert!(repo.feed_start(&boot(2)).is_err());
}

#[test]
fn m8_feed_starts_each_case_of_a_restore_from_a_backup() {
    let (mut repo, bot) = with_bot();
    let feed = repo.feed_id().unwrap();
    repo.feed_start(&boot(1)).unwrap();
    complete_turn(&mut repo, bot.main_chat, 0, "Before the backup", 10);
    // The backup is taken here, during start 1.
    let backup_head = repo.feed_head().unwrap();
    let (_directory, mut backup) = restored(&repo);
    assert_eq!(backup.feed_head().unwrap(), backup_head);
    assert_eq!(backup.feed_id().unwrap(), feed);

    // The owner goes on: more history under start 1, then a start 2.
    complete_turn(&mut repo, bot.main_chat, 2, "After the backup", 30);
    let lost_head = repo.feed_head().unwrap();
    assert!(lost_head > backup_head);
    repo.feed_start(&boot(2)).unwrap();
    complete_turn(&mut repo, bot.main_chat, 4, "Under start 2", 50);
    let lost_later = repo.feed_head().unwrap();
    // Three linked desktops, by what they last applied.
    let after_backup_start = (boot(2), lost_later);
    let same_start_ahead = (boot(1), lost_head);
    let same_start_behind = (boot(1), backup_head);
    for (start, cursor) in [&after_backup_start, &same_start_ahead, &same_start_behind] {
        assert!(repo.feed_cursor_served(&feed, start, *cursor).unwrap());
    }

    // The owner's database is restored from the backup, and its worker
    // starts: start 3, at the restored head.
    assert_eq!(backup.feed_start(&boot(3)).unwrap(), backup_head);
    // New changes on the restored owner reuse seq values the lost history
    // had already given out.
    complete_turn(
        &mut backup,
        bot.main_chat,
        2,
        "Written after the restore",
        70,
    );
    assert!(backup.feed_head().unwrap() > backup_head);
    assert!(backup.feed_head().unwrap() <= lost_later);

    // A desktop whose start is from after the backup: the restored owner
    // does not know it. Reset.
    assert!(
        !backup
            .feed_cursor_served(&feed, &after_backup_start.0, after_backup_start.1)
            .unwrap()
    );
    // A desktop on the start the backup was taken in, with a cursor above
    // the restored head: it holds rows the owner lost. Reset.
    assert!(
        !backup
            .feed_cursor_served(&feed, &same_start_ahead.0, same_start_ahead.1)
            .unwrap()
    );
    // A desktop at or below the restored head saw only history the restored
    // owner also has: it continues, and is sent everything above its cursor.
    assert!(
        backup
            .feed_cursor_served(&feed, &same_start_behind.0, same_start_behind.1)
            .unwrap()
    );
    let page = backup
        .feed_scan(
            owner_of(&bot),
            same_start_behind.1,
            64,
            usize::MAX,
            &BTreeSet::from([bot.home_workspace]),
        )
        .unwrap();
    assert!(page.items.iter().any(|item| matches!(
        &item.body,
        FeedBody::Message(record) if record.message.markdown == "Written after the restore"
    )));
    // The original, which was not restored, still serves all three.
    assert!(
        repo.feed_cursor_served(&feed, &boot(2), lost_later)
            .unwrap()
    );
}

#[test]
fn m8_feed_starts_a_start_older_than_the_kept_ones_resets() {
    let (mut repo, _bot) = with_bot();
    let feed = repo.feed_id().unwrap();
    repo.feed_start(&boot(1)).unwrap();
    let head = repo.feed_head().unwrap();
    assert!(repo.feed_cursor_served(&feed, &boot(1), head).unwrap());
    for index in 2..=FEED_STARTS_KEPT as u32 {
        repo.feed_start(&boot(index)).unwrap();
    }
    // Exactly the kept number of starts: the first is still there.
    assert!(repo.feed_cursor_served(&feed, &boot(1), head).unwrap());
    repo.feed_start(&boot(1_000)).unwrap();
    // One more, and the first is gone: a desktop that names it is reset.
    assert!(!repo.feed_cursor_served(&feed, &boot(1), head).unwrap());
    assert!(repo.feed_cursor_served(&feed, &boot(2), head).unwrap());
    let kept: i64 = repo
        .connection
        .query_row("SELECT count(*) FROM bot_client_feed_boots", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(kept, FEED_STARTS_KEPT);
    assert_eq!(repo.feed_current_start().unwrap(), Some(boot(1_000)));
}

// ------------------------------------------------- who is never sent a record

#[test]
fn m8_feed_local_owner_only_a_bot_of_another_device_or_person_produces_no_record() {
    let (mut repo, bot) = with_bot();
    let done = complete_turn(&mut repo, bot.main_chat, 0, "Bot reply", 10);
    let copied = BTreeSet::from([bot.home_workspace]);
    let local = owner_of(&bot);
    // To its own owner, everything is sent.
    let page = repo.feed_scan(local, 0, 64, usize::MAX, &copied).unwrap();
    assert_eq!(page.items.len(), 4);
    assert!(
        page.items
            .iter()
            .all(|item| !matches!(item.body, FeedBody::Removed))
    );
    assert!(!page.stale_withheld);
    // The same database on another device (a copied or restored `ai.db`),
    // and under another person: the rows name the first, so nothing of the
    // Bot is built, and the scan says its markers need a reconcile.
    for other in [
        BotOwner {
            user_scope: local.user_scope,
            device: DeviceId::from_uuid(Uuid::new_v4()),
        },
        BotOwner {
            user_scope: UserScopeId::new(),
            device: local.device,
        },
    ] {
        let page = repo.feed_scan(other, 0, 64, usize::MAX, &copied).unwrap();
        assert_eq!(page.items.len(), 4);
        assert!(
            page.items
                .iter()
                .all(|item| matches!(item.body, FeedBody::Removed)),
            "{page:?}"
        );
        assert!(page.stale_withheld);
        let homes = repo.feed_bot_homes(other).unwrap();
        assert_eq!(homes.len(), 1);
        assert!(!homes[0].local && !homes[0].withheld);
        assert_eq!(homes[0].home, bot.home_workspace);
    }
    // Two Bots in one database, one local and one not: only the local one.
    let mut foreign = fixture_bot();
    foreign.home_workspace = bot.home_workspace;
    foreign.config.role.workspace = bot.home_workspace;
    repo.save_bot(
        &foreign,
        ProfileId::new(),
        BotRequestId::mint(),
        "create-2",
        None,
    )
    .unwrap();
    complete_turn(&mut repo, foreign.main_chat, 0, "Another owner's reply", 30);
    let page = repo.feed_scan(local, 0, 64, usize::MAX, &copied).unwrap();
    assert_eq!(page.items.len(), 8);
    for item in &page.items {
        let foreign_subject = item.marker.bot_id == Some(foreign.id);
        assert_eq!(
            matches!(item.body, FeedBody::Removed),
            foreign_subject,
            "{item:?}"
        );
    }
    let homes = repo.feed_bot_homes(local).unwrap();
    assert_eq!(homes.iter().filter(|home| home.local).count(), 1);
    // The debug form of a scan and of an entry holds no text.
    let printed = format!("{page:?} {:?}", page.items);
    assert!(!printed.contains("Bot reply") && !printed.contains("Another owner"));
    assert!(!printed.contains(&done.user_message.markdown));
}

#[test]
fn m8_feed_withheld_a_bot_whose_home_is_not_copied_is_never_built_and_the_change_rewrites_its_markers()
 {
    let (mut repo, bot) = with_bot();
    let done = complete_turn(&mut repo, bot.main_chat, 0, "Bot reply", 10);
    let owner = owner_of(&bot);
    let copied = BTreeSet::from([bot.home_workspace]);
    let nothing = BTreeSet::new();
    assert_eq!(repo.feed_page_homes(0, 64).unwrap(), [bot.home_workspace]);

    // The Store says the home is kept on this computer only, or could not be
    // asked: the marker still says "not withheld", and no record is built.
    let page = repo.feed_scan(owner, 0, 64, usize::MAX, &nothing).unwrap();
    assert_eq!(page.items.len(), 4);
    assert!(
        page.items
            .iter()
            .all(|item| matches!(item.body, FeedBody::Removed))
    );
    assert!(page.items.iter().all(|item| !item.marker.withheld));
    assert!(page.stale_withheld);

    // The reconcile: the Bot's marker and every marker of its conversations
    // and messages get a new seq, the Bot first, and say "withheld".
    let head = repo.feed_head().unwrap();
    assert!(repo.feed_set_withheld(bot.id, true).unwrap());
    let page = repo
        .feed_scan(owner, head, 64, usize::MAX, &nothing)
        .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.marker.kind)
            .collect::<Vec<_>>(),
        [
            FeedKind::Bot,
            FeedKind::Chat,
            FeedKind::Message,
            FeedKind::Message
        ]
    );
    assert!(
        page.items
            .iter()
            .all(|item| item.marker.withheld && item.marker.seq > head)
    );
    assert!(
        page.items
            .iter()
            .all(|item| matches!(item.body, FeedBody::Removed))
    );
    assert!(!page.stale_withheld);
    assert_eq!(assert_counters(&repo), FeedCounts::default());
    assert_eq!(marker_count(&repo), 4);
    // Nothing to do twice.
    assert!(!repo.feed_set_withheld(bot.id, true).unwrap());
    assert!(repo.feed_bot_homes(owner).unwrap()[0].withheld);

    // While withheld, new work of the Bot is marked withheld from the
    // start, counted nowhere, and never built, whatever the caller says.
    let later = complete_turn(&mut repo, bot.main_chat, 2, "Written while withheld", 30);
    assert_eq!(assert_counters(&repo), FeedCounts::default());
    let page = repo.feed_scan(owner, 0, 64, usize::MAX, &copied).unwrap();
    assert_eq!(page.items.len(), 6);
    assert!(page.items.iter().all(|item| item.marker.withheld));
    assert!(
        page.items
            .iter()
            .all(|item| matches!(item.body, FeedBody::Removed))
    );
    // The caller says "copied" and the marker says "withheld": a reconcile
    // is due, and until it runs the marker wins.
    assert!(page.stale_withheld);

    // The workspace is copied again: every marker moves once more and the
    // Bot, its conversation and its messages are sent.
    let head = repo.feed_head().unwrap();
    assert!(repo.feed_set_withheld(bot.id, false).unwrap());
    let page = repo
        .feed_scan(owner, head, 64, usize::MAX, &copied)
        .unwrap();
    assert_eq!(page.items.len(), 6);
    assert_eq!(page.items[0].marker.kind, FeedKind::Bot);
    assert!(
        page.items
            .iter()
            .all(|item| !matches!(item.body, FeedBody::Removed))
    );
    assert!(!page.stale_withheld);
    for message in [done.assistant_message.id, later.assistant_message.id] {
        assert!(matches!(
            body_of(&page, FeedKind::Message, &message.to_storage()),
            FeedBody::Message(_)
        ));
    }
    assert_eq!(
        assert_counters(&repo),
        FeedCounts {
            bots: 1,
            chats: 1,
            messages: 4
        }
    );
    // A Bot with no marker is not this function's to mark.
    assert!(!repo.feed_set_withheld(BotId::mint(), true).unwrap());
    assert!(repo.feed_audit_refusals().is_empty());
}
