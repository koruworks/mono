// SPDX-License-Identifier: MPL-2.0
//! Cohort `m8_live_role`: the events of a running reply reach a linked
//! device that watches the conversation, a full buffer ends the watch, and
//! nothing of it is written to the feed.
use koru_ai_core::{
    InferenceAttemptId, Timestamp, ToolActivity, ToolActivityStatus, ToolApproval, ToolInvocationId,
};
use koru_ai_wire::PublicAiEvent;
use koru_bot_client_wire::{Live, LiveEnd, LiveEndReason, Outcome, Unwatch, Watch, refusal};

use super::*;
use crate::worker::bots::tests::turn;

async fn watch(live: &Client, chat: ChatId) {
    live.write(WireMessage::Watch(Watch {
        owner_device: live.owner,
        chat_id: chat,
    }))
    .await;
}

/// The watch is registered on the writer once the owner has checked it.
fn watching(world: &World) -> bool {
    world.on_writer(|state| state.bot_clients.live.any())
}

/// A pending reply in the Bot's main conversation, written as a turn is,
/// with nothing running: the events are then published by the test.
fn pending_attempt(world: &World) -> InferenceAttemptId {
    let pending = turn(world.bot.main_chat, Uuid::new_v4(), 1);
    let attempt = pending.attempt.id;
    world.on_writer(move |state| {
        use koru_ai_core::AiPersistence;
        state.repository.begin_turn(&pending).unwrap();
    });
    attempt
}

fn delta(attempt: InferenceAttemptId, text: &str) -> PublicAiEvent {
    PublicAiEvent::TextDelta {
        attempt_id: attempt,
        text: text.into(),
        at: Timestamp::now(),
    }
}

/// The events of a reply pass through to a device that watches its
/// conversation: the text as it is written, in order, with ascending
/// sequences, and then the end of the watch when the reply ends.
#[test]
fn m8_live_role_the_events_of_a_running_reply_pass_through() {
    let world = World::new();
    let device = world.device();
    let (mut live, hello) = world.open(device, Role::Live);
    assert!(hello.capabilities.contains(&"live.ai_stream.1".to_owned()));
    world.runtime.block_on(watch(&live, world.bot.main_chat));
    until("the watch", || watching(&world));

    // Another stream of the same device sends the message.
    let (mut commands, _) = world.open(device, Role::Command);
    let request = Uuid::new_v4();
    let sent = world.command(
        &mut commands,
        request,
        send(&world.bot, world.bot.main_chat, request),
    );
    assert_eq!(sent.outcome, Outcome::Completed, "{sent:?}");
    let accepted: koru_ai_wire::SendTurnAccepted = sent.response.unwrap().parse().unwrap();

    let (text, sequences, end) = world.runtime.block_on(async {
        let (mut text, mut sequences) = (String::new(), Vec::new());
        loop {
            match live.next().await {
                Some(WireMessage::Live(Live {
                    chat_id,
                    attempt_id,
                    event,
                })) => {
                    assert_eq!(chat_id, world.bot.main_chat);
                    assert_eq!(attempt_id, accepted.attempt_id);
                    assert_eq!(event.version, koru_ai_wire::AI_STREAM_VERSION);
                    sequences.push(event.sequence);
                    if let PublicAiEvent::TextDelta { text: more, .. } = event.event {
                        text.push_str(&more);
                    }
                }
                Some(WireMessage::LiveEnd(end)) => return (text, sequences, end),
                other => panic!("expected a live event, read {other:?}"),
            }
        }
    });
    assert_eq!(text, "NATIVE-REPLY");
    assert!(
        sequences.windows(2).all(|pair| pair[0] < pair[1]),
        "{sequences:?}"
    );
    assert_eq!(
        end,
        LiveEnd {
            chat_id: world.bot.main_chat,
            attempt_id: Some(accepted.attempt_id),
            reason: LiveEndReason::Terminal,
        }
    );
    // The watch ended with the reply.
    until("the watch ended", || !watching(&world));
}

