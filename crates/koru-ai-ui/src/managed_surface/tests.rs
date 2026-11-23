// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::managed::tests::opened;
use crate::reader::Reader;
use std::sync::mpsc::{Receiver, SyncSender};

#[test]
fn a_new_native_question_stays_after_the_previous_answer_while_waiting() {
    let (mut surface, _, _) = surface();
    let state = surface.managed.as_mut().unwrap();
    let mut first = crate::managed::tests::message(1, uuid::Uuid::new_v4());
    first.sender = "person:fixture".into();
    first.content.body = "Tell me a joke".into();
    let mut reply = crate::managed::tests::message(2, uuid::Uuid::new_v4());
    reply.content.body = "A short joke.".into();
    reply.content.in_reply_to = Some(first.id);
    let mut next = crate::managed::tests::message(3, uuid::Uuid::new_v4());
    next.sender = "person:fixture".into();
    next.content.body = "What is the news?".into();
    let expected = [first.id, reply.id, next.id].map(MessageId::from_uuid);
    for message in [first, reply, next] {
        state.ingest(message);
    }
    surface.transcript = Some(state.transcript());
    surface.rebuild_messages(640.0);
    let shown: Vec<_> = surface
        .message_layouts
        .iter()
        .filter_map(|m| m.id)
        .collect();
    assert_eq!(&shown[..3], &expected);
    surface.rebuild_messages(480.0);
    let shown: Vec<_> = surface
        .message_layouts
        .iter()
        .filter_map(|m| m.id)
        .collect();
    assert_eq!(
        &shown[..3],
        &expected,
        "resizing must preserve engine order"
    );
}

#[test]
fn native_instruction_notice_reaches_the_existing_visible_status_surface() {
    let (mut surface, _, _) = surface();
    let state = surface.managed.as_ref().unwrap();
    let mut entry = crate::managed::tests::event(state, 1, None);
    entry.observation.event = koru_ai_wire::bot_threads::AgentEvent::Notice {
        turn_id: entry.observation.turn.id.to_string().parse().unwrap(),
        text: "Edited instructions were unavailable; built-in instructions were used.".into(),
    };
    let update = managed::Refreshed {
        navigation: Default::default(),
        snapshot: state.snapshot.clone(),
        messages: vec![],
        after: state.after,
        feed: managed::Feed {
            boot: uuid::Uuid::new_v4(),
            cursor: 1,
            gap: false,
            entries: vec![entry],
        },
        questions: vec![],
        attention: Default::default(),
    };
    assert!(surface.managed_refreshed(update));
    assert_eq!(
        surface.status.as_deref(),
        Some("Edited instructions were unavailable; built-in instructions were used.")
    );
    assert!(surface.managed.as_ref().unwrap().notice.is_none());
}

#[test]
fn assigning_the_window_profile_never_lists_ordinary_chats_for_a_bot() {
    let (mut surface, commands, _) = surface();
    let profile = koru_ai_core::ProfileId::new();
    surface.transcript.as_mut().unwrap().chat.profile_id = Some(profile);
    assert!(surface.set_profile(Some(profile)));
    assert!(commands.try_recv().is_err());
    assert!(!surface.queue_command(Command::ListChats(Some(profile))));
    assert!(surface.status.is_none());
    assert!(surface.activate_send_button());
    assert!(matches!(
        commands.try_recv().unwrap(),
        Command::Engine {
            command: C::Stop { .. },
            ..
        }
    ));
}

#[test]
fn attachment_send_keeps_ids_and_receipt_identity_on_the_engine_carrier() {
    let (mut surface, _, _) = surface();
    let thread = surface.managed.as_ref().unwrap().thread();
    let operation_id = uuid::Uuid::new_v4();
    let attachment = koru_ai_core::ChatAttachmentId::new();
    let request = koru_ai_wire::SendTurnRequest {
        destination_workspace: None,
        operation_id,
        chat_id: thread,
        surface: SURFACE,
        markdown: "with a file".into(),
        explicit_refs: vec![],
        reasoning_override: None,
        web_search: false,
        system_prompt_override: None,
        replace_pinned_documents: true,
        attachment_ids: vec![attachment],
        answers: None,
        skills: vec!["review-doc".into()],
        mini_app_host: None,
        mini_app_refinement: None,
    };
    let Some(Command::Engine {
        command:
            C::Send {
                id,
                attachments,
                references,
                thread: sent_thread,
                ..
            },
        ..
    }) = surface.managed_command(Command::Send(request))
    else {
        panic!("engine send expected")
    };
    assert_eq!(id, operation_id);
    assert_eq!(sent_thread, thread);
    assert_eq!(attachments, vec![serde_json::json!(attachment.as_uuid())]);
    assert_eq!(
        references,
        vec![serde_json::json!({"kind":"skill","name":"review-doc"})]
    );
}

