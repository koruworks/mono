// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::ReasoningLevel;

fn effort() -> ReasoningControl {
    ReasoningControl::Levels {
        values: vec![ReasoningLevel::Low, ReasoningLevel::High],
        default: Some(ReasoningLevel::Low),
        supports_off: false,
    }
}
fn settings_proof(
    base: &CommandContext,
    chat: Chat,
    model: ModelSelection,
    mode: Option<String>,
    fast: Option<bool>,
) -> CommandContext {
    let old = base.prepared.as_ref().unwrap();
    CommandContext {
        request: base.request.clone(),
        prepared: Some(Arc::new(PreparedBotCommand {
            authority: old.authority.clone(),
            chats: old.chats.clone(),
            runtime: Ok(()),
            pins: old.pins.clone(),
            chat_snapshot: Some(chat),
            settings: Some(crate::worker::bots::chat::SettingsProof {
                model,
                mode,
                fast,
                reasoning_control: Some(effort()),
            }),
        })),
    }
}
fn update_model(
    client: &AiWorkerClient,
    context: CommandContext,
    chat: ChatId,
    model: ModelSelection,
) -> ApiResult<UpdateChatModelResponse> {
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::UpdateChatModel {
            context,
            chat,
            request: UpdateChatModelRequest { model },
            reply,
        })
        .unwrap();
    response.recv_timeout(Duration::from_secs(5)).unwrap()
}
fn read_bot(client: &AiWorkerClient, context: &CommandContext) -> Bot {
    let bot_id = context.prepared.as_ref().unwrap().chats[0].0;
    let (reply, response) = sync_channel(1);
    client
        .send(WorkerCommand::Bots {
            context: context.clone(),
            request: BotCommand::Read { bot_id },
            reply,
        })
        .unwrap();
    let BotResponse::Bot { bot } = response
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
    else {
        panic!("Bot read")
    };
    bot
}

#[test]
fn validated_settings_return_effort_without_cache_and_only_change_the_next_task() {
    let native = Arc::new(Interactive::new());
    let (worker, client, _, context, _) = start(native.clone());
    let before = read_bot(&client, &context);
    let chat = context
        .prepared
        .as_ref()
        .unwrap()
        .chat_snapshot
        .clone()
        .unwrap();
    let original = native
        .native
        .state
        .lock()
        .unwrap()
        .submission
        .clone()
        .unwrap();
    let original_input = native.native.state.lock().unwrap().input.clone();
    let mut model = chat.current_model.clone();
    model.requested_model = "fixture-next".into();
    // No model-list request has populated the writer's ordinary cache.
    let response = update_model(
        &client,
        settings_proof(&context, chat.clone(), model.clone(), None, None),
        chat.id,
        model.clone(),
    )
    .unwrap();
    assert_eq!(response.reasoning_control, Some(effort()));
    assert_eq!(
        response.chat.current_model.default_reasoning,
        ReasoningChoice::Default
    );
    let mut no_control = settings_proof(&context, response.chat.clone(), model.clone(), None, None);
    Arc::get_mut(no_control.prepared.as_mut().unwrap())
        .unwrap()
        .settings
        .as_mut()
        .unwrap()
        .reasoning_control = None;
    let response = update_model(&client, no_control, chat.id, model.clone()).unwrap();
    assert_eq!(
        response.reasoning_control, None,
        "validated absence must not invent a cache control"
    );
    let mut missing_proof =
        settings_proof(&context, response.chat.clone(), model.clone(), None, None);
    Arc::get_mut(missing_proof.prepared.as_mut().unwrap())
        .unwrap()
        .settings = None;
    assert!(update_model(&client, missing_proof, chat.id, model.clone()).is_err());
    model.default_reasoning = ReasoningChoice::Level(ReasoningLevel::High);
    let response = update_model(
        &client,
        settings_proof(&context, response.chat.clone(), model.clone(), None, None),
        chat.id,
        model.clone(),
    )
    .unwrap();
    assert_eq!(response.reasoning_control, Some(effort()));
    let (reply, received) = sync_channel(1);
    client
        .send(WorkerCommand::UpdateChatAgentSettings {
            context: settings_proof(
                &context,
                response.chat.clone(),
                model.clone(),
                Some("safe".into()),
                Some(true),
            ),
            chat: chat.id,
            request: koru_ai_wire::UpdateChatAgentSettingsRequest {
                mode: Some("safe".into()),
                fast: Some(true),
            },
            reply,
        })
        .unwrap();
    let changed = received
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .chat;
    assert_eq!(changed.agent_mode.as_deref(), Some("safe"));
    assert_eq!(changed.agent_fast, Some(true));
    assert_eq!(
        native.native.state.lock().unwrap().submission.as_ref(),
        Some(&original)
    );
    assert_eq!(native.native.state.lock().unwrap().input, original_input);
    assert_eq!(native.native.state.lock().unwrap().begin, 1);
    assert_eq!(read_bot(&client, &context).config, before.config);
    native.publish.store(true, Ordering::SeqCst);
    native.complete.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !native.native.state.lock().unwrap().delivered {
        assert!(Instant::now() < deadline, "first task did not settle");
        std::thread::yield_now();
    }
    let (reply, received) = sync_channel(1);
    client
        .send(WorkerCommand::SendTurn {
            context: settings_proof(
                &context,
                changed,
                model.clone(),
                Some("safe".into()),
                Some(true),
            ),
            request: request(&before),
            reply,
        })
        .unwrap();
    assert!(
        received
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_ok()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = native.native.state.lock().unwrap();
        if state.begin == 2 {
            let input = &state.submission.as_ref().unwrap().input;
            assert_eq!(
                input.model.as_ref().map(|value| value.as_str()),
                Some("fixture-next")
            );
            assert_eq!(
                input.mode.as_ref().map(|value| value.as_str()),
                Some("safe")
            );
            assert_eq!(input.fast, Some(true));
            assert!(input.reasoning.is_some());
            break;
        }
        drop(state);
        assert!(Instant::now() < deadline, "next task did not begin");
        std::thread::yield_now();
    }
    worker.shutdown();
    let _ = worker.join();
}

