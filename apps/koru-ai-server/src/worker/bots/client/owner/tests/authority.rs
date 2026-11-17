// SPDX-License-Identifier: MPL-2.0
//! Cohort `m8_owner_authority`: what a linked device is refused, over
//! `TransportSession::pair()`, and that a refusal changes nothing.
use koru_ai_core::bots::{BotConfig, BotId};
use koru_bot_client_wire::{Outcome, Query, Watch, capability, refusal};

use super::super::command::{
    ALLOWED_KINDS, EXECUTING_COMPUTER_ONLY, local_bot_command, local_read, outside_allow_list,
};
use super::*;

fn update(bot: &Bot, request: Uuid, config: BotConfig) -> ClientCommand {
    ClientCommand::UpdateBot {
        request_id: BotRequestId::from_uuid(request),
        bot_id: bot.id,
        expected_revision: bot.revision,
        config,
    }
}

/// A hello whose `client_device` is not the device the carrier
/// authenticated is refused `principal_mismatch`, and a device the
/// admission port does not name is refused `not_own_device`. Neither is
/// told anything of the feed.
#[test]
fn m8_owner_authority_a_hello_for_another_device_and_an_unnamed_device_are_refused() {
    let world = World::new();
    let device = world.device();
    let other = DeviceId::from_uuid(Uuid::new_v4());
    for role in [Role::Feed, Role::Command, Role::Live] {
        assert_eq!(
            world.refused(device, hello(other, role), 1),
            refusal::PRINCIPAL_MISMATCH
        );
    }
    // The gateway's adapter refuses this one first; the engine refuses it
    // again if it ever gets here.
    assert_eq!(
        world.refused(other, hello(other, Role::Command), 1),
        refusal::NOT_OWN_DEVICE
    );
    // A hello with no common version is told this build's range.
    let mut newer = hello(device, Role::Feed);
    (newer.min_version, newer.max_version) = (2, 3);
    assert_eq!(
        world.refused(device, newer, 1),
        refusal::UNSUPPORTED_VERSION
    );
    assert!(receipts(&world).is_empty());
}

/// A message that names another computer ends the stream with
/// `wrong_owner`, in each role, and nothing of it is recorded or applied.
#[test]
fn m8_owner_authority_a_message_that_names_another_computer_is_refused() {
    let world = World::new();
    let device = world.device();
    let elsewhere = DeviceId::from_uuid(Uuid::new_v4());
    let request = Uuid::new_v4();

    let (mut commands, _) = world.open(device, Role::Command);
    commands.owner = elsewhere;
    world.runtime.block_on(async {
        commands
            .command(request, pause(&world.bot, request, world.bot.revision))
            .await;
        assert_eq!(commands.refusal().await, refusal::WRONG_OWNER);
    });

    let (mut queries, _) = world.open(device, Role::Command);
    world.runtime.block_on(async {
        queries
            .write(WireMessage::Query(Query {
                query_id: "q".into(),
                owner_device: elsewhere,
                query: koru_bot_client_wire::ClientQuery::ReadOperation {
                    request_id: BotRequestId::mint(),
                    bot_id: world.bot.id,
                },
            }))
            .await;
        assert_eq!(queries.refusal().await, refusal::WRONG_OWNER);
    });

    let (mut feed, _) = world.open(device, Role::Feed);
    feed.owner = elsewhere;
    world.runtime.block_on(async {
        feed.subscribe(None, None, None).await;
        assert_eq!(feed.refusal().await, refusal::WRONG_OWNER);
    });

    let (mut live, _) = world.open(device, Role::Live);
    world.runtime.block_on(async {
        live.write(WireMessage::Watch(Watch {
            owner_device: elsewhere,
            chat_id: world.bot.main_chat,
        }))
        .await;
        assert_eq!(live.refusal().await, refusal::WRONG_OWNER);
    });

    assert!(receipts(&world).is_empty());
    assert!(world.local.applied().is_empty());
    assert!(!world.read_bot().paused);
}

