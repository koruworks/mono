// SPDX-License-Identifier: MPL-2.0
//! The lifecycle contract on the worker: progress is readable by its owner;
//! changes whose cross-owner drivers do not exist yet refuse explicitly. (The
//! Archived/Trash listing is served; see `holds.rs`.)
use super::*;
use koru_ai_sqlite::{LifecycleCapture, LifecycleMemberDraft};

fn worker(repo: Repository, authority: Arc<Authority>) -> (WorkerHandle, AiWorkerClient) {
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-lifecycle"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority);
    config.test_repository = Some(repo);
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    (worker, client)
}

#[test]
fn lifecycle_progress_reads_back_and_changes_refuse_until_built() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let owner = BotOwner {
        user_scope: bot.user_scope,
        device: bot.owner_device,
    };
    let request = BotRequestId::mint();
    repo.capture_bot_lifecycle(&LifecycleCapture {
        operation: request,
        owner,
        bot: bot.id,
        kind: BotLifecycleKind::TrashChat,
        chat: Some(bot.main_chat),
        fingerprint: "a".repeat(64),
        expected_revision: bot.revision,
        source: None,
        members: vec![LifecycleMemberDraft {
            subject: BotLifecycleSubject::Chat {
                chat_id: bot.main_chat,
            },
            reason: BotLifecycleReason::OwnedConversation,
            prior_state: BotLifecyclePriorState::Live,
            intended: BotLifecycleIntended::Hide,
            retained: None,
        }],
        now: 5,
    })
    .unwrap();
    let (worker, client) = worker(repo, authority);
    let BotResponse::Lifecycle { operation } = client
        .bots(
            context(true),
            BotCommand::ReadLifecycleOperation {
                request_id: request,
                bot_id: bot.id,
            },
        )
        .unwrap()
    else {
        panic!("lifecycle progress")
    };
    assert_eq!(operation.kind, BotLifecycleKind::TrashChat);
    assert_eq!(operation.stage, BotLifecycleStage::Captured);
    assert_eq!(
        operation.members[0].subject,
        BotLifecycleSubject::Chat {
            chat_id: bot.main_chat
        }
    );
    // Another Bot's identity, an unknown operation and an ordinary caller see nothing.
    for (context, request_id, bot_id) in [
        (context(true), request, BotId::mint()),
        (context(true), BotRequestId::mint(), bot.id),
    ] {
        assert_eq!(
            client
                .bots(
                    context,
                    BotCommand::ReadLifecycleOperation { request_id, bot_id }
                )
                .unwrap_err()
                .code,
            "not_found"
        );
    }
    assert!(
        client
            .bots(
                context(false),
                BotCommand::ReadLifecycleOperation {
                    request_id: request,
                    bot_id: bot.id,
                },
            )
            .is_err()
    );
    // Archive is built (S4); without its delegate owner it refuses as
    // unavailable before anything is captured.
    assert_eq!(
        client
            .bots(
                context(true),
                BotCommand::Archive {
                    request_id: BotRequestId::mint(),
                    bot_id: bot.id,
                    expected_revision: bot.revision,
                },
            )
            .unwrap_err()
            .code,
        BotError::Unavailable.code()
    );
    for command in [
        BotCommand::Trash {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: bot.revision,
        },
        BotCommand::TrashChat {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            chat_id: bot.main_chat,
            expected_revision: bot.revision,
        },
    ] {
        assert_eq!(
            client.bots(context(true), command).unwrap_err().code,
            "unsupported_capability"
        );
    }
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn an_operations_progress_needs_current_authority_over_its_bots_workspace() {
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    let request = BotRequestId::mint();
    repo.capture_bot_lifecycle(&LifecycleCapture {
        operation: request,
        owner: BotOwner {
            user_scope: bot.user_scope,
            device: bot.owner_device,
        },
        bot: bot.id,
        kind: BotLifecycleKind::Archive,
        chat: None,
        fingerprint: "d".repeat(64),
        expected_revision: bot.revision,
        source: None,
        members: vec![],
        now: 5,
    })
    .unwrap();
    let (worker, client) = worker(repo, authority.clone());
    let read = || {
        client.bots(
            context(true),
            BotCommand::ReadLifecycleOperation {
                request_id: request,
                bot_id: bot.id,
            },
        )
    };
    assert!(matches!(read(), Ok(BotResponse::Lifecycle { .. })));
    // The same owner, after losing the workspace, reads no progress.
    authority.allowed.store(false, Ordering::SeqCst);
    assert!(read().is_err());
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn an_archived_bot_reads_and_every_change_answers_bot_archived_after_authority() {
    use koru_ai_wire::bot_lifecycle_codes::ARCHIVED;
    let (mut repo, authority, command) = fixture();
    let bot = created(&mut repo, authority.clone(), command);
    repo.fixture_set_bot_lifecycle(bot.id, BotLifecycle::Archived)
        .unwrap();
    let (worker, client) = worker(repo, authority.clone());
    // Reads answer as for an active Bot.
    let BotResponse::Bot { bot: read } = client
        .bots(context(true), BotCommand::Read { bot_id: bot.id })
        .unwrap()
    else {
        panic!("Bot")
    };
    assert_eq!(read.id, bot.id);
    assert_eq!(read.lifecycle, BotLifecycle::Archived);
    assert_eq!(read.open_operation, None);
    assert_eq!(
        client
            .bots(
                context(true),
                BotCommand::ReadOperation {
                    request_id: BotRequestId::mint(),
                    bot_id: bot.id,
                },
            )
            .unwrap_err()
            .code,
        "not_found",
        "an archived Bot's receipts are read, and an unknown one is missing"
    );
    assert_eq!(
        client
            .read_chat(context(true), bot.main_chat)
            .unwrap()
            .chat
            .id,
        bot.main_chat
    );
    let task = BotTask {
        text: "CURRENT-TASK".into(),
        documents: vec![],
    };
    let changes = vec![
        BotCommand::CreateChat {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
        },
        BotCommand::Update {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: bot.revision,
            config: bot.config.clone(),
        },
        BotCommand::UpdateAccess {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: bot.revision,
            expected_policy_revision: bot.policy_revision,
            access: BotAccess::Workspace,
            selected_pages: vec![],
        },
        BotCommand::Pause {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: bot.revision,
        },
        BotCommand::Resume {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: bot.revision,
        },
        BotCommand::Start {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            chat_id: bot.main_chat,
            expected_revision: bot.revision,
            destination: bot.home_workspace,
            task: task.clone(),
        },
        BotCommand::Continue {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            chat_id: bot.main_chat,
            expected_revision: bot.revision,
            destination: bot.home_workspace,
            task,
            previous_run: koru_delegate_core::RunId::mint(),
        },
        BotCommand::ReconcileOperation {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
        },
        BotCommand::Archive {
            request_id: BotRequestId::mint(),
            bot_id: bot.id,
            expected_revision: bot.revision,
        },
    ];
    for change in &changes {
        assert_eq!(
            client.bots(context(true), change.clone()).unwrap_err().code,
            ARCHIVED,
            "{change:?}"
        );
    }
    let send = || {
        client.send_turn(
            context(true),
            SendTurnRequest {
                destination_workspace: Some(bot.home_workspace),
                operation_id: Uuid::new_v4(),
                chat_id: bot.main_chat,
                surface: AiSurface::Standalone,
                markdown: "CURRENT-TASK".into(),
                explicit_refs: vec![],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
                attachment_ids: vec![],
                answers: None,
                skills: vec![],
            },
        )
    };
    assert_eq!(send().map(|_| ()).unwrap_err().code, ARCHIVED);
    // Authority first: without the Bot's workspace nothing says "archived".
    authority.allowed.store(false, Ordering::SeqCst);
    for change in changes
        .into_iter()
        .chain([BotCommand::Read { bot_id: bot.id }])
    {
        let code = client.bots(context(true), change.clone()).unwrap_err().code;
        assert_ne!(code, ARCHIVED, "{change:?}");
    }
    assert_ne!(send().map(|_| ()).unwrap_err().code, ARCHIVED);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
