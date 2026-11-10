// SPDX-License-Identifier: MPL-2.0
//! Branch a Bot conversation into another conversation with the same Bot.
use super::*;
use std::collections::BTreeSet;

/// A Bot whose own mode/Fast differ from its main conversation's, so the O24
/// seeding is observable, with one finished reply to branch through.
fn source() -> (Repository, Arc<Authority>, Bot, PendingTurn) {
    let (mut repo, authority, mut command) = fixture();
    if let BotCommand::Create { config, .. } = &mut command {
        config.mode = Some("plan".into());
        config.fast = Some(true);
    }
    let bot = created(&mut repo, authority.clone(), command);
    repo.patch_chat_agent_settings(
        bot.main_chat,
        Some("edit"),
        Some(false),
        &ChatReadAccess::authorized(vec![bot.main_chat]),
    )
    .unwrap();
    let pending = complete(&mut repo, bot.main_chat, 1, vec![]);
    (repo, authority, bot, pending)
}

fn complete(
    repo: &mut Repository,
    chat: ChatId,
    offset: i64,
    attachments: Vec<ChatAttachmentId>,
) -> PendingTurn {
    let mut pending = turn(chat, Uuid::new_v4(), offset);
    pending.attachment_ids = attachments;
    repo.begin_turn(&pending).unwrap();
    repo.apply_event(
        &AiEvent::Completed {
            attempt_id: pending.attempt.id,
            stop_reason: None,
            at: at(offset + 1),
        },
        &[MessageBlock {
            id: MessageBlockId::new(),
            message_id: pending.assistant_message.id,
            ordinal: 0,
            kind: MessageBlockKind::Paragraph,
            markdown: "Bot reply".into(),
            plain_text: "Bot reply".into(),
        }],
    )
    .unwrap();
    pending
}

