// SPDX-License-Identifier: MPL-2.0

use super::*;
mod readiness;

#[test]
fn a_slow_local_consumer_gets_an_explicit_gap_and_a_closed_stream() {
    let queue = Queue::new();
    {
        let mut state = queue.state.lock().unwrap();
        for _ in 0..SUBSCRIPTION_CAPACITY {
            state
                .updates
                .push_back(SubscriptionUpdate::Closed { error: None });
        }
    }
    queue.push(SubscriptionUpdate::Closed { error: None });
    let state = queue.state.lock().unwrap();
    assert!(state.closed);
    assert!(matches!(
        state.updates.front(),
        Some(SubscriptionUpdate::Closed {
            error: Some(SessionFailure {
                code: ErrorCode::StreamGap,
                ..
            })
        })
    ));
}

#[test]
fn empty_queue_wakes_do_not_disconnect_or_restart_the_timeout() {
    let queue = Arc::new(Queue::new());
    let subscription = Subscription {
        id: SubscriptionId(1),
        queue: Arc::clone(&queue),
    };
    let timeout = Duration::from_millis(100);
    let (finished, completion) = std::sync::mpsc::channel();
    let receiver = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let result = subscription.recv_timeout(timeout);
        finished.send((result, started.elapsed())).unwrap();
    });
    // Wake a live, empty queue throughout the wait. No producer adds an
    // update or closes it: a wake is permission to recheck the predicate,
    // never evidence of a disconnect. The outer bound also catches a wait
    // that restarts its full timeout on every notification.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let (result, elapsed) = loop {
        queue.ready.notify_all();
        match completion.recv_timeout(Duration::from_millis(1)) {
            Ok(completed) => break completed,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "wakes restarted the timeout"
                );
            }
            Err(error) => panic!("subscription receiver failed: {error}"),
        }
    };
    receiver.join().unwrap();
    assert!(
        matches!(result, Err(ClientError::TimedOut)),
        "live empty queue returned {result:?}"
    );
    assert!(elapsed >= timeout, "a notification ended the wait early");
}

#[test]
fn closed_empty_queue_reports_disconnect_without_waiting() {
    let queue = Arc::new(Queue::new());
    queue.close();
    let subscription = Subscription {
        id: SubscriptionId(1),
        queue,
    };
    assert!(matches!(
        subscription.recv_timeout(Duration::from_secs(1)),
        Err(ClientError::Disconnected)
    ));
}

struct ApprovalWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
    inner: std::sync::Weak<Inner>,
}

impl Write for ApprovalWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(inner) = self.inner.upgrade() {
            for sender in inner.pending.lock().unwrap().values() {
                let _ = sender.try_send(Ok(Response::Ok));
            }
        }
        Ok(())
    }
}

fn approval_client(
    version: u32,
    managed: bool,
    dynamic: bool,
) -> (SessionClient, Arc<Mutex<Vec<u8>>>) {
    use koru_session_protocol::{CapabilityName, CapabilitySet, ProtocolAgreement, capability};
    use std::str::FromStr;
    let inner = Arc::new(Inner {
        writer: Mutex::new(None),
        pending: Mutex::new(HashMap::new()),
        subscriptions: Mutex::new(BTreeMap::new()),
        orphaned: Mutex::new(BTreeMap::new()),
        next_request: AtomicU64::new(1),
        state: Mutex::new(ConnectionState::Connected),
        controller: Mutex::new(None),
    });
    let bytes = Arc::new(Mutex::new(Vec::new()));
    *inner.writer.lock().unwrap() = Some(Box::new(ApprovalWriter {
        bytes: bytes.clone(),
        inner: Arc::downgrade(&inner),
    }));
    let mut capabilities = Vec::new();
    if managed {
        capabilities.push(CapabilityName::from_str(capability::MANAGED_APPROVAL).unwrap());
    }
    if dynamic {
        capabilities.push(CapabilityName::from_str(capability::DYNAMIC_APPROVALS).unwrap());
    }
    (
        SessionClient {
            inner,
            host: koru_session_wire::HostHello {
                product: "fixture".into(),
                product_version: "fixture".into(),
                instance_id: "fixture".into(),
                supervisor_launch_id: "fixture".into(),
                agreement: ProtocolAgreement {
                    version,
                    capabilities: CapabilitySet::new(capabilities),
                },
            },
        },
        bytes,
    )
}

