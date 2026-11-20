// SPDX-License-Identifier: MPL-2.0

use super::*;

fn snapshot() -> koru_delegate_preamble::Resolved {
    let mut value = koru_delegate_preamble::session::bundled();
    value.text = "Custom manual foundation\n".into();
    value
}

fn ready() -> (ManagedHarness, ControllerLeaseToken) {
    let h = managed_harness_oriented(
        CapabilitySet::new([
            capability::MANAGED_EVENTS.parse().unwrap(),
            capability::NATIVE_ATTACHMENT_DELIVERY.parse().unwrap(),
        ]),
        Some(snapshot()),
    );
    h.state.inner.lock().unwrap().hold_events = true;
    let lease = h.handle.attach(viewport(1, 0, 6), 20).unwrap().lease.token;
    (h, lease)
}

fn input(text: &str) -> PendingInput {
    PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        text: text.into(),
        attachment_ids: vec![],
        references: vec![],
        model: None,
        mode: None,
        fast: None,
        reasoning: None,
        skills: vec![],
    }
}

#[test]
fn first_begin_delivers_common_once_and_exact_reattach_replay_keeps_caller_identity() {
    let (h, lease) = ready();
    let mut request = input("Person's exact words");
    request.fast = Some(true);
    request.model = Some("fixture-model".parse().unwrap());
    request.skills = vec!["review-pr".into()];
    request
        .references
        .push(koru_session_protocol::PathReference {
            kind: koru_session_protocol::PathReferenceKind::Folder,
            path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .display()
                .to_string(),
        });
    let attachment = AttachmentId::mint();
    h.handle
        .add_attachment(lease, pending_attachment(h.handle.id(), attachment))
        .unwrap();
    request.attachment_ids.push(attachment);
    let accepted = h
        .handle
        .begin_pending_input(lease, request.clone())
        .unwrap();
    h.handle.detach(lease).unwrap();
    let lease = h.handle.attach(viewport(2, 0, 6), 21).unwrap().lease.token;
    assert_eq!(
        h.handle
            .begin_pending_input(lease, request.clone())
            .unwrap(),
        accepted
    );
    let mut changed = request.clone();
    changed.text.push('!');
    assert_eq!(
        h.handle.begin_pending_input(lease, changed).unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
    let calls = &h.state.inner.lock().unwrap().calls;
    assert_eq!(calls.len(), 1);
    assert!(calls[0].text.starts_with("Custom manual foundation\n"));
    assert!(calls[0].text.ends_with(&request.text));
    assert_eq!(calls[0].fast, request.fast);
    assert_eq!(calls[0].model, request.model);
    assert_eq!(calls[0].skills, request.skills);
    assert_eq!(calls[0].references, request.references);
    assert_eq!(calls[0].attachments[0].id, attachment);
    assert_eq!(calls[0].turn_id, accepted.turn_id);
}

#[test]
fn proven_unsent_first_send_then_b_accept_then_a_retry_does_not_duplicate_foundation() {
    let (h, lease) = ready();
    let a = input("A");
    {
        let mut peer = h.state.inner.lock().unwrap();
        peer.reject_submit = true;
        peer.reject_known_unsent = true;
    }
    assert_eq!(
        h.handle.send_pending_input(lease, a.clone()).unwrap_err(),
        ErrorCode::DeliveryUnsupported
    );
    h.state.inner.lock().unwrap().reject_submit = false;
    let b = h.handle.send_pending_input(lease, input("B")).unwrap();
    h.handle
        .shared
        .apply_agent_event(AgentEvent::TurnCompleted { turn_id: b.turn_id });
    h.handle.send_pending_input(lease, a).unwrap();
    let peer = h.state.inner.lock().unwrap();
    assert_eq!(peer.calls.len(), 2);
    assert!(peer.calls[0].text.contains("Custom manual foundation"));
    assert_eq!(peer.calls[1].text, "A");
}

#[test]
fn ambiguous_send_keeps_first_owner_through_a_later_proven_unsent_retry() {
    let (h, lease) = ready();
    let a = input("A");
    h.state.inner.lock().unwrap().reject_submit = true;
    assert_eq!(
        h.handle.send_pending_input(lease, a.clone()).unwrap_err(),
        ErrorCode::DeliveryUnsupported
    );
    h.state.inner.lock().unwrap().reject_known_unsent = true;
    assert_eq!(
        h.handle.send_pending_input(lease, a.clone()).unwrap_err(),
        ErrorCode::DeliveryUnsupported
    );
    h.state.inner.lock().unwrap().reject_submit = false;
    assert_eq!(
        h.handle.send_pending_input(lease, input("B")).unwrap_err(),
        ErrorCode::Backpressure
    );
    h.handle.send_pending_input(lease, a).unwrap();
    let peer = h.state.inner.lock().unwrap();
    assert_eq!(peer.calls.len(), 1);
    assert!(peer.calls[0].text.contains("Custom manual foundation"));
    assert_eq!(peer.attempted[0].text, peer.attempted[1].text);
    assert_eq!(peer.attempted[0].text, peer.attempted[2].text);
}

#[test]
fn missing_attachment_or_oversize_cannot_consume_first_orientation() {
    let (h, lease) = ready();
    let mut missing = input("Missing");
    missing.attachment_ids.push(AttachmentId::mint());
    assert_eq!(
        h.handle.send_pending_input(lease, missing).unwrap_err(),
        ErrorCode::AttachmentNotFound
    );
    assert_eq!(
        h.handle
            .send_pending_input(lease, input(&"x".repeat(crate::MAX_QUEUED_WRITE_BYTES)))
            .unwrap_err(),
        ErrorCode::InputTooLarge
    );
    h.handle
        .send_pending_input(lease, input("Real first input"))
        .unwrap();
    assert!(
        h.state.inner.lock().unwrap().calls[0]
            .text
            .starts_with("Custom manual foundation")
    );
}

#[test]
fn authoritative_completion_reconciles_unknown_carrier_without_requiring_a_resend() {
    let (h, lease) = ready();
    let a = input("A");
    h.state.inner.lock().unwrap().reject_submit = true;
    assert!(h.handle.send_pending_input(lease, a.clone()).is_err());
    let turn = h.state.inner.lock().unwrap().attempted[0].turn_id;
    h.handle
        .shared
        .apply_agent_event(AgentEvent::TurnStarted { turn_id: turn });
    assert!(
        h.handle.shared.lock().orientation.owner().is_some(),
        "a write is not acceptance"
    );
    h.handle
        .shared
        .apply_agent_event(AgentEvent::TurnCompleted {
            turn_id: AgentTurnId::mint(),
        });
    assert!(
        h.handle.shared.lock().orientation.owner().is_some(),
        "another turn cannot settle this carrier"
    );
    h.handle
        .shared
        .apply_agent_event(AgentEvent::TurnCompleted { turn_id: turn });
    assert!(h.handle.shared.lock().orientation.owner().is_none());
    h.state.inner.lock().unwrap().reject_submit = false;
    h.handle.send_pending_input(lease, a).unwrap();
    h.handle.send_pending_input(lease, input("B")).unwrap();
    let peer = h.state.inner.lock().unwrap();
    assert_eq!(
        peer.calls.len(),
        1,
        "the completed uncertain A is never resent"
    );
    assert_eq!(peer.calls[0].text, "B");
}

#[test]
fn oversized_strict_begin_is_local_preflight_and_a_shorter_begin_can_start() {
    let (h, lease) = ready();
    let rejected = input(&"x".repeat(crate::MAX_QUEUED_WRITE_BYTES));
    assert_eq!(
        h.handle
            .begin_pending_input(lease, rejected.clone())
            .unwrap_err(),
        ErrorCode::InputTooLarge
    );
    assert!(h.state.inner.lock().unwrap().attempted.is_empty());
    assert!(h.handle.summary().active_turn_id.is_none());
    assert!(
        !h.handle
            .shared
            .lock()
            .pending_inputs
            .contains_key(&rejected.idempotency_key)
    );
    h.handle
        .begin_pending_input(lease, input("Shorter request"))
        .unwrap();
    let peer = h.state.inner.lock().unwrap();
    assert_eq!(peer.calls.len(), 1);
    assert!(peer.calls[0].text.starts_with("Custom manual foundation"));
}

#[test]
fn accepted_first_carrier_survives_lease_change_and_reports_fallback_once() {
    let mut source = snapshot();
    source.notice = Some(koru_delegate_preamble::Notice::Empty);
    let h = managed_harness_oriented(
        CapabilitySet::new([
            capability::MANAGED_EVENTS.parse().unwrap(),
            capability::NATIVE_ATTACHMENT_DELIVERY.parse().unwrap(),
        ]),
        Some(source),
    );
    {
        let mut peer = h.state.inner.lock().unwrap();
        peer.hold_submit = true;
        peer.hold_events = true;
    }
    let lease = h.handle.attach(viewport(1, 0, 6), 20).unwrap().lease.token;
    let request = input("first");
    let sender = h.handle.clone();
    let sent = request.clone();
    let task = std::thread::spawn(move || sender.begin_pending_input(lease, sent));
    {
        let mut peer = h.state.inner.lock().unwrap();
        while peer.calls.is_empty() {
            let (next, timeout) = h
                .state
                .ready
                .wait_timeout(peer, Duration::from_secs(2))
                .unwrap();
            assert!(!timeout.timed_out());
            peer = next;
        }
    }
    h.handle.detach(lease).unwrap();
    let next_lease = h.handle.attach(viewport(2, 0, 6), 21).unwrap().lease.token;
    assert_eq!(
        h.handle
            .begin_pending_input(next_lease, request.clone())
            .unwrap_err(),
        ErrorCode::Backpressure
    );
    h.state.inner.lock().unwrap().hold_submit = false;
    h.state.ready.notify_all();
    let accepted = task.join().unwrap().unwrap();
    assert_eq!(
        h.handle.begin_pending_input(next_lease, request).unwrap(),
        accepted
    );
    assert_eq!(h.state.inner.lock().unwrap().calls.len(), 1);
    assert_eq!(
        h.handle
            .observe_agent_events()
            .unwrap()
            .events
            .iter()
            .filter(|event| matches!(event.event, AgentEvent::Notice { .. }))
            .count(),
        1
    );
}