/// A message of another role, a message the owner sends, and a second hello
/// each end the stream with `protocol_error`. A command sent on a feed
/// stream is never applied.
#[test]
fn m8_owner_authority_a_message_in_the_wrong_role_is_a_protocol_error() {
    let world = World::new();
    let device = world.device();
    let request = Uuid::new_v4();

    let (mut feed, _) = world.open(device, Role::Feed);
    world.runtime.block_on(async {
        feed.command(request, pause(&world.bot, request, world.bot.revision))
            .await;
        assert_eq!(feed.refusal().await, refusal::PROTOCOL_ERROR);
    });

    let (mut commands, _) = world.open(device, Role::Command);
    world.runtime.block_on(async {
        commands.subscribe(None, None, None).await;
        assert_eq!(commands.refusal().await, refusal::PROTOCOL_ERROR);
    });

    let (mut live, _) = world.open(device, Role::Live);
    world.runtime.block_on(async {
        live.write(WireMessage::Keepalive(Default::default())).await;
        assert_eq!(live.refusal().await, refusal::PROTOCOL_ERROR);
    });

    let (mut again, _) = world.open(device, Role::Command);
    world.runtime.block_on(async {
        again
            .write(WireMessage::ClientHello(hello(device, Role::Command)))
            .await;
        assert_eq!(again.refusal().await, refusal::PROTOCOL_ERROR);
    });

    // A frame that is not a message at all.
    let (mut garbled, _) = world.open(device, Role::Command);
    world.runtime.block_on(async {
        garbled
            .write_json(serde_json::json!({"type": "delete_everything"}))
            .await;
        assert_eq!(garbled.refusal().await, refusal::PROTOCOL_ERROR);
    });

    assert!(receipts(&world).is_empty());
    assert!(world.local.applied().is_empty());
    assert!(!world.read_bot().paused);
}

/// A command outside the allow-list is answered `executing_computer_only`
/// with a receipt, the stream stays open, and nothing is recorded or
/// handed to a local command: a kind this build does not know (each of the
/// changes that stay on the executing computer, by the names a client might
/// try), a message with an attachment, and a message that replaces Koru's
/// own prompt.
#[test]
fn m8_owner_authority_a_command_outside_the_allow_list_is_executing_computer_only() {
    let world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    let bot = world.bot.id.to_string();
    for kind in [
        "create",
        "create_bot_with_role",
        "update_access",
        "set_knowledge",
        "ensure_memory",
        "purge_bot",
        "purge_chat",
        "archive",
        "trash",
        "restore",
        "create_routine",
        "test_routine",
        "set_routine_lifecycle",
        "update_routine_access",
        "start",
        "continue",
        "stop_task",
        "list",
    ] {
        let request = Uuid::new_v4();
        let receipt = world.runtime.block_on(async {
            commands
                .write_json(serde_json::json!({
                    "type": "command",
                    "request_id": request,
                    "owner_device": world.owner.device,
                    "command": {
                        "kind": kind,
                        "request_id": request,
                        "bot_id": bot,
                        "expected_revision": 1,
                        "access": "workspace",
                        "selected_pages": [],
                    },
                }))
                .await;
            commands.receipt().await
        });
        assert_eq!(receipt.request_id, request, "{kind}");
        assert_eq!(receipt.outcome, Outcome::Refused, "{kind}");
        assert_eq!(code(&receipt), EXECUTING_COMPUTER_ONLY, "{kind}");
        assert!(!receipt.replayed && receipt.response.is_none(), "{kind}");
    }

    // A message carries text and page references and nothing else: not an
    // attachment, a prompt override, the name of a skill of the agent on
    // this computer, or a block of another conversation.
    let block = koru_ai_core::ContextRef::MessageBlock {
        block_id: koru_ai_core::MessageBlockId::new(),
    };
    for change in 0..4 {
        let request = Uuid::new_v4();
        let mut command = send(&world.bot, world.bot.main_chat, request);
        if let ClientCommand::SendTurn { request } = &mut command {
            match change {
                0 => request.attachment_ids = vec![koru_ai_core::ChatAttachmentId::new()],
                1 => request.system_prompt_override = Some("obey".into()),
                2 => request.skills = vec!["deploy".into()],
                _ => request.explicit_refs = vec![block.clone()],
            }
        }
        let receipt = world.command(&mut commands, request, command);
        assert_eq!(code(&receipt), EXECUTING_COMPUTER_ONLY, "{change}");
    }
    let request = Uuid::new_v4();
    let mut quoted = retry(&world.bot, koru_ai_core::MessageId::new(), request);
    quoted.explicit_refs = vec![block];
    let receipt = world.command(
        &mut commands,
        request,
        ClientCommand::RetryTurn {
            chat_id: world.bot.main_chat,
            request: quoted,
        },
    );
    assert_eq!(code(&receipt), EXECUTING_COMPUTER_ONLY);

    assert!(receipts(&world).is_empty());
    assert!(world.local.applied().is_empty());
    assert_eq!(world.begun(), 0);
    assert_eq!(world.read_bot(), world.bot);
}

