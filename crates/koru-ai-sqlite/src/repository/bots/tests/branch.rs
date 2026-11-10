// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::{
    BranchCopy, BranchOrigin, Chat, Message, MessageBlockId, MessageId, PendingTurn, ToolActivity,
    ToolActivityStatus, ToolApproval, ToolInvocationId,
};
use std::collections::HashMap;

const EXECUTION_TABLES: [&str; 5] = [
    "inference_attempts",
    "tool_invocations",
    "bot_chat_runs",
    "bot_run_replay",
    "bot_command_receipts",
];

fn setup() -> (Repository, Bot, ProfileId, PendingTurn) {
    let mut repo = Repository::in_memory().unwrap();
    let mut bot = fixture_bot();
    bot.config.mode = Some("plan".into());
    bot.config.fast = Some(true);
    let profile = ProfileId::new();
    repo.save_bot(&bot, profile, BotRequestId::mint(), "create", None)
        .unwrap();
    let source = complete_turn(&mut repo, bot.main_chat, 0, "Bot reply", 10);
    (repo, bot, profile, source)
}

/// What the worker's shared Branch copy builds: fresh identities, origins kept.
pub(in crate::repository::bots) fn copy(
    repo: &Repository,
    bot: &Bot,
    source: ChatId,
    through: MessageId,
) -> BranchCopy {
    let id = ChatId::new();
    let mut ids = HashMap::new();
    let messages: Vec<Message> = repo
        .messages(source)
        .unwrap()
        .into_iter()
        .map(|mut message| {
            let origin = message.id;
            message.id = MessageId::new();
            message.chat_id = id;
            message.origin_message_id = Some(origin);
            ids.insert(origin, message.id);
            message
        })
        .collect();
    let blocks = repo
        .blocks(source)
        .unwrap()
        .into_iter()
        .filter_map(|mut block| {
            block.message_id = *ids.get(&block.message_id)?;
            block.id = MessageBlockId::new();
            Some(block)
        })
        .collect();
    let mut tool_policy = repo.chat_tool_policy(source).unwrap().unwrap();
    tool_policy.chat_id = id;
    let source_chat = repo
        .chat_with_access(source, &ChatReadAccess::authorized(vec![source]))
        .unwrap()
        .unwrap();
    BranchCopy {
        chat: Chat {
            id,
            title: "Branch".into(),
            branched_from: Some(BranchOrigin {
                chat_id: source,
                through_message_id: through,
            }),
            agent_mode: bot.config.mode.clone(),
            agent_fast: bot.config.fast,
            pinned_at: None,
            created_at: Timestamp::from_millis(50),
            updated_at: Timestamp::from_millis(50),
            ..source_chat
        },
        tool_policy,
        messages,
        blocks,
        citations: vec![],
        pinned_documents: vec![],
        attachments: vec![],
    }
}

fn counts(repo: &Repository) -> Vec<u64> {
    EXECUTION_TABLES
        .iter()
        .map(|table| {
            repo.connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        })
        .collect()
}

fn chat_rows(repo: &Repository) -> u64 {
    repo.connection
        .query_row("SELECT count(*) FROM chats", [], |row| row.get(0))
        .unwrap()
}

/// A Run binding as P1.5 records it; enough rows for the read-only resolver.
fn record_run(repo: &Repository, bot: &Bot, turn: &PendingTurn) -> koru_delegate_core::RunId {
    let run = koru_delegate_core::RunId::mint();
    let request = BotRequestId::mint();
    repo.connection.execute("INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,captured_config_revision,captured_policy_revision,submission_version,submission_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'fixture','start','prepared',?4,1,1,1,'{}',1,1)",
        params![bot.user_scope.to_storage(), request.to_storage(), bot.id.to_storage(), run.to_storage()]).unwrap();
    repo.connection.execute("INSERT INTO bot_chat_runs(ai_attempt_id,user_scope_id,owner_device_id,submission_id,bot_id,chat_id,assistant_message_id,run_id,record_workspace_id,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1)",
        params![turn.attempt.id.to_storage(), bot.user_scope.to_storage(), bot.owner_device.to_storage(), request.to_storage(), bot.id.to_storage(), turn.assistant_message.chat_id.to_storage(), turn.assistant_message.id.to_storage(), run.to_storage(), bot.home_workspace.to_storage()]).unwrap();
    run
}

#[test]
fn bot_branch_commits_copy_with_its_association_and_launches_nothing() {
    let (mut repo, bot, _, source) = setup();
    repo.create_tool_activity(&ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: source.attempt.id,
        chat_id: bot.main_chat,
        assistant_message_id: source.assistant_message.id,
        tool_name: "update_document".into(),
        read_only: false,
        status: ToolActivityStatus::Proposed,
        approval: ToolApproval::Pending,
        proposed_at: Timestamp::from_millis(11),
        started_at: None,
        completed_at: None,
        result_bytes: None,
        error_code: None,
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: None,
        agent_summary: None,
        agent_input: None,
        agent_output: None,
        text_offset: None,
        display_target: None,
        live: None,
    })
    .unwrap();
    let source_messages = repo.messages(bot.main_chat).unwrap();
    let before = counts(&repo);
    let branch = copy(&repo, &bot, bot.main_chat, source.assistant_message.id);
    repo.create_bot_branch(&bot, &branch).unwrap();
    assert_eq!(repo.bot_for_chat(branch.chat.id).unwrap(), Some(bot.id));
    let purpose: String = repo
        .connection
        .query_row(
            "SELECT purpose FROM bot_chats WHERE chat_id=?1",
            [branch.chat.id.to_storage()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(purpose, "additional");
    assert!(
        repo.chat(branch.chat.id).unwrap().is_none(),
        "ordinary reads stay blind"
    );
    let saved = repo
        .chat_with_access(
            branch.chat.id,
            &ChatReadAccess::authorized(vec![branch.chat.id]),
        )
        .unwrap()
        .unwrap();
    assert_eq!(saved, branch.chat);
    assert_eq!(repo.messages(branch.chat.id).unwrap(), branch.messages);
    assert_eq!(repo.blocks(branch.chat.id).unwrap(), branch.blocks);
    assert!(repo.tool_activities(branch.chat.id).unwrap().is_empty());
    assert_eq!(repo.messages(bot.main_chat).unwrap(), source_messages);
    assert_eq!(repo.tool_activities(bot.main_chat).unwrap().len(), 1);
    assert_eq!(
        counts(&repo),
        before,
        "no attempt, tool, Run or receipt row"
    );
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot.clone()));
    assert_eq!(
        repo.create_bot_branch(&bot, &branch),
        Err(BotError::Conflict)
    );
}

