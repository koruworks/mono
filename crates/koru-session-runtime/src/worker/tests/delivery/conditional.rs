// SPDX-License-Identifier: MPL-2.0

use super::*;

fn text_input(text: &str) -> PendingInput {
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

fn running() -> (ManagedHarness, ControllerLeaseToken, AgentTurnId) {
    let harness = managed_harness(CapabilitySet::new([
        capability::MANAGED_EVENTS.parse().unwrap(),
        capability::CONDITIONAL_TURN_STEERING.parse().unwrap(),
    ]));
    {
        let mut state = harness.state.inner.lock().unwrap();
        state.hold_events = true;
        state.steerable = true;
    }
    let lease = harness
        .handle
        .attach(viewport(1, 0, 6), 20)
        .unwrap()
        .lease
        .token;
    let turn = harness
        .handle
        .send_pending_input(lease, text_input("first"))
        .unwrap()
        .turn_id;
    (harness, lease, turn)
}

#[test]
fn lost_ack_and_ended_turn_replay_once_but_changed_identity_and_lease_refuse() {
    let (h, lease, turn) = running();
    let input = text_input("amend");
    assert_eq!(
        h.handle
            .steer_pending_input(lease, turn, input.clone())
            .unwrap()
            .turn_id,
        turn
    );
    h.handle.shared.lock().summary.active_turn_id = None;
    assert_eq!(
        h.handle
            .steer_pending_input(lease, turn, input.clone())
            .unwrap()
            .turn_id,
        turn
    );
    let mut changed = input.clone();
    changed.text = "different".into();
    assert_eq!(
        h.handle
            .steer_pending_input(lease, turn, changed)
            .unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        h.handle
            .steer_pending_input(lease, AgentTurnId::mint(), input.clone())
            .unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
    h.handle.detach(lease).unwrap();
    let next = h.handle.attach(viewport(2, 0, 6), 21).unwrap().lease.token;
    assert!(
        h.handle
            .steer_pending_input(lease, turn, input.clone())
            .is_err()
    );
    assert_eq!(
        h.handle
            .steer_pending_input(next, turn, input.clone())
            .unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
    let (other, other_lease, other_turn) = running();
    assert!(
        other
            .handle
            .steer_pending_input(lease, other_turn, input.clone())
            .is_err()
    );
    assert_eq!(
        other
            .handle
            .steer_pending_input(other_lease, turn, input)
            .unwrap_err(),
        ErrorCode::InvalidTransition
    );
    let state = h.state.inner.lock().unwrap();
    assert_eq!(state.calls.len(), 1);
    assert_eq!(state.steer_calls, 1);
}

#[test]
fn native_turn_end_race_and_uncertain_result_never_start_or_resend_a_turn() {
    for failure in [ErrorCode::InvalidTransition, ErrorCode::ProviderUnavailable] {
        let (h, lease, turn) = running();
        h.state.inner.lock().unwrap().hold_steer = true;
        let input = text_input("amend");
        let sender = h.handle.clone();
        let sent = input.clone();
        let pending = std::thread::spawn(move || sender.steer_pending_input(lease, turn, sent));
        {
            let mut state = h.state.inner.lock().unwrap();
            while state.steer_calls == 0 {
                let (next, timeout) = h
                    .state
                    .ready
                    .wait_timeout(state, Duration::from_secs(5))
                    .unwrap();
                state = next;
                assert!(!timeout.timed_out());
            }
        }
        assert_eq!(
            h.handle
                .steer_pending_input(lease, turn, input.clone())
                .unwrap_err(),
            ErrorCode::Backpressure
        );
        assert_eq!(
            h.handle
                .send_pending_input(lease, input.clone())
                .unwrap_err(),
            ErrorCode::IdempotencyConflict
        );
        // Owner admission has happened. Native expected-turn validation now loses
        // to completion (or the transport loses the acknowledgment).
        h.handle.shared.lock().summary.active_turn_id = None;
        {
            let mut state = h.state.inner.lock().unwrap();
            state.steer_failure = Some(failure.clone());
            state.hold_steer = false;
        }
        h.state.ready.notify_all();
        assert_eq!(pending.join().unwrap().unwrap_err(), failure);
        assert_eq!(
            h.handle
                .steer_pending_input(lease, turn, input)
                .unwrap_err(),
            failure
        );
        let state = h.state.inner.lock().unwrap();
        assert_eq!(state.calls.len(), 1);
        assert_eq!(state.steer_calls, 1);
        assert!(state.steered.is_empty());
    }
}

#[test]
fn unsupported_fields_provider_and_stale_turn_refuse_before_native_dispatch() {
    let (h, lease, turn) = running();
    let mut unsupported = text_input("x");
    unsupported.fast = Some(false);
    assert_eq!(
        h.handle
            .steer_pending_input(lease, turn, unsupported)
            .unwrap_err(),
        ErrorCode::DeliveryUnsupported
    );
    assert_eq!(
        h.handle
            .steer_pending_input(lease, AgentTurnId::mint(), text_input("x"))
            .unwrap_err(),
        ErrorCode::InvalidTransition
    );
    h.handle.shared.lock().summary.capabilities = CapabilitySet::default();
    assert_eq!(
        h.handle
            .steer_pending_input(lease, turn, text_input("x"))
            .unwrap_err(),
        ErrorCode::DeliveryUnsupported
    );
    assert_eq!(h.state.inner.lock().unwrap().steer_calls, 0);
}

#[test]
fn ordinary_in_flight_and_uncertain_result_block_strict_reuse_of_the_key() {
    let (h, lease, turn) = running();
    h.state.inner.lock().unwrap().hold_steer = true;
    let input = text_input("ordinary amend");
    let sender = h.handle.clone();
    let sent = input.clone();
    let pending = std::thread::spawn(move || sender.send_pending_input(lease, sent));
    {
        let mut state = h.state.inner.lock().unwrap();
        while state.steer_calls == 0 {
            let (next, timeout) = h
                .state
                .ready
                .wait_timeout(state, Duration::from_secs(5))
                .unwrap();
            state = next;
            assert!(!timeout.timed_out());
        }
    }
    assert_eq!(
        h.handle
            .steer_pending_input(lease, turn, input.clone())
            .unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(
        h.handle
            .send_pending_input(lease, input.clone())
            .unwrap_err(),
        ErrorCode::Backpressure
    );
    {
        let mut state = h.state.inner.lock().unwrap();
        state.steer_failure = Some(ErrorCode::ProviderUnavailable);
        state.hold_steer = false;
    }
    h.state.ready.notify_all();
    assert_eq!(
        pending.join().unwrap().unwrap_err(),
        ErrorCode::ProviderUnavailable
    );
    assert_eq!(
        h.handle
            .steer_pending_input(lease, turn, input.clone())
            .unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(h.state.inner.lock().unwrap().steer_calls, 1);
    // Ordinary callers retain their established explicit retry behavior.
    h.state.inner.lock().unwrap().steer_failure = None;
    h.handle.send_pending_input(lease, input.clone()).unwrap();
    assert_eq!(h.state.inner.lock().unwrap().steer_calls, 2);
    assert_eq!(
        h.handle
            .steer_pending_input(lease, turn, input)
            .unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
}

#[test]
fn marker_saturation_refuses_new_strict_input_without_evicting_existing_replay() {
    let (h, lease, turn) = running();
    let input = text_input("strict");
    h.handle
        .steer_pending_input(lease, turn, input.clone())
        .unwrap();
    {
        let mut state = h.handle.shared.lock();
        for _ in 0..4096 {
            state.failed_ordinary_steers.insert(IdempotencyKey::mint());
        }
    }
    assert_eq!(
        h.handle
            .steer_pending_input(lease, turn, text_input("new"))
            .unwrap_err(),
        ErrorCode::Backpressure
    );
    h.handle.steer_pending_input(lease, turn, input).unwrap();
    {
        let mut state = h.state.inner.lock().unwrap();
        state.steer_failure = Some(ErrorCode::ProviderUnavailable);
    }
    h.handle
        .send_pending_input(lease, text_input("ordinary failure"))
        .unwrap_err();
    assert_eq!(h.handle.shared.lock().failed_ordinary_steers.len(), 4096);
    assert_eq!(h.state.inner.lock().unwrap().steer_calls, 2);
}