/// A watch holds 64 events its stream has not written. One more ends it
/// with `live_end { dropped }`, and nothing more of that reply is sent.
/// **Nothing of it is written to the feed**: the head, the reply's stored
/// text and the receipts are what they were.
#[test]
fn m8_live_role_a_full_buffer_ends_the_watch_and_nothing_is_written_to_the_feed() {
    let world = World::new();
    let device = world.device();
    let attempt = pending_attempt(&world);
    let (mut live, _) = world.open(device, Role::Live);
    world.runtime.block_on(watch(&live, world.bot.main_chat));
    until("the watch", || watching(&world));

    // The client reads nothing while a hundred events are published.
    let chat = world.bot.main_chat;
    let (before, after, stored) = world.on_writer(move |state| {
        use koru_ai_core::AiPersistence;
        let before = state.repository.feed_head().unwrap();
        for index in 0..100 {
            state.publish_public(delta(attempt, &format!("{index} ")));
        }
        let stored = state.repository.messages(chat).unwrap();
        (before, state.repository.feed_head().unwrap(), stored)
    });
    assert_eq!(before, after);
    assert!(
        stored
            .iter()
            .all(|message| !message.markdown.contains("0 "))
    );
    assert!(receipts(&world).is_empty());
    assert!(
        !watching(&world),
        "the full watch was dropped on the writer"
    );

    let (events, end) = world.runtime.block_on(async {
        let mut events = 0;
        loop {
            match live.next().await {
                Some(WireMessage::Live(_)) => events += 1,
                Some(WireMessage::LiveEnd(end)) => return (events, end),
                other => panic!("expected a live event, read {other:?}"),
            }
        }
    });
    // What the stream had written or held when the buffer filled: the 64
    // of the watch, and the few frames already on their way.
    assert!((64..100).contains(&events), "{events}");
    assert_eq!(end.reason, LiveEndReason::Dropped);
    assert_eq!(end.attempt_id, Some(attempt));

    // The watch is over: a later event of the reply is not sent.
    world.on_writer(move |state| state.publish_public(delta(attempt, "later")));
    assert!(
        world
            .runtime
            .block_on(live.silent(Duration::from_millis(300)))
    );
    assert_eq!(
        world.on_writer(|state| state.repository.feed_head().unwrap()),
        before
    );
}

/// Only a Bot conversation a copy shows can be watched: anything else is
/// `not_found`. An `unwatch` ends the watch `closed`. A fifth watch on one
/// stream is refused.
#[test]
fn m8_live_role_only_a_shown_bot_conversation_is_watched() {
    let world = World::new();
    let device = world.device();
    let (mut live, _) = world.open(device, Role::Live);
    let ended = |live: &mut Client| {
        world.runtime.block_on(async {
            match live.next().await {
                Some(WireMessage::LiveEnd(end)) => end,
                other => panic!("expected the end of a watch, read {other:?}"),
            }
        })
    };

    // A conversation nothing has, which is how an ordinary chat reads too.
    let absent = ChatId::new();
    world.runtime.block_on(watch(&live, absent));
    assert_eq!(
        ended(&mut live),
        LiveEnd {
            chat_id: absent,
            attempt_id: None,
            reason: LiveEndReason::NotFound,
        }
    );
    assert!(!watching(&world));

    // The Bot's workspace is kept on this computer.
    world
        .authority
        .keep_on_this_computer(world.bot.home_workspace, true);
    world.runtime.block_on(watch(&live, world.bot.main_chat));
    assert_eq!(ended(&mut live).reason, LiveEndReason::NotFound);
    assert!(!watching(&world));
    world
        .authority
        .keep_on_this_computer(world.bot.home_workspace, false);

    // Watched, then unwatched.
    world.runtime.block_on(watch(&live, world.bot.main_chat));
    until("the watch", || watching(&world));
    world
        .runtime
        .block_on(live.write(WireMessage::Unwatch(Unwatch {
            chat_id: world.bot.main_chat,
        })));
    assert_eq!(
        ended(&mut live),
        LiveEnd {
            chat_id: world.bot.main_chat,
            attempt_id: None,
            reason: LiveEndReason::Closed,
        }
    );
    assert!(!watching(&world));

    // Four more conversations of the Bot, so one stream can ask for five.
    let (mut commands, _) = world.open(device, Role::Command);
    let mut chats = vec![world.bot.main_chat];
    for _ in 0..4 {
        let request = Uuid::new_v4();
        let receipt = world.command(
            &mut commands,
            request,
            ClientCommand::CreateChat {
                request_id: BotRequestId::from_uuid(request),
                bot_id: world.bot.id,
            },
        );
        assert_eq!(receipt.outcome, Outcome::Completed, "{receipt:?}");
        let BotResponse::Chat { reference } = receipt.response.unwrap().parse().unwrap() else {
            panic!("a conversation")
        };
        chats.push(reference.chat_id);
    }
    world.runtime.block_on(async {
        for chat in &chats {
            watch(&live, *chat).await;
        }
        assert_eq!(live.refusal().await, refusal::BUSY);
    });
    // The stream ended, and the writer forgot its four watches.
    until("the watches are forgotten", || !watching(&world));
}

