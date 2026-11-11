// SPDX-License-Identifier: MPL-2.0
//! Lifecycle holds on the worker: a conversation in Trash, or captured by an
//! unfinished operation, refuses changes with a stable code before any
//! mutation; a trashed one leaves every aggregate and reads as neither the
//! Bot's nor an ordinary chat.
use super::*;
use koru_ai_core::chat_tools::{ListChatsToolRequest, ReadChatToolRequest, SearchChatsToolRequest};
use koru_ai_sqlite::{LifecycleCapture, LifecycleMemberDraft, LifecycleOperation};
use koru_ai_wire::bot_lifecycle_codes::{PENDING, TRASHED};

fn owner_of(bot: &Bot) -> BotOwner {
    BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    }
}

fn capture(bot: &Bot, kind: BotLifecycleKind, chat: Option<ChatId>) -> LifecycleCapture {
    LifecycleCapture {
        operation: BotRequestId::mint(),
        owner: owner_of(bot),
        bot: bot.id,
        kind,
        chat,
        fingerprint: "b".repeat(64),
        expected_revision: bot.revision,
        source: None,
        members: chat
            .map(|chat_id| LifecycleMemberDraft {
                subject: BotLifecycleSubject::Chat { chat_id },
                reason: BotLifecycleReason::OwnedConversation,
                prior_state: BotLifecyclePriorState::Live,
                intended: BotLifecycleIntended::Hide,
                retained: None,
            })
            .into_iter()
            .collect(),
        now: 7,
    }
}

fn completed_turn(repo: &mut Repository, chat: ChatId, offset: i64, text: &str) -> PendingTurn {
    let pending = turn(chat, Uuid::new_v4(), offset);
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
            markdown: text.into(),
            plain_text: text.into(),
        }],
    )
    .unwrap();
    pending
}

/// A Bot with a kept main conversation and an additional one in Trash.
fn trashed() -> (
    Repository,
    Arc<Authority>,
    Bot,
    ChatId,
    PendingTurn,
    LifecycleOperation,
) {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    completed_turn(&mut repo, bot.main_chat, 1, "Visible kept answer");
    let additional = repo
        .create_bot_chat(&bot, authority.profile, BotRequestId::mint(), "x", at(3))
        .unwrap()
        .chat_id;
    let hidden = completed_turn(&mut repo, additional, 4, "Hidden trashed answer");
    let operation = repo
        .capture_bot_lifecycle(&capture(
            &bot,
            BotLifecycleKind::TrashChat,
            Some(additional),
        ))
        .unwrap();
    repo.fixture_trash_bot_chat(&operation, additional, 9)
        .unwrap();
    (repo, authority, bot, additional, hidden, operation)
}

fn worker(repo: Repository, authority: Arc<Authority>) -> (WorkerHandle, AiWorkerClient) {
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-holds"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    (worker, client)
}

fn usage_context() -> RequestContext {
    let mut context = context(true);
    context.principal.scopes.push(AiScope::ReadUsage);
    context
}

fn send(chat: ChatId, workspace: WorkspaceId, refs: Vec<ContextRef>) -> SendTurnRequest {
    SendTurnRequest {
        destination_workspace: Some(workspace),
        operation_id: Uuid::new_v4(),
        chat_id: chat,
        surface: AiSurface::Standalone,
        markdown: "CURRENT-TASK".into(),
        explicit_refs: refs,
        web_search: false,
        reasoning_override: None,
        system_prompt_override: None,
        replace_pinned_documents: false,
        attachment_ids: vec![],
        answers: None,
        skills: vec![],
    }
}

