// SPDX-License-Identifier: MPL-2.0
use crate::worker::*;
use koru_ai_core::bots::*;

#[test]
fn staged_memory_commands_refuse_before_any_authority_or_store_work() {
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-memory-contract-fixture"),
        "internal-debug",
        ProviderRegistry::default(),
    );
    config.test_repository = Some(Repository::in_memory().unwrap());
    // No Bot authority, delegate, provider or Store is installed: the closed
    // capability must be answered before looking up any of them or the Bot.
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    let bot_id = BotId::mint();
    let creation_request = BotRequestId::mint();
    let commands = vec![
        BotCommand::ReadMemory { bot_id },
        BotCommand::EnsureMemory {
            request_id: creation_request,
            bot_id,
            expected_revision: 1,
            title: "Memory".into(),
        },
        BotCommand::RetryMemory {
            bot_id,
            creation_request,
            observed_state: BotMemoryState::Unconfirmed,
            observed_updated_at_ms: 1,
        },
        BotCommand::AbandonMemory {
            bot_id,
            creation_request,
            observed_state: BotMemoryState::Prepared,
            observed_updated_at_ms: 1,
        },
        BotCommand::ListMemory {
            bot_id,
            cursor: None,
            limit: 20,
        },
        BotCommand::ReadMemoryFact {
            bot_id,
            fact_id: "12345678-1234-5123-8123-123456789abc".parse().unwrap(),
            cursor: None,
        },
    ];
    for command in commands {
        let context = RequestContext {
            principal: koru_ai_api::AiPrincipal {
                label: "fixture".into(),
                scopes: vec![AiScope::Chat, AiScope::ManageBots],
            },
            trace_id: "memory-contract".into(),
            deadline_millis: None,
        };
        assert_eq!(
            client.bots(context, command).unwrap_err().code,
            "unsupported_capability"
        );
    }
    let denied = RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "fixture".into(),
            scopes: vec![AiScope::Chat],
        },
        trace_id: "memory-contract-denied".into(),
        deadline_millis: None,
    };
    assert_ne!(
        client
            .bots(denied, BotCommand::ReadMemory { bot_id })
            .unwrap_err()
            .code,
        "unsupported_capability"
    );
    let workspace = koru_ai_core::WorkspaceId::new();
    let request = super::RunMemoryReadRequest {
        run: RunId::mint(),
        generation: koru_delegate_core::AttemptGeneration::FIRST,
        provenance: koru_delegate_core::BotProvenance {
            routine: None,
            owner: koru_delegate_core::BotOwner {
                user_scope: UserScopeId::new(),
                device: DeviceId::new(),
            },
            bot: bot_id,
            submission: creation_request,
            chat_id: Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        },
        captured: koru_delegate_core::CapturedBotPolicy {
            workspace,
            access: koru_delegate_core::BotAccess::Default,
            selected_pages: Default::default(),
            destination: workspace,
        },
        cancellation: Cancellation::default(),
        operation: MemoryReadOperation::List {
            view: MemoryView::Captured,
            cursor: None,
            limit: 20,
        },
    };
    assert_eq!(
        client.read_bot_memory(request).unwrap_err().code,
        "unsupported_capability"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
