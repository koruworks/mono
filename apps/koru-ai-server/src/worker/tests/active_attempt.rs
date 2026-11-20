// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::sync::{Mutex, mpsc::Receiver};

/// The test, not a provider process or clock, decides when events arrive.
struct Controlled {
    requests: std::sync::mpsc::SyncSender<AiRequest>,
    events: Mutex<Option<Receiver<AiEvent>>>,
}

impl AiProvider for Controlled {
    fn start(&self, request: AiRequest, _: Cancellation) -> Result<ProviderStream, AiError> {
        self.requests.send(request).unwrap();
        Ok(Box::new(
            self.events.lock().unwrap().take().unwrap().into_iter(),
        ))
    }
}

#[test]
fn authorized_transcript_keeps_zero_tool_pending_attempt_and_omits_it_after_terminal() {
    let (events, receive_events) = sync_channel(4);
    let (requests, receive_request) = sync_channel(1);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(Controlled {
            requests,
            events: Mutex::new(Some(receive_events)),
        }),
    );
    let mut config = WorkerConfig::new("unused-memory-fixture".into(), "internal-debug", providers);
    config.test_repository = Some(koru_ai_sqlite::Repository::in_memory().unwrap());
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();
    let chat = create_chat(&client);
    let other = create_chat(&client);
    let stream = send(&client, chat, Uuid::new_v4());
    let accepted = stream.accepted.clone();
    let request = receive_request
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(request.attempt_id, accepted.attempt_id);

    let pending = client.read_chat(context(), chat).unwrap();
    assert!(pending.tool_activities.is_empty());
    assert!(pending.messages[1].markdown.is_empty());
    let attempt = pending.active_attempt.as_ref().unwrap();
    assert_eq!(attempt.id, accepted.attempt_id);
    assert_eq!(attempt.chat_id, Some(chat));
    assert_eq!(
        attempt.assistant_message_id,
        Some(accepted.assistant_message_id)
    );
    assert_eq!(attempt.status, AttemptStatus::Pending);
    assert_eq!(attempt.requested_model, model().requested_model);
    assert!(
        client
            .read_chat(context(), other)
            .unwrap()
            .active_attempt
            .is_none()
    );
    let mut denied = context();
    denied.principal.scopes.clear();
    assert!(
        client.read_chat(denied, chat).is_err(),
        "the projection grants no access"
    );

    // A stale, foreign, detached or terminal attempt never supplies a Stop
    // target merely because a transcript still has a pending placeholder.
    for status in [
        AttemptStatus::Complete,
        AttemptStatus::Interrupted,
        AttemptStatus::Failed,
        AttemptStatus::Cancelled,
    ] {
        let mut ended = attempt.clone();
        ended.status = status;
        assert!(
            transcript_active_attempt(chat, &pending.messages, &[attempt.clone(), ended]).is_none()
        );
    }
    let mut foreign = attempt.clone();
    foreign.chat_id = Some(other);
    assert!(transcript_active_attempt(chat, &pending.messages, &[foreign]).is_none());
    let mut detached = attempt.clone();
    detached.assistant_message_id = Some(MessageId::new());
    assert!(transcript_active_attempt(chat, &pending.messages, &[detached]).is_none());
    let mut terminal_message = pending.messages.clone();
    terminal_message[1].state = MessageState::Complete;
    assert!(transcript_active_attempt(chat, &terminal_message, &[attempt.clone()]).is_none());

    events
        .send(AiEvent::Started {
            attempt_id: accepted.attempt_id,
            provider_response_id: None,
            request_payload_bytes: None,
            resolved_model: Some("actual-model".into()),
            effective_reasoning: None,
            at: Timestamp::now(),
        })
        .unwrap();
    loop {
        if matches!(
            stream
                .events
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .event,
            PublicAiEvent::Started { .. }
        ) {
            break;
        }
    }
    let running = client
        .read_chat(context(), chat)
        .unwrap()
        .active_attempt
        .unwrap();
    assert_eq!(running.status, AttemptStatus::Streaming);
    assert_eq!(running.id, accepted.attempt_id);
    assert_eq!(running.resolved_model.as_deref(), Some("actual-model"));
    events
        .send(AiEvent::Completed {
            attempt_id: accepted.attempt_id,
            stop_reason: None,
            at: Timestamp::now(),
        })
        .unwrap();
    drop(events);
    assert_eq!(
        wait_for_terminal(&client, accepted.attempt_id),
        AttemptStatus::Complete
    );
    let complete = client.read_chat(context(), chat).unwrap();
    assert!(complete.active_attempt.is_none());
    assert_eq!(complete.messages[1].state, MessageState::Complete);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}
