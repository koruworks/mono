// SPDX-License-Identifier: MPL-2.0

use std::sync::mpsc::Receiver;
use std::time::Duration;

use koru_ai_core::{AiProviderId, ModelSelection, ReasoningChoice, Timestamp};
use koru_ai_wire::{
    AgentProviderView, ChatTranscript, Failure, ProviderSettingsResponse, PublicAiEvent,
    SendTurnAccepted, StreamEvent,
};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

use super::*;
use crate::reader::{Reader, Update};
use crate::{SurfaceMode, attachments};

/// A surface on a new agent chat, with the agents the settings list:
/// `(id, steers)`.
fn agent_surface(
    provider: &str,
    agents: &[(&str, bool)],
) -> (ChatSurface, ChatTranscript, Receiver<Command>) {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    let (reader, commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    let mut transcript = crate::unmade_transcript(ModelSelection {
        provider: AiProviderId::parse(provider).unwrap(),
        requested_model: koru_ai_core::AGENT_DEFAULT_MODEL.into(),
        default_reasoning: ReasoningChoice::Default,
    });
    transcript.chat.id = ChatId::new();
    transcript.chat.title = "Agent chat".into();
    transcript.tool_policy.chat_id = transcript.chat.id;
    surface.transcript = Some(transcript.clone());
    surface.validated_ready = true;
    surface.settings = Some(ProviderSettingsResponse {
        selected_provider: None,
        openai: koru_ai_wire::OpenAiSettingsView {
            model: None,
            reasoning: ReasoningChoice::Default,
            credential_configured: false,
        },
        ollama: koru_ai_wire::OllamaSettingsView {
            endpoint: "http://localhost:11434".into(),
            model: None,
            reasoning: ReasoningChoice::Default,
        },
        agents: agents
            .iter()
            .map(|(id, steers)| AgentProviderView {
                id: AiProviderId::parse(*id).unwrap(),
                label: (*id).into(),
                steers: *steers,
            })
            .collect(),
    });
    // No reader thread answers here: the test applies each update itself,
    // and the sender is kept so the surface never reads a hung-up reader.
    std::mem::forget(_updates);
    (surface, transcript, commands)
}

fn type_into(surface: &mut ChatSurface, text: &str) {
    for ch in text.chars() {
        let s = ch.to_string();
        surface.key(
            &WinitKey::Character(s.as_str().into()),
            Some(&s),
            ModifiersState::empty(),
        );
    }
}

fn press(surface: &mut ChatSurface, key: NamedKey) -> bool {
    surface.key(&WinitKey::Named(key), None, ModifiersState::empty())
}

/// Every command the surface queued since the last look.
fn drain(commands: &Receiver<Command>) -> Vec<Command> {
    let mut out = Vec::new();
    while let Ok(command) = commands.recv_timeout(Duration::from_millis(20)) {
        out.push(command);
    }
    out
}

fn sends(commands: &[Command]) -> Vec<String> {
    commands
        .iter()
        .filter_map(|command| match command {
            Command::Send(request) => Some(request.markdown.clone()),
            _ => None,
        })
        .collect()
}

/// Send `text` with nothing running, and have the service accept it.
fn send_and_accept(
    surface: &mut ChatSurface,
    commands: &Receiver<Command>,
    text: &str,
) -> InferenceAttemptId {
    type_into(surface, text);
    assert!(press(surface, NamedKey::Enter));
    assert_eq!(sends(&drain(commands)), [text]);
    let attempt = InferenceAttemptId::new();
    surface.apply_update(Update::Accepted(SendTurnAccepted {
        attempt_id: attempt,
        user_message_id: koru_ai_core::MessageId::new(),
        assistant_message_id: koru_ai_core::MessageId::new(),
        replayed: false,
    }));
    attempt
}

/// The reply's terminal event, then the transcript read it asks for.
fn end_reply(surface: &mut ChatSurface, transcript: &ChatTranscript, event: PublicAiEvent) {
    surface.apply_update(Update::Event(StreamEvent {
        version: 1,
        sequence: 9,
        event,
    }));
    surface.apply_update(Update::Transcript(transcript.clone()));
}

fn completed(attempt: InferenceAttemptId) -> PublicAiEvent {
    PublicAiEvent::Completed {
        attempt_id: attempt,
        stop_reason: None,
        at: Timestamp::from_millis(5),
    }
}

fn cancelled(attempt: InferenceAttemptId) -> PublicAiEvent {
    PublicAiEvent::Cancelled {
        attempt_id: attempt,
        at: Timestamp::from_millis(5),
    }
}

fn failed(attempt: InferenceAttemptId) -> PublicAiEvent {
    PublicAiEvent::Failed {
        attempt_id: attempt,
        error: koru_ai_core::AiError::new(koru_ai_core::AiErrorClass::Provider, "no", true),
        at: Timestamp::from_millis(5),
    }
}

fn queued_texts(surface: &ChatSurface) -> Vec<String> {
    surface
        .shown_queue()
        .map(|queue| {
            queue
                .items
                .iter()
                .map(|item| item.markdown.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// A stored attachment on the shown chat's strip.
fn stored_attachment(surface: &mut ChatSurface) {
    let chat = surface.shown_chat_id().unwrap();
    surface.strips.push(
        chat,
        attachments::Draft {
            id: koru_ai_core::ChatAttachmentId::new(),
            file_name: "a.png".into(),
            source: koru_ai_core::AttachmentSource::Clipboard,
            kind: koru_ai_core::AttachmentKind::Image,
            size: Some((4, 4)),
            byte_count: 64,
            state: attachments::DraftState::Stored,
            bytes: None,
            path: None,
        },
    );
}

// ----- queueing -----------------------------------------------------------

/// **Enter while a reply runs queues the message** and clears the
/// composer; nothing is sent, and the list shows it.
#[test]
fn sending_while_a_reply_runs_queues_the_message_and_clears_the_composer() {
    let (mut surface, _transcript, commands) = agent_surface("agent-claude", &[]);
    send_and_accept(&mut surface, &commands, "first");
    type_into(&mut surface, "second");
    assert!(press(&mut surface, NamedKey::Enter));
    assert!(surface.composer.is_blank(), "the composer is cleared");
    assert!(sends(&drain(&commands)).is_empty(), "nothing is sent");
    assert_eq!(queued_texts(&surface), ["second"]);
    assert!(surface.queue_band_h() > 0.0, "the pending list shows");
    type_into(&mut surface, "third");
    assert!(press(&mut surface, NamedKey::Enter));
    assert_eq!(
        queued_texts(&surface),
        ["second", "third"],
        "first in, first out"
    );
}

/// **A reply that completes sends the head of the queue.**
#[test]
fn a_completed_reply_sends_the_head_of_the_queue() {
    let (mut surface, transcript, commands) = agent_surface("agent-claude", &[]);
    let attempt = send_and_accept(&mut surface, &commands, "first");
    type_into(&mut surface, "second");
    press(&mut surface, NamedKey::Enter);
    type_into(&mut surface, "third");
    press(&mut surface, NamedKey::Enter);
    type_into(&mut surface, "being typed");
    end_reply(&mut surface, &transcript, completed(attempt));
    assert_eq!(sends(&drain(&commands)), ["second"]);
    assert_eq!(queued_texts(&surface), ["third"]);
    assert_eq!(
        surface.composer.text(),
        "being typed",
        "what is being typed stays where it is"
    );
}

/// **A Stop pauses the queue**, and so does a failure; *Send next* sends
/// the head and resumes it.
#[test]
fn a_stop_or_a_failure_pauses_the_queue_and_send_next_resumes_it() {
    let ends: [fn(InferenceAttemptId) -> PublicAiEvent; 2] = [cancelled, failed];
    for end in ends {
        let (mut surface, transcript, commands) = agent_surface("agent-claude", &[]);
        let attempt = send_and_accept(&mut surface, &commands, "first");
        type_into(&mut surface, "second");
        press(&mut surface, NamedKey::Enter);
        end_reply(&mut surface, &transcript, end(attempt));
        assert!(
            sends(&drain(&commands)).is_empty(),
            "nothing sent by itself"
        );
        let chat = surface.shown_chat_id().unwrap();
        assert!(surface.queues.is_paused(chat), "Queue paused · Send next");
        assert!(surface.queue_action(QueueHit::SendNext));
        assert_eq!(sends(&drain(&commands)), ["second"]);
        assert!(!surface.queues.is_paused(chat));
    }
}

/// A message sent by hand while the queue is paused resumes it: what is
/// queued follows the reply to that message.
#[test]
fn a_send_by_hand_resumes_a_paused_queue() {
    let (mut surface, transcript, commands) = agent_surface("agent-claude", &[]);
    let attempt = send_and_accept(&mut surface, &commands, "first");
    type_into(&mut surface, "queued");
    press(&mut surface, NamedKey::Enter);
    end_reply(&mut surface, &transcript, cancelled(attempt));
    let chat = surface.shown_chat_id().unwrap();
    assert!(surface.queues.is_paused(chat));
    let attempt = send_and_accept(&mut surface, &commands, "by hand");
    assert!(!surface.queues.is_paused(chat));
    end_reply(&mut surface, &transcript, completed(attempt));
    assert_eq!(sends(&drain(&commands)), ["queued"]);
}

/// **Edit pulls the item back into the composer** and out of the queue;
/// **Remove** drops it.
#[test]
fn edit_and_remove() {
    let (mut surface, _transcript, commands) = agent_surface("agent-claude", &[]);
    send_and_accept(&mut surface, &commands, "first");
    for text in ["one", "two", "three"] {
        type_into(&mut surface, text);
        press(&mut surface, NamedKey::Enter);
    }
    assert!(surface.queue_action(QueueHit::Remove(1)));
    assert_eq!(queued_texts(&surface), ["one", "three"]);
    assert!(surface.queue_action(QueueHit::Edit(0)));
    assert_eq!(surface.composer.text(), "one");
    assert_eq!(queued_texts(&surface), ["three"]);
    // Edit with something already in the composer: it takes the place.
    assert!(surface.queue_action(QueueHit::Edit(0)));
    assert_eq!(surface.composer.text(), "three");
    assert_eq!(queued_texts(&surface), ["one"], "nothing typed is lost");
    assert!(sends(&drain(&commands)).is_empty());
}

/// **A queued send the service refused goes back to the head**, paused,
/// never into the composer over what is being typed.
#[test]
fn a_refused_queued_send_goes_back_to_the_head_of_the_queue() {
    let (mut surface, transcript, commands) = agent_surface("agent-claude", &[]);
    let attempt = send_and_accept(&mut surface, &commands, "first");
    for text in ["second", "third"] {
        type_into(&mut surface, text);
        press(&mut surface, NamedKey::Enter);
    }
    end_reply(&mut surface, &transcript, completed(attempt));
    assert_eq!(sends(&drain(&commands)), ["second"]);
    type_into(&mut surface, "mine");
    surface.apply_update(Update::SendFailed(Failure {
        code: "chat_busy".into(),
        message: "This chat already has a response in progress.".into(),
        retryable: true,
        details: Default::default(),
    }));
    assert_eq!(queued_texts(&surface), ["second", "third"]);
    assert_eq!(surface.composer.text(), "mine", "the composer is untouched");
    let chat = surface.shown_chat_id().unwrap();
    assert!(surface.queues.is_paused(chat), "it waits for Send next");
}

/// **The queue is per chat**: another chat shows its own, and coming back
/// shows the first again, paused for the person.
#[test]
fn each_chat_has_its_own_queue() {
    let (mut surface, first, commands) = agent_surface("agent-claude", &[]);
    send_and_accept(&mut surface, &commands, "hello");
    type_into(&mut surface, "for the first chat");
    press(&mut surface, NamedKey::Enter);
    let mut second = first.clone();
    second.chat.id = ChatId::new();
    surface.apply_update(Update::Transcript(second.clone()));
    assert!(surface.shown_queue().is_none(), "the other chat has none");
    assert_eq!(surface.queue_band_h(), 0.0);
    surface.apply_update(Update::Transcript(first.clone()));
    assert_eq!(queued_texts(&surface), ["for the first chat"]);
    assert!(surface.queues.is_paused(first.chat.id));
}

// ----- the button and Escape -------------------------------------------

/// **While a reply runs the button reads Queue with something in the
/// composer and Stop without**; Escape twice stops the reply whatever the
/// composer holds.
#[test]
fn the_button_reads_queue_or_stop_and_escape_twice_still_stops() {
    let (mut surface, _transcript, commands) = agent_surface("agent-claude", &[]);
    assert_eq!(surface.send_mode(), SendMode::Send);
    let attempt = send_and_accept(&mut surface, &commands, "first");
    assert_eq!(surface.send_mode(), SendMode::Stop);
    assert_eq!(surface.send_button_name(), "Stop");
    type_into(&mut surface, "more");
    assert_eq!(surface.send_mode(), SendMode::Queue);
    assert_eq!(surface.send_button_name(), "Queue");
    // A press on the button queues it.
    surface.composer_rect();
    let button = surface.send_button();
    surface.mouse_down(button.x + 2.0, button.y + 2.0);
    assert_eq!(queued_texts(&surface), ["more"]);
    assert_eq!(surface.send_mode(), SendMode::Stop);
    // Escape twice stops, with text in the composer too.
    type_into(&mut surface, "typed");
    drain(&commands);
    press(&mut surface, NamedKey::Escape);
    press(&mut surface, NamedKey::Escape);
    let stopped = drain(&commands)
        .into_iter()
        .any(|command| matches!(command, Command::Cancel(cancelled) if cancelled == attempt));
    assert!(stopped, "Esc Esc stops the reply");
    // And a press on Stop with an empty composer stops it.
    surface.composer.clear();
    surface.mouse_down(button.x + 2.0, button.y + 2.0);
    assert!(
        drain(&commands)
            .into_iter()
            .any(|command| matches!(command, Command::Cancel(_)))
    );
}

// ----- steer ------------------------------------------------------------

/// **Steer to an agent that takes text mid-turn is native**: the item's
/// text goes to the attempt's steer; taken, it shows under the reply.
#[test]
fn steer_is_native_for_an_agent_that_steers() {
    let (mut surface, _transcript, commands) =
        agent_surface("agent-codex", &[("agent-codex", true)]);
    let attempt = send_and_accept(&mut surface, &commands, "first");
    type_into(&mut surface, "also this");
    press(&mut surface, NamedKey::Enter);
    assert!(surface.queue_action(QueueHit::Steer(0)));
    let steered = drain(&commands)
        .into_iter()
        .find_map(|command| match command {
            Command::Steer(to, request) => Some((to, request.markdown)),
            _ => None,
        });
    assert_eq!(steered, Some((attempt, "also this".to_owned())));
    assert!(
        surface.shown_queue().is_none(),
        "out of the queue while it goes"
    );
    surface.apply_update(Update::Steered(koru_ai_wire::SteerAttemptResponse {
        attempt_id: attempt,
        user_message_id: koru_ai_core::MessageId::new(),
        replayed: false,
    }));
    assert_eq!(surface.steered_shown(), ["also this"]);
}

/// A steer the reply would not take goes back to the head of the queue,
/// unpaused, and is sent when the reply ends.
#[test]
fn a_refused_native_steer_waits_first_in_line() {
    let (mut surface, transcript, commands) =
        agent_surface("agent-claude", &[("agent-claude", true)]);
    let attempt = send_and_accept(&mut surface, &commands, "first");
    for text in ["one", "two"] {
        type_into(&mut surface, text);
        press(&mut surface, NamedKey::Enter);
    }
    surface.queue_action(QueueHit::Steer(1));
    surface.apply_update(Update::SteerFailed(Failure {
        code: "steer_unavailable".into(),
        message: "This reply cannot take a message now.".into(),
        retryable: true,
        details: Default::default(),
    }));
    assert_eq!(queued_texts(&surface), ["two", "one"]);
    end_reply(&mut surface, &transcript, completed(attempt));
    assert!(sends(&drain(&commands)).contains(&"two".to_owned()));
}

/// **Every other agent is steered by stopping the reply**: Steer cancels
/// it, and when the cancel lands the item is sent first — the queue is not
/// paused, as a Stop would pause it.
#[test]
fn steer_stops_then_sends_for_an_agent_without_steering() {
    let (mut surface, transcript, commands) =
        agent_surface("agent-opencode", &[("agent-opencode", false)]);
    let attempt = send_and_accept(&mut surface, &commands, "first");
    for text in ["one", "two"] {
        type_into(&mut surface, text);
        press(&mut surface, NamedKey::Enter);
    }
    assert_eq!(
        surface.hit_name(QueueHit::Steer(1)),
        "Stops the reply, then sends"
    );
    surface.queue_action(QueueHit::Steer(1));
    let cancelled_it = drain(&commands)
        .into_iter()
        .any(|command| matches!(command, Command::Cancel(to) if to == attempt));
    assert!(cancelled_it);
    assert_eq!(
        queued_texts(&surface),
        ["two", "one"],
        "the steered item is first"
    );
    end_reply(&mut surface, &transcript, cancelled(attempt));
    assert_eq!(sends(&drain(&commands)), ["two"]);
    let chat = surface.shown_chat_id().unwrap();
    assert!(!surface.queues.is_paused(chat), "a steer is not a Stop");
}

/// A message with an attachment is never steered natively: it rides a
/// send, so it stops the reply first even for an agent that steers.
#[test]
fn a_steer_with_attachments_stops_then_sends() {
    let (mut surface, transcript, commands) =
        agent_surface("agent-codex", &[("agent-codex", true)]);
    let attempt = send_and_accept(&mut surface, &commands, "first");
    stored_attachment(&mut surface);
    type_into(&mut surface, "see this");
    press(&mut surface, NamedKey::Enter);
    assert!(!surface.has_strip(), "the attachment went with it");
    assert_eq!(
        surface.hit_name(QueueHit::Steer(0)),
        "Stops the reply, then sends"
    );
    surface.queue_action(QueueHit::Steer(0));
    let commands_now = drain(&commands);
    assert!(
        commands_now
            .iter()
            .any(|command| matches!(command, Command::Cancel(to) if *to == attempt))
    );
    assert!(
        !commands_now
            .iter()
            .any(|command| matches!(command, Command::Steer(..)))
    );
    end_reply(&mut surface, &transcript, cancelled(attempt));
    let sent = drain(&commands)
        .into_iter()
        .find_map(|command| match command {
            Command::Send(request) => Some(request),
            _ => None,
        })
        .expect("sent once the cancel landed");
    assert_eq!(sent.markdown, "see this");
    assert_eq!(sent.attachment_ids.len(), 1);
}