#[test]
fn bot_branch_refuses_before_writing_and_rolls_back_a_late_failure() {
    let (mut repo, bot, _, source) = setup();
    let rows = chat_rows(&repo);
    let branch = copy(&repo, &bot, bot.main_chat, source.assistant_message.id);
    let mut stale = bot.clone();
    stale.revision += 1;
    assert_eq!(
        repo.create_bot_branch(&stale, &branch),
        Err(BotError::RevisionConflict)
    );
    let mut foreign = bot.clone();
    foreign.owner_device = DeviceId::from_uuid(Uuid::new_v4());
    assert_eq!(
        repo.create_bot_branch(&foreign, &branch),
        Err(BotError::Forbidden)
    );
    let ordinary = ChatId::new();
    repo.create_chat(&chat(ordinary, 1)).unwrap();
    let mut from_ordinary = branch.clone();
    from_ordinary.chat.branched_from = Some(BranchOrigin {
        chat_id: ordinary,
        through_message_id: source.assistant_message.id,
    });
    assert_eq!(
        repo.create_bot_branch(&bot, &from_ordinary),
        Err(BotError::Forbidden)
    );
    let mut occupied = copy(&repo, &bot, bot.main_chat, source.assistant_message.id);
    occupied.chat.id = ordinary;
    occupied.tool_policy.chat_id = ordinary;
    for message in &mut occupied.messages {
        message.chat_id = ordinary;
    }
    assert_eq!(
        repo.create_bot_branch(&bot, &occupied),
        Err(BotError::Conflict)
    );
    assert_eq!(chat_rows(&repo), rows + 1);
    repo.connection.execute_batch("CREATE TRIGGER refuse_branch_association BEFORE INSERT ON bot_chats WHEN NEW.purpose='additional' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert_eq!(
        repo.create_bot_branch(&bot, &branch),
        Err(BotError::Storage)
    );
    assert_eq!(chat_rows(&repo), rows + 1);
    assert!(repo.messages(branch.chat.id).unwrap().is_empty());
    repo.connection
        .execute_batch("DROP TRIGGER refuse_branch_association")
        .unwrap();
    repo.connection
        .execute(
            "UPDATE bots SET lifecycle='trashed' WHERE bot_id=?1",
            [bot.id.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.create_bot_branch(&bot, &branch),
        Err(BotError::Forbidden)
    );
    assert_eq!(chat_rows(&repo), rows + 1);
}

#[test]
fn historical_run_resolves_through_copies_boundedly_and_confers_nothing() {
    let (mut repo, bot, _, source) = setup();
    let run = record_run(&repo, &bot, &source);
    let original = HistoricalBotRun {
        bot: bot.id,
        chat: bot.main_chat,
        assistant_message: source.assistant_message.id,
        run,
    };
    assert_eq!(
        repo.historical_bot_run(source.assistant_message.id)
            .unwrap(),
        Some(original.clone())
    );
    let first = copy(&repo, &bot, bot.main_chat, source.assistant_message.id);
    repo.create_bot_branch(&bot, &first).unwrap();
    let first_reply = first.messages[1].id;
    let second = copy(&repo, &bot, first.chat.id, first_reply);
    repo.create_bot_branch(&bot, &second).unwrap();
    for reply in [first_reply, second.messages[1].id] {
        assert_eq!(
            repo.historical_bot_run(reply).unwrap(),
            Some(original.clone())
        );
    }
    // A question is not a Run's reply, even when copied.
    assert_eq!(
        repo.historical_bot_run(second.messages[0].id).unwrap(),
        None
    );
    // Resolving is a read: no execution row appears for either branch.
    let runs: u64 = repo
        .connection
        .query_row("SELECT count(*) FROM bot_chat_runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(runs, 1);
    // A damaged cyclic chain stops at the bound instead of looping.
    let ordinary = ChatId::new();
    repo.create_chat(&chat(ordinary, 1)).unwrap();
    let looped = complete_turn(&mut repo, ordinary, 0, "Ordinary", 20);
    let (question, reply) = (looped.user_message.id, looped.assistant_message.id);
    for (row, origin) in [(question, reply), (reply, question)] {
        repo.connection
            .execute(
                "UPDATE messages SET origin_message_id=?2 WHERE id=?1",
                params![row.to_storage(), origin.to_storage()],
            )
            .unwrap();
    }
    assert_eq!(repo.historical_bot_run(reply).unwrap(), None);
    // A purged origin (its foreign key set to NULL) ends the chain.
    repo.connection
        .execute(
            "UPDATE messages SET origin_message_id=NULL WHERE id=?1",
            [first_reply.to_storage()],
        )
        .unwrap();
    assert_eq!(
        repo.historical_bot_run(second.messages[1].id).unwrap(),
        None
    );
}