/// A watch that stands is checked again: when the Bot's workspace becomes
/// kept on this computer, the watch ends `not_found` and nothing more of the
/// conversation is forwarded.
#[test]
fn m8_live_role_a_watch_ends_when_the_bot_stops_being_copied() {
    let world = World::with(
        Timing {
            reconcile: Duration::from_millis(200),
            ..Timing::default()
        },
        Native::default(),
    );
    let device = world.device();
    let attempt = pending_attempt(&world);
    let (mut live, _) = world.open(device, Role::Live);
    world.runtime.block_on(watch(&live, world.bot.main_chat));
    until("the watch", || watching(&world));
    world.on_writer(move |state| state.publish_public(delta(attempt, "before")));
    assert!(matches!(
        world.runtime.block_on(live.next()),
        Some(WireMessage::Live(_))
    ));

    world
        .authority
        .keep_on_this_computer(world.bot.home_workspace, true);
    let end = world.runtime.block_on(async {
        loop {
            match live.next().await {
                Some(WireMessage::LiveEnd(end)) => return end,
                Some(WireMessage::Live(_)) => {}
                other => panic!("expected the end of the watch, read {other:?}"),
            }
        }
    });
    assert_eq!(
        end,
        LiveEnd {
            chat_id: world.bot.main_chat,
            attempt_id: None,
            reason: LiveEndReason::NotFound,
        }
    );
    assert!(!watching(&world));
    world.on_writer(move |state| state.publish_public(delta(attempt, "after")));
    assert!(
        world
            .runtime
            .block_on(live.silent(Duration::from_millis(300)))
    );
}

/// A tool call is forwarded as a copied message holds it: its name and
/// state, never its arguments or its results. The session's command
/// listing is not forwarded at all.
#[test]
fn m8_live_role_a_tool_call_is_forwarded_without_its_arguments_or_results() {
    let world = World::new();
    let device = world.device();
    let attempt = pending_attempt(&world);
    let (mut live, _) = world.open(device, Role::Live);
    world.runtime.block_on(watch(&live, world.bot.main_chat));
    until("the watch", || watching(&world));

    let chat = world.bot.main_chat;
    world.on_writer(move |state| {
        use koru_ai_core::AiPersistence;
        let message = state
            .repository
            .attempt(attempt)
            .unwrap()
            .unwrap()
            .assistant_message_id
            .unwrap();
        state.publish_public(PublicAiEvent::AgentCommands {
            attempt_id: attempt,
            chat,
            commands: Vec::new(),
        });
        state.publish_public(PublicAiEvent::ToolActivity {
            attempt_id: attempt,
            activity: ToolActivity {
                id: ToolInvocationId::new(),
                attempt_id: attempt,
                chat_id: chat,
                assistant_message_id: message,
                tool_name: "agent.command".into(),
                read_only: false,
                status: ToolActivityStatus::Running,
                approval: ToolApproval::NotRequired,
                proposed_at: Timestamp::now(),
                started_at: None,
                completed_at: None,
                result_bytes: None,
                error_code: None,
                target_workspace_id: None,
                target_document_id: None,
                error_message: None,
                agent_label: Some("Run a command".into()),
                agent_summary: None,
                agent_input: Some("SECRET-ARGUMENTS".into()),
                agent_output: Some("SECRET-RESULTS".into()),
                text_offset: None,
                display_target: Some("C:/secret/path".into()),
                live: None,
            },
        });
    });
    let frame = world.runtime.block_on(live.next());
    let Some(WireMessage::Live(Live { event, .. })) = frame else {
        panic!("expected the tool call, read {frame:?}")
    };
    let PublicAiEvent::ToolActivity { activity, .. } = &event.event else {
        panic!("the command listing was forwarded: {:?}", event.event)
    };
    assert_eq!(activity.agent_label.as_deref(), Some("Run a command"));
    assert_eq!(activity.agent_input, None);
    assert_eq!(activity.agent_output, None);
    let wire = serde_json::to_string(&event).unwrap();
    assert!(
        !wire.contains("SECRET") && !wire.contains("secret"),
        "{wire}"
    );
}