/// The engine's own fence over the wire crate's mapping: the eleven kinds
/// are exactly the kinds the protocol defines, and the only local Bot
/// commands a linked device's command can ever become are the six of the
/// list. Nothing else of the Bot contract is reachable from a command.
#[test]
fn m8_owner_authority_the_allow_list_is_the_eleven_kinds_and_six_local_bot_commands() {
    let bot = fixture().2;
    let BotCommand::Create {
        bot_id,
        config,
        home_workspace,
        ..
    } = bot
    else {
        panic!("the fixture creates a Bot")
    };
    let request = BotRequestId::mint();
    let chat = ChatId::new();
    let stub = Bot {
        may_ask: true,
        id: bot_id,
        home_workspace,
        user_scope: koru_ai_core::bots::UserScopeId::new(),
        owner_device: DeviceId::from_uuid(Uuid::new_v4()),
        config: config.clone(),
        access: koru_ai_core::bots::BotAccess::Default,
        selected_pages: vec![],
        revision: 1,
        policy_revision: 1,
        main_chat: chat,
        paused: false,
        folder_lock_request: None,
        folder_frozen_at_ms: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        lifecycle: Default::default(),
        open_operation: None,
        held_by: None,
        pending_change: None,
        connections: Default::default(),
    };
    let every = vec![
        send(&stub, chat, Uuid::new_v4()),
        ClientCommand::RetryTurn {
            chat_id: chat,
            request: retry(&stub, koru_ai_core::MessageId::new(), Uuid::new_v4()),
        },
        ClientCommand::CancelAttempt {
            attempt_id: koru_ai_core::InferenceAttemptId::new(),
        },
        ClientCommand::Steer {
            attempt_id: koru_ai_core::InferenceAttemptId::new(),
            request: koru_ai_wire::SteerAttemptRequest {
                operation_id: Uuid::new_v4(),
                markdown: "more".into(),
            },
        },
        ClientCommand::CreateChat {
            request_id: request,
            bot_id,
        },
        pause(&stub, request.as_uuid(), 1),
        ClientCommand::ResumeBot {
            request_id: request,
            bot_id,
            expected_revision: 1,
        },
        ClientCommand::PauseTeam {
            request_id: request,
            expected_team_revision: 0,
        },
        ClientCommand::ResumeTeam {
            request_id: request,
            expected_team_revision: 0,
        },
        ClientCommand::AnswerDecision {
            tool_id: koru_ai_core::ToolInvocationId::new(),
            approve: true,
            decision: Default::default(),
        },
        update(&stub, request.as_uuid(), config),
    ];
    let kinds: Vec<&str> = every.iter().map(ClientCommand::kind).collect();
    assert_eq!(kinds, ALLOWED_KINDS);
    assert!(every.iter().all(|command| !outside_allow_list(command)));
    assert!(outside_allow_list(&ClientCommand::Unsupported));
    assert_eq!(local_bot_command(&ClientCommand::Unsupported), None);

    let local: Vec<Option<BotCommand>> = every.iter().map(local_bot_command).collect();
    let names: Vec<&str> = local
        .iter()
        .map(|local| match local {
            None => "none",
            Some(BotCommand::CreateChat { .. }) => "create_chat",
            Some(BotCommand::Pause { .. }) => "pause",
            Some(BotCommand::Resume { .. }) => "resume",
            Some(BotCommand::PauseTeam { .. }) => "pause_team",
            Some(BotCommand::ResumeTeam { .. }) => "resume_team",
            Some(BotCommand::Update { .. }) => "update",
            Some(_) => "something else",
        })
        .collect();
    assert_eq!(
        names,
        [
            "none",
            "none",
            "none",
            "none",
            "create_chat",
            "pause",
            "resume",
            "pause_team",
            "resume_team",
            "none",
            "update",
        ]
    );
    // The two reads, and nothing for a kind this build does not know.
    assert!(matches!(
        local_read(&koru_bot_client_wire::ClientQuery::ReadOperation {
            request_id: request,
            bot_id,
        }),
        Some(BotCommand::ReadOperation { .. })
    ));
    assert_eq!(
        local_read(&koru_bot_client_wire::ClientQuery::Unsupported),
        None
    );
    let _: BotId = bot_id;
}