fn worker(
    repo: Repository,
    authority: Arc<Authority>,
) -> (WorkerHandle, AiWorkerClient, Receiver<Repository>) {
    let (returned_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-branch"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    config.test_repository_return = Some(returned_tx);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    (worker, client, returned)
}

fn request(destination: ChatId, through: MessageId, title: &str) -> BranchChatRequest {
    BranchChatRequest {
        branch_chat_id: destination,
        through_message_id: through,
        title: title.into(),
    }
}

#[test]
fn bot_branch_copies_the_prefix_into_the_same_bot_and_starts_nothing() {
    let (repo, authority, bot, pending) = source();
    let source_messages = repo.messages(bot.main_chat).unwrap();
    let source_chat = repo
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    let attempts = repo.attempts_for_chat(bot.main_chat).unwrap();
    let (worker, client, returned) = worker(repo, authority);
    let destination = ChatId::new();
    let asked = request(destination, pending.assistant_message.id, "Branch");
    let branched = client
        .branch_chat(context(true), bot.main_chat, asked.clone())
        .unwrap()
        .transcript;
    assert_eq!(branched.chat.id, destination);
    assert_eq!(
        branched.chat.branched_from,
        Some(BranchOrigin {
            chat_id: bot.main_chat,
            through_message_id: pending.assistant_message.id,
        })
    );
    // O24: model and reasoning from the source; mode and Fast from the Bot.
    assert_eq!(branched.chat.current_model, source_chat.current_model);
    assert_eq!(branched.chat.agent_mode.as_deref(), Some("plan"));
    assert_eq!(branched.chat.agent_fast, Some(true));
    assert_eq!(
        branched.chat.agent_working_folder,
        source_chat.agent_working_folder
    );
    assert_eq!(branched.chat.profile_id, source_chat.profile_id);
    let origins: Vec<_> = branched
        .messages
        .iter()
        .map(|message| message.origin_message_id)
        .collect();
    assert_eq!(
        origins,
        source_messages
            .iter()
            .map(|message| Some(message.id))
            .collect::<Vec<_>>()
    );
    assert!(
        branched
            .messages
            .iter()
            .all(|message| source_messages.iter().all(|s| s.id != message.id))
    );
    // A lost reply retries to the same branch; another use of the identity conflicts.
    let again = client
        .branch_chat(context(true), bot.main_chat, asked)
        .unwrap()
        .transcript;
    assert_eq!(again.chat, branched.chat);
    assert_eq!(again.messages, branched.messages);
    assert_eq!(
        client
            .branch_chat(
                context(true),
                bot.main_chat,
                request(destination, pending.assistant_message.id, "Renamed"),
            )
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    // The branch reads only under the current owner proof.
    assert_eq!(
        client.read_chat(context(true), destination).unwrap().chat,
        branched.chat
    );
    assert_eq!(
        client
            .read_chat(context(false), destination)
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        client
            .branch_chat(
                context(false),
                bot.main_chat,
                request(ChatId::new(), pending.assistant_message.id, "Hidden"),
            )
            .unwrap_err()
            .code,
        "not_found"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repo = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(repo.bot_for_chat(destination).unwrap(), Some(bot.id));
    let (reply, _) = sync_channel(1);
    let read = WorkerCommand::ReadChat {
        context: context(true).into(),
        chat: destination,
        reply,
    };
    assert_eq!(
        preparation::bot_targets(&repo, &read).unwrap(),
        Some(vec![(bot.id, destination)])
    );
    assert_eq!(repo.messages(bot.main_chat).unwrap(), source_messages);
    assert!(repo.attempts_for_chat(destination).unwrap().is_empty());
    assert!(repo.tool_activities(destination).unwrap().is_empty());
    assert_eq!(repo.attempts_for_chat(bot.main_chat).unwrap(), attempts);
    // Table-level counts of Run, replay and receipt rows are in the repository tests.
    for message in &branched.messages {
        assert_eq!(repo.historical_bot_run(message.id).unwrap(), None);
    }
    assert_eq!(repo.bot(bot.id).unwrap(), Some(bot));
}

#[test]
fn branch_destination_is_unused_or_this_bots_own_earlier_branch() {
    let (mut repo, authority, bot, pending) = source();
    let (_, other_authority, other_command) = fixture();
    let other = created(&mut repo, other_authority, other_command);
    let additional = repo
        .create_bot_chat(
            &bot,
            authority.profile,
            BotRequestId::mint(),
            "additional",
            at(5),
        )
        .unwrap()
        .chat_id;
    let ordinary = repo
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .map(|chat| Chat {
            id: ChatId::new(),
            ..chat.unwrap()
        })
        .unwrap();
    repo.create_chat(&ordinary).unwrap();
    let ordinary_turn = complete(&mut repo, ordinary.id, 6, vec![]);
    // The proof names an unused destination for this Bot, never another owner's.
    let branch_command = |destination| {
        let (reply, _) = sync_channel(1);
        WorkerCommand::BranchChat {
            context: context(true).into(),
            chat: bot.main_chat,
            request: request(destination, pending.assistant_message.id, "Branch"),
            reply,
        }
    };
    let fresh = ChatId::new();
    assert_eq!(
        preparation::bot_targets(&repo, &branch_command(fresh)).unwrap(),
        Some(
            [(bot.id, bot.main_chat), (bot.id, fresh)]
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        )
    );
    assert_eq!(
        preparation::bot_targets(&repo, &branch_command(other.main_chat)).unwrap(),
        Some(vec![(bot.id, bot.main_chat)])
    );
    let chats = repo.all_bot_chats().unwrap().len();
    let (worker, client, returned) = worker(repo, authority);
    for (source, through, destination) in [
        // Bot to another Bot, an existing ordinary Chat, its own main and additional Chats.
        (bot.main_chat, pending.assistant_message.id, other.main_chat),
        (bot.main_chat, pending.assistant_message.id, ordinary.id),
        (bot.main_chat, pending.assistant_message.id, bot.main_chat),
        (bot.main_chat, pending.assistant_message.id, additional),
        // An ordinary source cannot reach a Bot conversation.
        (ordinary.id, ordinary_turn.assistant_message.id, additional),
        (
            ordinary.id,
            ordinary_turn.assistant_message.id,
            bot.main_chat,
        ),
    ] {
        assert_eq!(
            client
                .branch_chat(
                    context(true),
                    source,
                    request(destination, through, "Branch")
                )
                .unwrap_err()
                .code,
            "idempotency_conflict",
            "{destination:?}"
        );
    }
    // Ordinary Branch is unchanged: it still resets mode/Fast and has no Bot owner.
    let plain = ChatId::new();
    let branched = client
        .branch_chat(
            context(false),
            ordinary.id,
            request(plain, ordinary_turn.assistant_message.id, "Plain"),
        )
        .unwrap()
        .transcript;
    assert_eq!(branched.chat.agent_mode, None);
    assert_eq!(branched.chat.agent_fast, None);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repo = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    // Every refused identity already existed, so only the association set can show a write.
    assert_eq!(repo.all_bot_chats().unwrap().len(), chats);
    assert_eq!(repo.bot_for_chat(plain).unwrap(), None);
    assert!(repo.chat(plain).unwrap().is_some());
}

#[test]
fn bot_branch_with_attached_files_refuses_before_any_write() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let attachment = branch_attachment(bot.main_chat);
    repo.stage_attachment(&attachment).unwrap();
    let pending = complete(&mut repo, bot.main_chat, 1, vec![attachment.attachment.id]);
    let records = repo.attachment_records(bot.main_chat).unwrap();
    assert_eq!(records.len(), 1);
    let chats = repo.all_bot_chats().unwrap();
    let (worker, client, returned) = worker(repo, authority);
    let destination = ChatId::new();
    let failure = client
        .branch_chat(
            context(true),
            bot.main_chat,
            request(destination, pending.assistant_message.id, "Branch"),
        )
        .unwrap_err();
    assert_eq!(failure.code, "unsupported_capability");
    assert!(!failure.retryable);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    let repo = returned.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(repo.all_bot_chats().unwrap(), chats);
    assert!(repo.chat(destination).unwrap().is_none());
    assert!(repo.messages(destination).unwrap().is_empty());
    assert_eq!(repo.attachment_records(bot.main_chat).unwrap(), records);
}
