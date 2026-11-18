// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::managed::tests::opened;
use crate::reader::Reader;
use std::sync::mpsc::{Receiver, SyncSender};

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
        skills: vec![],
    };
    let Some(Command::Engine {
        command:
            C::Send {
                id,
                attachments,
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
}

fn surface() -> (ChatSurface, Receiver<Command>, SyncSender<Update>) {
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
    assert_eq!(surface.response_action_count(), 1);
    assert!(surface.managed.as_ref().unwrap().history_pending);
}
