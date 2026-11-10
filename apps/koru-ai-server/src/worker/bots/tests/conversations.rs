// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn create_conversation_checks_current_authority_before_receipt_replay() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let port: Arc<dyn BotAuthority> = authority.clone();
    let request = BotRequestId::mint();
    let BotResponse::Chat { reference } =
        create_bot_chat(&mut repo, &port, &context(true), request, bot.id).unwrap()
    else {
        panic!("Chat reference")
    };
    assert_eq!(
        create_bot_chat(&mut repo, &port, &context(true), request, bot.id).unwrap(),
        BotResponse::Chat {
            reference: reference.clone()
        }
    );
    assert_eq!(
        create_bot_chat(&mut repo, &port, &context(false), request, bot.id)
            .unwrap_err()
            .code,
        "forbidden"
    );
    authority.allowed.store(false, Ordering::SeqCst);
    assert_eq!(
        create_bot_chat(&mut repo, &port, &context(true), request, bot.id)
            .unwrap_err()
            .code,
        "wrong_owner"
    );
    assert_eq!(repo.all_bot_chats().unwrap().len(), 2);
}

#[test]
fn additional_chat_read_and_indirect_attempt_keep_their_actual_target() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let reference = repo
        .create_bot_chat(
            &bot,
            authority.profile,
            BotRequestId::mint(),
            "additional",
            at(2),
        )
        .unwrap();
    let pending = turn(reference.chat_id, Uuid::new_v4(), 3);
    repo.begin_turn(&pending).unwrap();
    let block = MessageBlock {
        id: MessageBlockId::new(),
        message_id: pending.assistant_message.id,
        ordinal: 0,
        kind: koru_ai_core::MessageBlockKind::Paragraph,
        markdown: "Additional context".into(),
        plain_text: "Additional context".into(),
    };
    repo.apply_event(
        &AiEvent::Completed {
            attempt_id: pending.attempt.id,
            stop_reason: None,
            at: at(4),
        },
        &[block.clone()],
    )
    .unwrap();
    assert_eq!(
        repo.bot_chat_for_block(block.id).unwrap(),
        Some((bot.id, reference.chat_id))
    );
    assert!(
        repo.message_block_context_with_access(block.id, &ChatReadAccess::ordinary())
            .unwrap()
            .is_none()
    );
    assert!(
        repo.message_block_context_with_access(
            block.id,
            &ChatReadAccess::authorized(vec![bot.main_chat])
        )
        .unwrap()
        .is_none()
    );
    assert!(
        repo.message_block_context_with_access(
            block.id,
            &ChatReadAccess::authorized(vec![reference.chat_id])
        )
        .unwrap()
        .is_some()
    );
    let (reply, _) = sync_channel(1);
    let selected = WorkerCommand::SendTurn {
        context: context(true).into(),
        reply,
        request: SendTurnRequest {
            destination_workspace: None,
            operation_id: Uuid::new_v4(),
            chat_id: ChatId::new(),
            surface: AiSurface::Standalone,
            markdown: "Use context".into(),
            explicit_refs: vec![ContextRef::MessageBlock { block_id: block.id }],
            web_search: false,
            reasoning_override: None,
            system_prompt_override: None,
            replace_pinned_documents: false,
            attachment_ids: vec![],
            answers: None,
            skills: vec![],
        },
    };
    assert_eq!(
        preparation::bot_targets(&repo, &selected).unwrap(),
        Some(vec![(bot.id, reference.chat_id)])
    );
    let (reply, _) = sync_channel(1);
    let command = WorkerCommand::ReadAttempt {
        context: context(true).into(),
        attempt: pending.attempt.id,
        reply,
    };
    assert_eq!(
        preparation::bot_targets(&repo, &command).unwrap(),
        Some(vec![(bot.id, reference.chat_id)])
    );
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-conversation"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    assert_eq!(
        client
            .read_chat(context(true), reference.chat_id)
            .unwrap()
            .chat
            .id,
        reference.chat_id
    );
    assert_eq!(
        client
            .read_chat(context(false), reference.chat_id)
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        client
            .read_attempt(context(true), pending.attempt.id)
            .unwrap()
            .attempt
            .chat_id,
        Some(reference.chat_id)
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn named_task_conversation_must_already_belong_to_the_bot() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let reference = repo
        .create_bot_chat(
            &bot,
            authority.profile,
            BotRequestId::mint(),
            "additional",
            at(2),
        )
        .unwrap();
    let start = |chat_id| {
        let (reply, _) = sync_channel(1);
        WorkerCommand::Bots {
            context: context(true).into(),
            request: BotCommand::Start {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                chat_id,
                expected_revision: bot.revision,
                destination: bot.home_workspace,
                task: BotTask {
                    text: "task".into(),
                    documents: vec![],
                },
            },
            reply,
        }
    };
    assert_eq!(
        preparation::bot_targets(&repo, &start(reference.chat_id)).unwrap(),
        Some(vec![(bot.id, reference.chat_id)])
    );
    let mut ordinary = repo
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap();
    ordinary.id = ChatId::new();
    repo.create_chat(&ordinary).unwrap();
    for chat in [ordinary.id, ChatId::new()] {
        assert_eq!(
            preparation::bot_targets(&repo, &start(chat))
                .unwrap_err()
                .code,
            "wrong_owner"
        );
    }
}

#[test]
fn worker_create_conversation_needs_no_runtime_and_returns_stable_reference() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-create"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    let worker = AiWorker::new(config).spawn().unwrap();
    let request = BotCommand::CreateChat {
        request_id: BotRequestId::mint(),
        bot_id: bot.id,
    };
    let client = worker.client();
    let first = client.bots(context(true), request.clone()).unwrap();
    assert_eq!(client.bots(context(true), request).unwrap(), first);
    let BotResponse::Chat { reference } = first else {
        panic!("Chat reference")
    };
    assert!(
        client
            .read_chat(context(true), reference.chat_id)
            .unwrap()
            .messages
            .is_empty()
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