#[test]
fn explicit_choices_never_downgrade_or_write_to_an_unnegotiated_host() {
    for (version, managed, dynamic) in [(7, true, true), (14, true, false), (14, false, true)] {
        let (client, bytes) = approval_client(version, managed, dynamic);
        let result = client.answer_approval(
            SessionId::mint(),
            ControllerLeaseToken::mint(),
            IdempotencyKey::mint(),
            ApprovalId::mint(),
            ApprovalDecision::Approve,
            Some(koru_session_protocol::ApprovalChoiceId::mint()),
        );
        assert!(matches!(
            result,
            Err(ClientError::Session(SessionFailure {
                code: ErrorCode::CapabilityNotNegotiated,
                ..
            }))
        ));
        assert!(
            bytes.lock().unwrap().is_empty(),
            "refusal must precede wire traffic"
        );
    }
}

#[test]
fn old_hosts_receive_only_the_legacy_approval_shape() {
    let (client, bytes) = approval_client(14, true, false);
    client
        .answer_approval(
            SessionId::mint(),
            ControllerLeaseToken::mint(),
            IdempotencyKey::mint(),
            ApprovalId::mint(),
            ApprovalDecision::Reject,
            None,
        )
        .unwrap();
    let bytes = bytes.lock().unwrap();
    let body = std::str::from_utf8(&bytes[koru_ipc::frame::HEADER_LEN..]).unwrap();
    assert!(
        !body.contains("choice"),
        "an old host must never receive the new field"
    );
    let (_, frame): (_, RequestFrame) = koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
    assert!(matches!(
        frame.request,
        Request::AnswerApproval {
            decision: ApprovalDecision::Reject,
            choice: None,
            ..
        }
    ));
}

#[test]
fn negotiated_host_receives_the_exact_selected_choice() {
    let (client, bytes) = approval_client(14, true, true);
    let selected = koru_session_protocol::ApprovalChoiceId::mint();
    client
        .answer_approval(
            SessionId::mint(),
            ControllerLeaseToken::mint(),
            IdempotencyKey::mint(),
            ApprovalId::mint(),
            ApprovalDecision::Approve,
            Some(selected),
        )
        .unwrap();
    let bytes = bytes.lock().unwrap();
    let (_, frame): (_, RequestFrame) = koru_ipc::frame::read_frame(&mut bytes.as_slice()).unwrap();
    assert!(matches!(frame.request, Request::AnswerApproval {
        decision: ApprovalDecision::Approve, choice: Some(id), ..
    } if id == selected));
}

/// **A set's answers as the next message reach only a host that asks
/// questions** (koru-ai D108): to any other the input is refused before the
/// wire — it has no set to answer.
#[test]
fn answers_as_the_next_message_need_managed_questions() {
    let (client, bytes) = approval_client(21, true, true);
    assert!(!client.supports_questions());
    let refused = client.send_pending_answers(
        SessionId::mint(),
        ControllerLeaseToken::mint(),
        PendingInput {
            model: None,
            idempotency_key: IdempotencyKey::mint(),
            text: String::new(),
            attachment_ids: Vec::new(),
            mode: None,
            fast: None,
            reasoning: None,
            skills: Vec::new(),
        },
        koru_session_protocol::QuestionsReply {
            request_id: koru_session_protocol::QuestionRequestId::mint(),
            answers: Vec::new(),
        },
    );
    assert!(matches!(
        refused,
        Err(ClientError::Session(SessionFailure {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        }))
    ));
    assert!(bytes.lock().unwrap().is_empty(), "refused before the wire");
}