/// `update_bot` may change the name, the model's reasoning, the mode, Fast
/// and the face. A change of the working folder or of the Role page is
/// `executing_computer_only`, and the Bot is exactly as it was.
#[test]
fn m8_owner_authority_update_bot_may_not_change_the_folder_or_the_role_page() {
    let world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);

    let mut folder = world.bot.config.clone();
    folder.working_folder = if cfg!(windows) {
        "D:/somewhere-else"
    } else {
        "/somewhere-else"
    }
    .into();
    let mut role = world.bot.config.clone();
    role.role.document = DocumentId::new();
    let mut both = folder.clone();
    both.name = "Renamed too".into();
    for config in [folder, role, both] {
        let request = Uuid::new_v4();
        let receipt = world.command(&mut commands, request, update(&world.bot, request, config));
        assert_eq!(receipt.outcome, Outcome::Refused);
        assert_eq!(code(&receipt), EXECUTING_COMPUTER_ONLY);
        assert_eq!(receipt.feed_cursor, None);
    }
    assert_eq!(world.read_bot(), world.bot);
    // The three were recorded and closed as refused; no local command ran.
    assert_eq!(
        receipts(&world),
        vec![row("update_bot", "done", Some("refused")); 3]
    );
    assert!(world.local.applied().is_empty());

    // What it may change.
    let mut renamed = world.bot.config.clone();
    renamed.name = "Renamed from the laptop".into();
    renamed.avatar = Some(3);
    let request = Uuid::new_v4();
    let receipt = world.command(
        &mut commands,
        request,
        update(&world.bot, request, renamed.clone()),
    );
    assert_eq!(receipt.outcome, Outcome::Completed, "{receipt:?}");
    assert!(!receipt.replayed);
    let after = world.read_bot();
    assert_eq!(after.config, renamed);
    assert_eq!(after.revision, world.bot.revision + 1);
    assert_eq!(after.config.working_folder, world.bot.config.working_folder);
    assert_eq!(after.config.role, world.bot.config.role);
    // The response is the local command's own public JSON.
    let answered: BotResponse = receipt.response.unwrap().parse().unwrap();
    assert!(matches!(answered, BotResponse::Bot { bot } if bot.config == renamed));
    let head = world.on_writer(|state| state.repository.feed_head().unwrap());
    assert_eq!(receipt.feed_cursor, Some(head));

    // A command at a revision that is no longer the Bot's is answered what
    // the local command answers, before anything is compared: a stale copy
    // cannot put an old folder back.
    let mut stale = world.bot.config.clone();
    stale.working_folder = "D:/an-older-folder".into();
    let request = Uuid::new_v4();
    let receipt = world.command(&mut commands, request, update(&world.bot, request, stale));
    assert_eq!(code(&receipt), "revision_conflict");
    assert_eq!(world.read_bot(), after);
    audit_refused_nothing(&world);
}

