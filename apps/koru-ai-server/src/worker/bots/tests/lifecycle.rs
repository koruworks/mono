// SPDX-License-Identifier: MPL-2.0
//! The lifecycle contract on the worker: progress is readable by its owner;
//! changes refuse explicitly until their cross-owner drivers exist. (The
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
    for command in [
        BotCommand::Archive {
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