#[test]
fn settings_refuse_missing_runtime_foreign_agent_and_stale_chat_or_bot_evidence() {
    let native = Arc::new(Interactive::new());
    let (worker, client, _, context, _) = start(native.clone());
    let bot = read_bot(&client, &context);
    let chat = context
        .prepared
        .as_ref()
        .unwrap()
        .chat_snapshot
        .clone()
        .unwrap();
    let mut model = chat.current_model.clone();
    model.requested_model = "fixture-next".into();
    let error = client
        .update_chat_model(
            super::super::super::context(true),
            chat.id,
            UpdateChatModelRequest {
                model: model.clone(),
            },
        )
        .unwrap_err();
    assert_eq!(error.code, "runtime_unavailable");
    let old_proof = settings_proof(&context, chat.clone(), model.clone(), None, None);
    let mut foreign = model.clone();
    foreign.provider = AiProviderId::parse("agent-claude").unwrap();
    assert!(update_model(&client, old_proof.clone(), chat.id, foreign).is_err());
    let updated = update_model(&client, old_proof.clone(), chat.id, model.clone()).unwrap();
    assert!(update_model(&client, old_proof, chat.id, model.clone()).is_err());
    let fresh = settings_proof(&context, updated.chat, model.clone(), None, None);
    let mut config = bot.config.clone();
    config.name = "Changed while settings were prepared".into();
    let (reply, received) = sync_channel(1);
    client
        .send(WorkerCommand::Bots {
            context: context.clone(),
            request: BotCommand::Update {
                request_id: BotRequestId::mint(),
                bot_id: bot.id,
                expected_revision: bot.revision,
                config,
            },
            reply,
        })
        .unwrap();
    assert!(
        received
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_ok()
    );
    assert!(update_model(&client, fresh, chat.id, model).is_err());
    assert_eq!(native.native.state.lock().unwrap().begin, 1);
    worker.shutdown();
    let _ = worker.join();
}