/// A device that holds `read` alone is served the feed and no command
/// stream; one without `configure` cannot send `update_bot`; and a device
/// the port stops naming is refused on its next message, on a stream that
/// was already open.
#[test]
fn m8_owner_authority_a_stream_is_held_to_the_admission_ports_scopes() {
    let world = World::new();
    let reader = DeviceId::from_uuid(Uuid::new_v4());
    world.port.name(reader, &[scope::READ]);
    let (_, answer) = world.open(reader, Role::Feed);
    assert_eq!(answer.scopes, vec![scope::READ.to_owned()]);
    assert_eq!(
        answer.capabilities,
        vec![
            capability::FEED.to_owned(),
            capability::QUERY_RUN.to_owned(),
            capability::LIVE_AI_STREAM.to_owned(),
        ]
    );
    // Its command stream carries the two reads and no command.
    let (mut reads, _) = world.open(reader, Role::Command);
    let request = Uuid::new_v4();
    world.runtime.block_on(async {
        reads
            .command(request, pause(&world.bot, request, world.bot.revision))
            .await;
        assert_eq!(reads.refusal().await, refusal::UNSUPPORTED_MESSAGE);
    });

    let controller = DeviceId::from_uuid(Uuid::new_v4());
    world.port.name(controller, &[scope::READ, scope::CONTROL]);
    let (mut commands, answer) = world.open(controller, Role::Command);
    assert!(
        !answer
            .capabilities
            .contains(&capability::CMD_CONFIG.to_owned())
    );
    let request = Uuid::new_v4();
    world.runtime.block_on(async {
        let mut renamed = world.bot.config.clone();
        renamed.name = "Renamed".into();
        commands
            .command(request, update(&world.bot, request, renamed))
            .await;
        assert_eq!(commands.refusal().await, refusal::UNSUPPORTED_MESSAGE);
    });

    // Unlinked while its stream is open: the next command is refused, and
    // a scope taken away is gone for the stream that negotiated it.
    let device = world.device();
    let (mut open, _) = world.open(device, Role::Command);
    world.port.forget(device);
    let request = Uuid::new_v4();
    world.runtime.block_on(async {
        open.command(request, pause(&world.bot, request, world.bot.revision))
            .await;
        assert_eq!(open.refusal().await, refusal::NOT_OWN_DEVICE);
    });
    let device = world.device();
    let (mut narrowed, _) = world.open(device, Role::Command);
    world.port.name(device, &[scope::READ]);
    let request = Uuid::new_v4();
    world.runtime.block_on(async {
        narrowed
            .command(request, pause(&world.bot, request, world.bot.revision))
            .await;
        assert_eq!(narrowed.refusal().await, refusal::UNSUPPORTED_MESSAGE);
    });

    assert!(receipts(&world).is_empty());
    assert!(world.local.applied().is_empty());
    assert_eq!(world.read_bot(), world.bot);
}

/// A command that names something a copy does not show is `not_found`, and
/// no local command runs: a Bot whose workspace is kept on this computer,
/// an ordinary chat of the person, a Bot that does not exist, and a reply
/// or a pending call nothing has.
#[test]
fn m8_owner_authority_a_command_that_names_what_a_copy_does_not_show_is_not_found() {
    let world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);

    // A conversation that is not a Bot's. The repository's half of this
    // cohort shows that an ordinary chat of the person, a hidden
    // conversation and an id nothing has all read the same there.
    let ordinary = ChatId::new();
    let request = Uuid::new_v4();
    let receipt = world.command(&mut commands, request, send(&world.bot, ordinary, request));
    assert_eq!(code(&receipt), "not_found");

    // A Bot nothing has, and a reply and a pending call nothing has.
    let mut absent = world.bot.clone();
    absent.id = koru_ai_core::bots::BotId::mint();
    let request = Uuid::new_v4();
    let receipt = world.command(&mut commands, request, pause(&absent, request, 1));
    assert_eq!(code(&receipt), "not_found");
    let request = Uuid::new_v4();
    let receipt = world.command(
        &mut commands,
        request,
        ClientCommand::CancelAttempt {
            attempt_id: koru_ai_core::InferenceAttemptId::new(),
        },
    );
    assert_eq!(code(&receipt), "not_found");
    let request = Uuid::new_v4();
    let receipt = world.command(
        &mut commands,
        request,
        ClientCommand::AnswerDecision {
            tool_id: koru_ai_core::ToolInvocationId::new(),
            approve: true,
            decision: Default::default(),
        },
    );
    assert_eq!(code(&receipt), "not_found");

    // The Bot's workspace is kept on this computer: every command that
    // names the Bot, its conversation or a change of it is `not_found`,
    // exactly as for the Bot that does not exist.
    world
        .authority
        .keep_on_this_computer(world.bot.home_workspace, true);
    let mut renamed = world.bot.config.clone();
    renamed.working_folder = "D:/somewhere-else".into();
    let withheld = [
        pause(&world.bot, Uuid::new_v4(), world.bot.revision),
        send(&world.bot, world.bot.main_chat, Uuid::new_v4()),
        ClientCommand::CreateChat {
            request_id: BotRequestId::mint(),
            bot_id: world.bot.id,
        },
        update(&world.bot, Uuid::new_v4(), renamed),
    ];
    for command in withheld {
        let request = command.embedded_request_id().unwrap();
        let receipt = world.command(&mut commands, request, command);
        assert_eq!(receipt.outcome, Outcome::Refused);
        assert_eq!(code(&receipt), "not_found", "{receipt:?}");
    }
    // A read of it, too.
    let answer = world.runtime.block_on(async {
        commands
            .write(WireMessage::Query(Query {
                query_id: "q-1".into(),
                owner_device: world.owner.device,
                query: koru_bot_client_wire::ClientQuery::ReadOperation {
                    request_id: BotRequestId::mint(),
                    bot_id: world.bot.id,
                },
            }))
            .await;
        match commands.next().await {
            Some(WireMessage::Answer(answer)) => answer,
            other => panic!("expected an answer, read {other:?}"),
        }
    });
    assert_eq!(answer.query_id, "q-1");
    assert_eq!(answer.failure.unwrap().code, "not_found");

    assert!(world.local.applied().is_empty());
    assert_eq!(world.begun(), 0);
    assert_eq!(world.read_bot(), world.bot);
    // The team is not a Bot: a team pause is still taken.
    let request = Uuid::new_v4();
    let receipt = world.command(
        &mut commands,
        request,
        ClientCommand::PauseTeam {
            request_id: BotRequestId::from_uuid(request),
            expected_team_revision: 0,
        },
    );
    assert!(
        matches!(receipt.outcome, Outcome::Accepted | Outcome::Completed),
        "{receipt:?}"
    );
}