/// **Skills named with an input, and an agent's skills listed, need
/// `agent-commands`** (koru-ai D121): to any other host they are refused
/// before the wire — it would read past the skills and send `$name` as
/// words alone.
#[test]
fn skills_need_agent_commands() {
    let (client, bytes) = approval_client(21, true, true);
    assert!(!client.supports_agent_commands());
    let refused = client.send_pending_input(
        SessionId::mint(),
        ControllerLeaseToken::mint(),
        PendingInput {
            model: None,
            idempotency_key: IdempotencyKey::mint(),
            text: "Use $review".into(),
            attachment_ids: Vec::new(),
            mode: None,
            fast: None,
            reasoning: None,
            skills: vec!["review".into()],
        },
    );
    assert!(matches!(
        refused,
        Err(ClientError::Session(SessionFailure {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        }))
    ));
    let listed = client
        .discover_provider_commands("codex".parse().unwrap(), std::path::PathBuf::from("/work"));
    assert!(matches!(
        listed,
        Err(ClientError::Session(SessionFailure {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        }))
    ));
    assert!(bytes.lock().unwrap().is_empty(), "refused before the wire");
}

/// **A turn's level never reaches a v19 host** (koru-ai D70): it would read
/// and ignore it and run the turn at the old level, so the input is refused
/// before any wire traffic. A v20 host takes it.
#[test]
fn a_turns_level_is_sent_only_to_a_v20_host() {
    let input = || PendingInput {
        model: None,
        idempotency_key: IdempotencyKey::mint(),
        text: "go on".into(),
        attachment_ids: Vec::new(),
        mode: None,
        fast: None,
        reasoning: Some(koru_session_protocol::LaunchReasoning::High),
        skills: Vec::new(),
    };
    let (older, bytes) = approval_client(19, true, true);
    assert!(!older.supports_turn_reasoning());
    let refused =
        older.send_pending_input(SessionId::mint(), ControllerLeaseToken::mint(), input());
    assert!(matches!(
        refused,
        Err(ClientError::Session(SessionFailure {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        }))
    ));
    assert!(bytes.lock().unwrap().is_empty(), "refused before the wire");
    let (current, bytes) = approval_client(20, true, true);
    assert!(current.supports_turn_reasoning());
    let _ = current.send_pending_input(SessionId::mint(), ControllerLeaseToken::mint(), input());
    let bytes = bytes.lock().unwrap();
    let body = std::str::from_utf8(&bytes[koru_ipc::frame::HEADER_LEN..]).unwrap();
    assert!(body.contains("\"reasoning\":\"high\""), "{body}");
}

/// **A turn's model never reaches a v21 host**: it would read past it and
/// run the turn on the session's old model, so the input — and a set's
/// answers sent as the next message — is refused before any wire traffic.
/// A v22 host takes it.
#[test]
fn a_turns_model_is_sent_only_to_a_v22_host() {
    let input = || PendingInput {
        model: Some("sonnet".parse().unwrap()),
        idempotency_key: IdempotencyKey::mint(),
        text: "go on".into(),
        attachment_ids: Vec::new(),
        mode: None,
        fast: None,
        reasoning: None,
        skills: Vec::new(),
    };
    let refused = |result: Result<SendPendingInputResult, ClientError>| {
        matches!(
            result,
            Err(ClientError::Session(SessionFailure {
                code: ErrorCode::CapabilityNotNegotiated,
                ..
            }))
        )
    };
    let (older, bytes) = approval_client(21, true, true);
    assert!(older.supports_turn_reasoning());
    assert!(!older.supports_turn_model());
    assert!(refused(older.send_pending_input(
        SessionId::mint(),
        ControllerLeaseToken::mint(),
        input()
    )));
    assert!(bytes.lock().unwrap().is_empty(), "refused before the wire");
    let (current, bytes) = approval_client(22, true, true);
    assert!(current.supports_turn_model());
    let _ = current.send_pending_input(SessionId::mint(), ControllerLeaseToken::mint(), input());
    let bytes = bytes.lock().unwrap();
    let body = std::str::from_utf8(&bytes[koru_ipc::frame::HEADER_LEN..]).unwrap();
    assert!(body.contains("\"model\":\"sonnet\""), "{body}");
}
