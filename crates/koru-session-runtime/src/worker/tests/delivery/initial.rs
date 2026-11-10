// SPDX-License-Identifier: MPL-2.0

use super::*;

fn input(text: &str) -> PendingInput {
    PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        text: text.into(),
        attachment_ids: vec![],
        references: vec![],
        mode: None,
        fast: None,
        reasoning: None,
        skills: vec![],
        model: None,
    }
}

fn idle() -> (ManagedHarness, ControllerLeaseToken) {
    let h = managed_harness(CapabilitySet::new([
        capability::MANAGED_EVENTS.parse().unwrap(),
        capability::NATIVE_ATTACHMENT_DELIVERY.parse().unwrap(),
        capability::CONDITIONAL_TURN_STEERING.parse().unwrap(),
    ]));
    h.state.inner.lock().unwrap().hold_events = true;
    let lease = h.handle.attach(viewport(1, 0, 6), 20).unwrap().lease.token;
    (h, lease)
}

#[test]
fn initial_replay_accepts_fresh_lease_and_never_claims_a_different_active_turn() {
    let (h, lease) = idle();
    let request = input("initial");
    let first = h
        .handle
        .begin_pending_input(lease, request.clone())
        .unwrap();
    assert_eq!(
        h.handle
            .begin_pending_input(lease, input("second"))
            .unwrap_err(),
        ErrorCode::InvalidTransition
    );
    h.handle.detach(lease).unwrap();
    let fresh = h.handle.attach(viewport(2, 0, 6), 21).unwrap().lease.token;
    assert!(
        h.handle
            .begin_pending_input(lease, request.clone())
            .is_err()
    );
    let human_turn = AgentTurnId::mint();
    h.handle.shared.lock().summary.active_turn_id = Some(human_turn);
    assert_eq!(
        h.handle
            .begin_pending_input(fresh, request.clone())
            .unwrap(),
        first
    );
    assert_eq!(h.handle.summary().active_turn_id, Some(human_turn));
    let mut changed = request.clone();
    changed.text.push_str("changed");
    assert_eq!(
        h.handle.begin_pending_input(fresh, changed).unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        h.handle
            .send_pending_input(fresh, request.clone())
            .unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        h.handle
            .steer_pending_input(fresh, human_turn, request)
            .unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
    let state = h.state.inner.lock().unwrap();
    assert_eq!(state.calls.len(), 1);
    assert_eq!(state.steer_calls, 0);
}

#[test]
fn uncertain_initial_failure_is_retained_and_never_retried_or_steered() {
    let (h, lease) = idle();
    h.state.inner.lock().unwrap().reject_submit = true;
    let request = input("initial");
    assert_eq!(
        h.handle
            .begin_pending_input(lease, request.clone())
            .unwrap_err(),
        ErrorCode::DeliveryUnsupported
    );
    assert!(
        h.handle.summary().active_turn_id.is_some(),
        "uncertain submit keeps its reservation"
    );
    h.state.inner.lock().unwrap().reject_submit = false;
    assert_eq!(
        h.handle
            .begin_pending_input(lease, request.clone())
            .unwrap_err(),
        ErrorCode::DeliveryUnsupported
    );
    assert_eq!(
        h.handle.send_pending_input(lease, request).unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        h.handle
            .begin_pending_input(lease, input("new"))
            .unwrap_err(),
        ErrorCode::InvalidTransition
    );
    let state = h.state.inner.lock().unwrap();
    assert!(state.calls.is_empty());
    assert_eq!(state.steer_calls, 0);
}

#[test]
fn initial_preserves_supported_settings_references_and_staged_attachments() {
    let (h, lease) = idle();
    let attachment = AttachmentId::mint();
    h.handle
        .add_attachment(lease, pending_attachment(h.handle.id(), attachment))
        .unwrap();
    let mut request = input("initial");
    request.attachment_ids.push(attachment);
    request.mode = Some("plan".parse().unwrap());
    request.fast = Some(true);
    request.reasoning = Some(koru_session_protocol::LaunchReasoning::High);
    request.skills.push("fixture".into());
    request.model = Some("fixture-model".parse().unwrap());
    request.references.push(
        serde_json::from_value(serde_json::json!({"kind":"file","path":PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixture")})).unwrap(),
    );
    let result = h
        .handle
        .begin_pending_input(lease, request.clone())
        .unwrap();
    let state = h.state.inner.lock().unwrap();
    let forwarded = &state.calls[0];
    assert_eq!(forwarded.turn_id, result.turn_id);
    assert_eq!(forwarded.mode, request.mode);
    assert_eq!(forwarded.fast, request.fast);
    assert_eq!(forwarded.reasoning, request.reasoning);
    assert_eq!(forwarded.skills, request.skills);
    assert_eq!(forwarded.model, request.model);
    assert_eq!(forwarded.references, request.references);
    assert_eq!(forwarded.attachments[0].id, attachment);
}

#[test]
fn in_flight_initial_replay_is_backpressured_across_lease_change() {
    let (h, lease) = idle();
    h.state.inner.lock().unwrap().hold_submit = true;
    let request = input("initial");
    let sender = h.handle.clone();
    let submitted = request.clone();
    let thread = std::thread::spawn(move || sender.begin_pending_input(lease, submitted));
    {
        let mut state = h.state.inner.lock().unwrap();
        while state.calls.is_empty() {
            let (next, timeout) = h
                .state
                .ready
                .wait_timeout(state, Duration::from_secs(2))
                .unwrap();
            assert!(!timeout.timed_out());
            state = next;
        }
    }
    h.handle.detach(lease).unwrap();
    let fresh = h.handle.attach(viewport(2, 0, 6), 21).unwrap().lease.token;
    assert_eq!(
        h.handle
            .begin_pending_input(fresh, request.clone())
            .unwrap_err(),
        ErrorCode::Backpressure
    );
    h.state.inner.lock().unwrap().hold_submit = false;
    h.state.ready.notify_all();
    let original = thread.join().unwrap().unwrap();
    assert_eq!(
        h.handle.begin_pending_input(fresh, request).unwrap(),
        original
    );
    assert_eq!(h.state.inner.lock().unwrap().calls.len(), 1);
}

#[test]
fn ordinary_input_key_cannot_be_reinterpreted_as_initial() {
    let (h, lease) = idle();
    let request = input("ordinary");
    h.handle.send_pending_input(lease, request.clone()).unwrap();
    assert_eq!(
        h.handle.begin_pending_input(lease, request).unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(h.state.inner.lock().unwrap().calls.len(), 1);
}

#[test]
fn invalid_preparation_leaves_key_unused_but_saturation_preserves_cached_replay() {
    let (h, lease) = idle();
    let mut request = input("initial");
    let attachment = AttachmentId::mint();
    request.attachment_ids.push(attachment);
    assert_eq!(
        h.handle
            .begin_pending_input(lease, request.clone())
            .unwrap_err(),
        ErrorCode::AttachmentNotFound
    );
    assert!(
        !h.handle
            .shared
            .lock()
            .pending_inputs
            .contains_key(&request.idempotency_key)
    );
    h.handle
        .add_attachment(lease, pending_attachment(h.handle.id(), attachment))
        .unwrap();
    let first = h
        .handle
        .begin_pending_input(lease, request.clone())
        .unwrap();
    {
        let mut state = h.handle.shared.lock();
        state.summary.active_turn_id = None;
        for _ in 0..4096 {
            state.failed_ordinary_steers.insert(IdempotencyKey::mint());
        }
    }
    assert_eq!(
        h.handle
            .begin_pending_input(lease, input("new"))
            .unwrap_err(),
        ErrorCode::Backpressure
    );
    assert_eq!(h.handle.begin_pending_input(lease, request).unwrap(), first);
    assert_eq!(h.state.inner.lock().unwrap().calls.len(), 1);
}