/// **The person's ordinary AI chats are out of a linked device's reach.** An
/// ordinary chat with a finished reply, written as this computer writes
/// one: a message into it, a retry of its reply, a stop or a steer of its
/// attempt, a watch and a Run read through it are all `not_found`, exactly
/// as for a chat that does not exist; the feed never mentions it; and it is
/// as it was.
#[test]
fn m8_owner_authority_an_ordinary_chat_of_the_person_is_out_of_reach() {
    let world = World::new();
    let device = world.device();
    let chat = ChatId::new();
    world
        .client()
        .create_chat(
            local_owner(),
            koru_ai_wire::CreateChatRequest {
                chat_id: chat,
                title: "ORDINARY-TITLE".into(),
                profile_id: None,
                model: koru_ai_core::ModelSelection {
                    provider: koru_ai_core::AiProviderId::parse("mock").unwrap(),
                    requested_model: "deterministic-v1".into(),
                    default_reasoning: koru_ai_core::ReasoningChoice::Default,
                },
            },
        )
        .unwrap();
    let ClientCommand::SendTurn {
        request: mut ordinary,
    } = send(&world.bot, chat, Uuid::new_v4())
    else {
        unreachable!()
    };
    ordinary.destination_workspace = None;
    ordinary.markdown = "ORDINARY-QUESTION".into();
    let stream = world.client().send_turn(local_owner(), ordinary).unwrap();
    loop {
        let event = stream.events.recv_timeout(Duration::from_secs(10)).unwrap();
        if event.event.is_terminal() {
            break;
        }
    }
    let before = world.client().read_chat(local_owner(), chat).unwrap();
    assert_eq!(before.messages.len(), 2);

    let (mut commands, _) = world.open(device, Role::Command);
    let attempt = stream.accepted.attempt_id;
    let tries = [
        send(&world.bot, chat, Uuid::new_v4()),
        ClientCommand::RetryTurn {
            chat_id: chat,
            request: retry(
                &world.bot,
                stream.accepted.assistant_message_id,
                Uuid::new_v4(),
            ),
        },
        ClientCommand::CancelAttempt {
            attempt_id: attempt,
        },
        ClientCommand::Steer {
            attempt_id: attempt,
            request: koru_ai_wire::SteerAttemptRequest {
                operation_id: Uuid::new_v4(),
                markdown: "more".into(),
            },
        },
    ];
    for command in tries {
        let request = command.embedded_request_id().unwrap_or_else(Uuid::new_v4);
        let receipt = world.command(&mut commands, request, command);
        assert_eq!(receipt.outcome, Outcome::Refused);
        assert_eq!(code(&receipt), "not_found", "{receipt:?}");
    }
    let answer = world.query(
        &mut commands,
        koru_bot_client_wire::ClientQuery::ReadRun {
            bot_id: world.bot.id,
            chat_id: chat,
            message_id: Some(stream.accepted.assistant_message_id),
            run_id: koru_ai_core::bots::RunId::mint(),
            from: 0,
            len: 10,
        },
    );
    assert_eq!(answer.failure.unwrap().code, "not_found");
    assert!(answer.response.is_none());

    let (mut live, _) = world.open(device, Role::Live);
    world.runtime.block_on(async {
        live.write(WireMessage::Watch(Watch {
            owner_device: world.owner.device,
            chat_id: chat,
        }))
        .await;
        match live.next().await {
            Some(WireMessage::LiveEnd(end)) => {
                assert_eq!(end.reason, koru_bot_client_wire::LiveEndReason::NotFound)
            }
            other => panic!("expected the end of the watch, read {other:?}"),
        }
    });

    // The feed holds the Bot and its conversation, and nothing of the chat.
    let (mut feed, _) = world.open(device, Role::Feed);
    let pages = world.runtime.block_on(async {
        feed.subscribe(None, None, None).await;
        assert!(matches!(feed.next().await, Some(WireMessage::Gap(_))));
        feed.pages().await
    });
    let sent =
        serde_json::to_string(&pages.iter().map(|page| &page.entries).collect::<Vec<_>>()).unwrap();
    assert!(sent.contains(&world.bot.id.to_string()));
    assert!(!sent.contains(&chat.to_string()));
    assert!(!sent.contains("ORDINARY"));

    assert!(world.local.applied().is_empty());
    assert_eq!(
        world.client().read_chat(local_owner(), chat).unwrap(),
        before
    );
}

