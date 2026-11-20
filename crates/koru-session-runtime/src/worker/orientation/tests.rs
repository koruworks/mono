// SPDX-License-Identifier: MPL-2.0

use super::*;

fn snapshot(text: &str) -> Resolved {
    let mut value = koru_delegate_preamble::session::bundled();
    value.text = text.to_owned().into();
    value
}

#[test]
fn snapshot_is_immutable_and_only_a_new_session_uses_an_edit() {
    let mut file = snapshot("Original\n");
    let mut held = Orientation::new(Some(file.clone()));
    file.text = "Edited".into();
    let key = IdempotencyKey::mint();
    assert!(
        held.reserve(key, "first")
            .unwrap()
            .starts_with("Original\n")
    );
    held.accepted(key);
    assert_eq!(
        held.reserve(IdempotencyKey::mint(), "next").unwrap(),
        "next"
    );
    assert!(
        Orientation::new(Some(file))
            .reserve(key, "fresh")
            .unwrap()
            .starts_with("Edited")
    );
}

#[test]
fn unknown_carrier_stays_exact_and_blocks_distinct_input_even_after_local_retry_refusal() {
    let mut state = Orientation::new(Some(snapshot("Foundation")));
    let a = IdempotencyKey::mint();
    let b = IdempotencyKey::mint();
    let exact = state.reserve(a, "A").unwrap();
    state.failed(a, false);
    assert_eq!(state.reserve(b, "B"), Err(ErrorCode::Backpressure));
    assert_eq!(state.reserve(a, "A").unwrap(), exact);
    state.failed(a, true);
    assert_eq!(state.reserve(b, "B"), Err(ErrorCode::Backpressure));
    assert_eq!(state.reserve(a, "A").unwrap(), exact);
    state.accepted(a);
    assert_eq!(state.reserve(b, "B").unwrap(), "B");
}

#[test]
fn proven_preflight_failure_allows_b_then_retry_a_without_duplicate_orientation() {
    let mut state = Orientation::new(Some(snapshot("Foundation")));
    let a = IdempotencyKey::mint();
    let b = IdempotencyKey::mint();
    assert!(state.reserve(a, "A").unwrap().contains("Foundation"));
    state.failed(a, true);
    assert!(state.reserve(b, "B").unwrap().contains("Foundation"));
    state.accepted(b);
    assert_eq!(state.reserve(a, "A").unwrap(), "A");
}

#[test]
fn oversized_combination_is_refused_without_claiming_the_first_carrier() {
    let mut state = Orientation::new(Some(snapshot("Foundation")));
    assert_eq!(
        state.reserve(
            IdempotencyKey::mint(),
            &"x".repeat(crate::MAX_QUEUED_WRITE_BYTES)
        ),
        Err(ErrorCode::InputTooLarge)
    );
    assert!(
        state
            .reserve(IdempotencyKey::mint(), "fits")
            .unwrap()
            .contains("Foundation")
    );
}

#[test]
fn concurrent_first_inputs_have_one_owner() {
    let state = std::sync::Arc::new(std::sync::Mutex::new(Orientation::new(Some(snapshot(
        "Foundation",
    )))));
    let gate = std::sync::Arc::new(std::sync::Barrier::new(3));
    let tasks = (0..2)
        .map(|_| {
            let (state, gate) = (state.clone(), gate.clone());
            std::thread::spawn(move || {
                gate.wait();
                state
                    .lock()
                    .unwrap()
                    .reserve(IdempotencyKey::mint(), "request")
            })
        })
        .collect::<Vec<_>>();
    gate.wait();
    let answers = tasks
        .into_iter()
        .map(|task| task.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(answers.iter().filter(|answer| answer.is_ok()).count(), 1);
    assert_eq!(
        answers
            .iter()
            .filter(|answer| **answer == Err(ErrorCode::Backpressure))
            .count(),
        1
    );
}