#[test]
fn a_trashed_conversation_refuses_every_change_and_named_read_before_mutation() {
    let (repo, authority, bot, additional, hidden, _) = trashed();
    let profile = authority.profile;
    let (worker, client) = worker(repo, authority);
    let code = |result: ApiResult<()>| result.unwrap_err().code;
    assert_eq!(
        code(
            client
                .send_turn(context(true), send(additional, bot.home_workspace, vec![]))
                .map(|_| ())
        ),
        TRASHED
    );
    assert_eq!(
        code(
            client
                .retry_turn(
                    context(true),
                    additional,
                    koru_ai_wire::RetryTurnRequest {
                        destination_workspace: Some(bot.home_workspace),
                        operation_id: Uuid::new_v4(),
                        message_id: hidden.assistant_message.id,
                        surface: AiSurface::Standalone,
                        explicit_refs: vec![],
                        reasoning_override: None,
                        web_search: false,
                        replace_pinned_documents: false,
                    },
                )
                .map(|_| ())
        ),
        TRASHED
    );
    assert_eq!(
        code(
            client
                .branch_chat(
                    context(true),
                    additional,
                    BranchChatRequest {
                        branch_chat_id: ChatId::new(),
                        through_message_id: hidden.assistant_message.id,
                        title: "Branch".into(),
                    },
                )
                .map(|_| ())
        ),
        TRASHED
    );
    assert_eq!(
        code(
            client
                .update_chat_model(
                    context(true),
                    additional,
                    koru_ai_wire::UpdateChatModelRequest {
                        model: bot.config.model.clone(),
                    },
                )
                .map(|_| ())
        ),
        TRASHED
    );
    assert_eq!(
        code(
            client
                .update_chat_agent_settings(
                    context(true),
                    additional,
                    koru_ai_wire::UpdateChatAgentSettingsRequest {
                        mode: Some("plan".into()),
                        fast: None,
                    },
                )
                .map(|_| ())
        ),
        TRASHED
    );
    assert_eq!(
        code(client.read_chat(context(true), additional).map(|_| ())),
        TRASHED
    );
    assert_eq!(
        code(
            client
                .tool_read_chat(
                    context(true),
                    ReadChatToolRequest {
                        chat_id: additional,
                        cursor: None,
                        page_size: None,
                    },
                )
                .map(|_| ())
        ),
        TRASHED
    );
    assert_eq!(
        code(
            client
                .export_chat(
                    context(true),
                    additional,
                    koru_ai_wire::ExportChatRequest {
                        export_id: DocumentExportId::new(),
                        idempotency_key: Uuid::new_v4(),
                        through_message_id: hidden.assistant_message.id,
                        destination: DocumentDestination {
                            profile_id: profile,
                            workspace_id: bot.home_workspace,
                            parent_document_id: None,
                            index: None,
                            title: "Saved".into(),
                        },
                    },
                )
                .map(|_| ())
        ),
        TRASHED
    );
    // An ordinary caller still sees nothing at all.
    assert_eq!(
        code(client.read_chat(context(false), additional).map(|_| ())),
        "not_found"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn trashed_conversations_leave_every_aggregate_and_the_kept_one_stays() {
    let (repo, authority, bot, additional, _, _) = trashed();
    let (worker, client) = worker(repo, authority);
    let listed: Vec<ChatId> = client
        .list_chats(context(true), None)
        .unwrap()
        .chats
        .into_iter()
        .map(|chat| chat.id)
        .collect();
    assert!(listed.contains(&bot.main_chat));
    assert!(!listed.contains(&additional));
    let search = |text: &str| {
        client
            .search(
                context(true),
                koru_ai_wire::AiSearchRequest {
                    text: text.into(),
                    limit: 50,
                    cursor: None,
                    request_seq: 1,
                    profile: None,
                },
            )
            .unwrap()
            .page
            .hits
    };
    assert!(!search("Visible").is_empty());
    assert!(search("Hidden").is_empty());
    let tools: Vec<ChatId> = client
        .tool_list_chats(
            context(true),
            ListChatsToolRequest {
                cursor: None,
                page_size: None,
            },
        )
        .unwrap()
        .chats
        .into_iter()
        .map(|chat| chat.chat_id)
        .collect();
    assert!(tools.contains(&bot.main_chat));
    assert!(!tools.contains(&additional));
    let hits = |query: &str| {
        client
            .tool_search_chats(
                context(true),
                SearchChatsToolRequest {
                    query: query.into(),
                    cursor: None,
                    page_size: None,
                },
            )
            .unwrap()
            .hits
    };
    assert!(!hits("Visible").is_empty());
    assert!(hits("Hidden").is_empty());
    // Usage counts only the kept conversation's attempt.
    let attempts: u64 = client
        .usage(usage_context(), None)
        .unwrap()
        .report
        .groups
        .iter()
        .map(|group| group.totals.attempts)
        .sum();
    assert_eq!(attempts, 1);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn guard_refuses_trashed_context_and_shows_holds_only_to_the_owner() {
    let (repo, authority, bot, additional, _, _) = trashed();
    let block_in_trash = repo.blocks(additional).unwrap()[0].id;
    let own: Arc<dyn BotAuthority> = authority.clone();
    let (_, foreign, _) = fixture();
    let foreign: Arc<dyn BotAuthority> = foreign;
    let refused = |authority: &Arc<dyn BotAuthority>, command: WorkerCommand| {
        assert!(guard_command(&repo, Some(authority), command).is_none());
    };
    let (reply, response) = sync_channel(1);
    refused(
        &own,
        WorkerCommand::SendTurn {
            context: context(true).into(),
            request: send(
                bot.main_chat,
                bot.home_workspace,
                vec![ContextRef::MessageBlock {
                    block_id: block_in_trash,
                }],
            ),
            reply,
        },
    );
    assert_eq!(
        response.recv().unwrap().map(|_| ()).unwrap_err().code,
        TRASHED
    );
    let (reply, response) = sync_channel(1);
    refused(
        &foreign,
        WorkerCommand::ReadChat {
            context: context(true).into(),
            chat: additional,
            reply,
        },
    );
    assert_eq!(response.recv().unwrap().unwrap_err().code, "wrong_owner");
}

#[test]
fn an_unfinished_operation_holds_changes_but_not_reads_or_new_conversations() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    completed_turn(&mut repo, bot.main_chat, 1, "Kept answer");
    repo.capture_bot_lifecycle(&capture(
        &bot,
        BotLifecycleKind::TrashChat,
        Some(bot.main_chat),
    ))
    .unwrap();
    let (worker, client) = worker(repo, authority);
    assert_eq!(
        client
            .send_turn(
                context(true),
                send(bot.main_chat, bot.home_workspace, vec![])
            )
            .map(|_| ())
            .unwrap_err()
            .code,
        PENDING
    );
    // Reads stay available while the operation settles.
    assert_eq!(
        client
            .read_chat(context(true), bot.main_chat)
            .unwrap()
            .chat
            .id,
        bot.main_chat
    );
    // A conversation operation does not hold the Bot: New conversation works.
    assert!(matches!(
        client.bots(
            context(true),
            BotCommand::CreateChat {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
            },
        ),
        Ok(BotResponse::Chat { .. })
    ));
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn a_whole_bot_operation_holds_new_conversations_and_configuration() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    repo.capture_bot_lifecycle(&capture(&bot, BotLifecycleKind::Archive, None))
        .unwrap();
    let paused = repo.bot(bot.id).unwrap().unwrap();
    let (worker, client) = worker(repo, authority);
    for command in [
        BotCommand::CreateChat {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
        },
        BotCommand::Update {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: paused.revision,
            config: paused.config.clone(),
        },
        BotCommand::Resume {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: paused.revision,
        },
    ] {
        assert_eq!(
            client.bots(context(true), command).unwrap_err().code,
            PENDING
        );
    }
    assert_eq!(
        client
            .send_turn(
                context(true),
                send(bot.main_chat, bot.home_workspace, vec![])
            )
            .map(|_| ())
            .unwrap_err()
            .code,
        PENDING
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn o7_a_trashed_main_is_not_replaced_and_new_conversations_are_additional() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let operation = repo
        .capture_bot_lifecycle(&capture(
            &bot,
            BotLifecycleKind::TrashChat,
            Some(bot.main_chat),
        ))
        .unwrap();
    repo.fixture_trash_bot_chat(&operation, bot.main_chat, 9)
        .unwrap();
    repo.fixture_complete_bot_lifecycle(&operation, 10).unwrap();
    let (worker, client) = worker(repo, authority);
    let BotResponse::Bot { bot: read } = client
        .bots(context(true), BotCommand::Read { bot_id: bot.id })
        .unwrap()
    else {
        panic!("Bot")
    };
    assert_eq!(read.main_chat, bot.main_chat);
    let BotResponse::Chat { reference } = client
        .bots(
            context(true),
            BotCommand::CreateChat {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
            },
        )
        .unwrap()
    else {
        panic!("Chat")
    };
    assert_ne!(reference.chat_id, bot.main_chat);
    let listed: Vec<ChatId> = client
        .list_chats(context(true), None)
        .unwrap()
        .chats
        .into_iter()
        .map(|chat| chat.id)
        .collect();
    assert_eq!(listed, vec![reference.chat_id]);
    assert_eq!(
        client
            .send_turn(
                context(true),
                send(bot.main_chat, bot.home_workspace, vec![])
            )
            .map(|_| ())
            .unwrap_err()
            .code,
        TRASHED
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn the_archived_and_trash_filters_list_only_the_owners_bots_in_that_lifecycle() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    repo.fixture_set_bot_lifecycle(bot.id, BotLifecycle::Archived)
        .unwrap();
    let (worker, client) = worker(repo, authority);
    let list = |lifecycle| {
        client.bots(
            context(true),
            BotCommand::ListLifecycle {
                workspace: bot.home_workspace,
                lifecycle,
            },
        )
    };
    let Ok(BotResponse::List { bots }) = list(BotLifecycle::Archived) else {
        panic!("archived listing")
    };
    assert_eq!(bots.len(), 1);
    assert_eq!(bots[0].id, bot.id);
    let Ok(BotResponse::List { bots }) = list(BotLifecycle::Trashed) else {
        panic!("trash listing")
    };
    assert!(bots.is_empty());
    assert_eq!(
        list(BotLifecycle::Active).unwrap_err().code,
        "invalid_request"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