/// The two reads answer what the local read answers, change nothing and
/// leave no receipt. A read of a kind this build does not know is
/// `executing_computer_only`.
#[test]
fn m8_owner_authority_a_read_answers_what_the_local_read_answers() {
    let world = World::new();
    let device = world.device();
    let (mut commands, _) = world.open(device, Role::Command);
    let request = Uuid::new_v4();
    let paused = world.command(
        &mut commands,
        request,
        pause(&world.bot, request, world.bot.revision),
    );
    assert!(matches!(
        paused.outcome,
        Outcome::Accepted | Outcome::Completed
    ));
    let settled = world.settled_bot();

    let read = BotCommand::ReadOperation {
        request_id: BotRequestId::from_uuid(request),
        bot_id: world.bot.id,
    };
    let local = world.client().bots(local_owner(), read).unwrap();
    let answer = world.query(
        &mut commands,
        koru_bot_client_wire::ClientQuery::ReadOperation {
            request_id: BotRequestId::from_uuid(request),
            bot_id: world.bot.id,
        },
    );
    assert_eq!(answer.query_id, "q");
    assert!(answer.failure.is_none());
    let linked: BotResponse = answer.response.unwrap().parse().unwrap();
    assert_eq!(linked, local);
    assert!(matches!(linked, BotResponse::Operation { operation }
        if operation.request_id.as_uuid() == request));

    let unknown = world.runtime.block_on(async {
        commands
            .write_json(serde_json::json!({
                "type": "query",
                "query_id": "q-2",
                "owner_device": world.owner.device,
                "query": {"kind": "read_memory", "bot_id": world.bot.id.to_string()},
            }))
            .await;
        match commands.next().await {
            Some(WireMessage::Answer(answer)) => answer,
            other => panic!("expected an answer, read {other:?}"),
        }
    });
    assert_eq!(unknown.failure.unwrap().code, EXECUTING_COMPUTER_ONLY);

    assert_eq!(receipts(&world).len(), 1);
    assert_eq!(world.local.applied(), ["pause_bot"]);
    assert_eq!(world.read_bot(), settled);
}