pub(crate) fn surface() -> (ChatSurface, Receiver<Command>, SyncSender<Update>) {
    let opened = opened();
    let mut surface = ChatSurface::new_owned_at(None);
    surface.bind_owned(crate::OwnedConversation {
        chat: opened.snapshot.conversation.chat.id,
        destination: WorkspaceId::new(),
        provider: opened
            .snapshot
            .conversation
            .chat
            .current_model
            .provider
            .clone(),
        offers: Default::default(),
        refusals: &[],
    });
    let (reader, commands, updates) = Reader::test_pair();
    surface.reader = Some(reader);
    surface.managed_opened(opened);
    while commands.try_recv().is_ok() {}
    (surface, commands, updates)
}

fn send(surface: &mut ChatSurface) -> C {
    let state = surface.managed.as_ref().unwrap();
    C::Send {
        id: uuid::Uuid::new_v4(),
        bot: state.snapshot.conversation.label.bot_id,
        thread: state.thread(),
        body: "Keep exactly this".into(),
        in_reply_to: None,
        attachments: vec![],
        references: vec![],
    }
}

#[test]
fn acceptance_is_not_a_running_attempt_and_stop_names_real_turn() {
    let (mut surface, commands, _updates) = surface();
    let command = send(&mut surface);
    let C::Send { id, thread, .. } = command else {
        unreachable!()
    };
    surface.managed.as_mut().unwrap().pending_send = Some(command);
    assert!(surface.managed_reply(
        thread,
        R::Accepted {
            request: id,
            message: uuid::Uuid::new_v4(),
            thread: thread.as_uuid(),
            sequence: 1,
            replayed: false
        }
    ));
    assert!(surface.attempt.is_none() && surface.pending_assistant_message_id.is_none());
    let actual = surface.managed.as_ref().unwrap().primary_turn().unwrap();
    assert_eq!(surface.send_mode(), queue::SendMode::Stop);
    assert!(surface.activate_send_button());
    let Command::Engine {
        command: C::Stop { turn, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!("expected stop")
    };
    assert_eq!(
        (turn.id, turn.generation, turn.worker, turn.session),
        (actual.id, actual.generation, actual.worker, actual.session)
    );
}

#[test]
fn uncertain_send_reuses_id_after_rebinding_and_never_clears_other_chat() {
    let (mut surface, commands, _updates) = surface();
    let command = send(&mut surface);
    let serialized = serde_json::to_value(&command).unwrap();
    let C::Send { id, thread, .. } = command else {
        unreachable!()
    };
    let snapshot = surface.managed.as_ref().unwrap().snapshot.clone();
    surface.managed.as_mut().unwrap().pending_send = Some(command.clone());
    surface.managed_failed(
        thread,
        command,
        koru_ai_wire::Failure {
            code: "gateway_unavailable".into(),
            message: "offline".into(),
            retryable: true,
            details: Default::default(),
        },
    );
    assert!(surface.managed.as_ref().unwrap().retry_send);
    surface.leave_managed();
    assert!(surface.managed.is_none());
    assert_eq!(surface.managed_outbox.len(), 1);
    surface.managed_opened(managed::Opened {
        navigation: Default::default(),
        snapshot,
        messages: vec![],
        before: None,
        after: 0,
        settings: None,
        questions: vec![],
        attention: Default::default(),
    });
    surface.maintain_managed(Instant::now() + Duration::from_secs(3));
    let retried = std::iter::from_fn(|| commands.try_recv().ok())
        .find_map(|c| match c {
            Command::Engine {
                command: C::Send { .. },
                ..
            } => Some(c),
            _ => None,
        })
        .unwrap();
    let Command::Engine {
        command: retried, ..
    } = retried
    else {
        unreachable!()
    };
    assert_eq!(serde_json::to_value(retried).unwrap(), serialized);
    assert!(!surface.managed_reply(
        ChatId::new(),
        R::Accepted {
            request: id,
            message: uuid::Uuid::new_v4(),
            thread: thread.as_uuid(),
            sequence: 1,
            replayed: false
        }
    ));
    assert!(surface.managed.as_ref().unwrap().pending_send.is_some());
}

#[test]
fn changing_native_model_keeps_the_actual_running_turn_and_stop_target() {
    let (mut surface, commands, _) = surface();
    let state = surface.managed.as_ref().unwrap();
    let thread = state.thread();
    let actual = state.primary_turn().unwrap();
    let turns = serde_json::to_value(&state.snapshot.turns).unwrap();
    let mut next = state.snapshot.conversation.chat.current_model.clone();
    next.requested_model = "next-model".into();
    let revision = state.snapshot.settings_revision + 1;
    assert!(surface.managed_reply(
        thread,
        R::Settings {
            request: uuid::Uuid::new_v4(),
            revision,
            model: next.clone(),
            mode: None,
            fast: None,
            replayed: false,
        }
    ));
    assert_eq!(
        serde_json::to_value(&surface.managed.as_ref().unwrap().snapshot.turns).unwrap(),
        turns
    );
    assert_eq!(
        surface.transcript.as_ref().unwrap().chat.current_model,
        next
    );
    assert_eq!(surface.send_mode(), queue::SendMode::Stop);
    assert!(surface.status.is_none());
    assert!(surface.activate_send_button());
    assert!(commands.try_iter().any(|command| matches!(command, Command::Engine { command: C::Stop { turn, .. }, .. } if (turn.id, turn.generation, turn.worker, turn.session) == (actual.id, actual.generation, actual.worker, actual.session))));
}

#[test]
fn hidden_surface_has_no_poll_deadline_or_new_jobs() {
    let (mut surface, commands, _updates) = surface();
    surface.deactivate();
    assert!(surface.reader.is_none());
    surface.maintain_managed(Instant::now() + Duration::from_secs(2));
    assert!(commands.try_recv().is_err());
    assert!(!surface.managed.as_ref().unwrap().polling);
}

#[test]
fn model_controls_preserve_conversation_fields_and_never_change_global_defaults() {
    let (mut surface, commands, _updates) = surface();
    let state = surface.managed.as_mut().unwrap();
    state.snapshot.conversation.chat.agent_mode = Some("plan".into());
    state.snapshot.conversation.chat.agent_fast = Some(true);
    state.snapshot.settings_revision = 7;
    let thread = state.thread();
    let mut model = state.snapshot.conversation.chat.current_model.clone();
    model.requested_model = "another-model".into();
    assert!(surface.queue_command(Command::UpdateModel(
        thread,
        UpdateChatModelRequest {
            model: model.clone()
        }
    )));
    let Command::Engine {
        command:
            C::Settings {
                revision,
                mode,
                fast,
                model: selected,
                ..
            },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!("expected engine settings")
    };
    assert_eq!(revision, 7);
    assert_eq!(mode.as_deref(), Some("plan"));
    assert_eq!(fast, Some(true));
    assert_eq!(selected, model);
    assert!(commands.try_recv().is_err());
}

#[test]
fn history_loading_is_bounded_and_local_blocks_offer_no_durable_link() {
    let (mut surface, commands, _updates) = surface();
    surface.managed.as_mut().unwrap().before = Some(40);
    surface.follow_tail = false;
    surface.conversation_scroll = 0.0;
    let now = Instant::now();
    surface.maintain_managed(now);
    surface.maintain_managed(now);
    let reads = std::iter::from_fn(|| commands.try_recv().ok())
        .filter(|command| {
            matches!(
                command,
                Command::Engine {
                    command: C::History {
                        before: Some(40),
                        ..
                    },
                    ..
                }
            )
        })
        .count();
    assert_eq!(reads, 1);
    assert!(surface.managed.as_ref().unwrap().history_pending);
}
