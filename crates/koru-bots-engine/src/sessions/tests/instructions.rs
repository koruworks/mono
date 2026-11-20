// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn common_edit_changes_context_hash_without_reoffering_a_proven_old_input() {
    let f = Fixture::new();
    f.port.instructions.lock().unwrap().text = "COMMON-BEFORE".into();
    f.plan(Plan {
        lost: true,
        ..Default::default()
    });
    assert!(f.run().is_err());
    let offered = f.port.offers.lock().unwrap()[0].id;
    assert!(
        f.input(0)["context"]["instructions"]
            .as_str()
            .unwrap()
            .starts_with("COMMON-BEFORE\n\n")
    );
    f.port.instructions.lock().unwrap().text = "COMMON-AFTER".into();
    f.run().unwrap();
    let next = f.input(1);
    assert!(
        next["context"]["instructions"]
            .as_str()
            .unwrap()
            .starts_with("COMMON-AFTER\n\n")
    );
    assert!(next["delivery"]["work"].as_array().unwrap().is_empty());
    assert_eq!(next["delivery"]["resume_accepted"][0], offered.to_string());
    assert_eq!(f.records.lock().unwrap().starts, [false, true]);
    // A matching old journal proof settles only the offered old work. It is
    // not proof that the provider received the changed instruction snapshot.
    let state = f
        .database
        .lock()
        .unwrap()
        .latest(f.thread, "scripted")
        .unwrap()
        .unwrap();
    let saved: Saved = serde_json::from_slice(&state.resume_state).unwrap();
    assert_eq!(
        saved.context_delivery,
        Some(f.port.offers.lock().unwrap()[1].id)
    );
}

#[test]
fn common_bytes_are_inside_the_exact_input_proof_and_fallback_uses_a_notice() {
    let f = Fixture::new();
    {
        let mut source = f.port.instructions.lock().unwrap();
        source.text = "  CUSTOM\r\n".into();
        source.notice = Some(koru_delegate_preamble::Notice::Unreadable);
    }
    f.run().unwrap();
    let text = f.records.lock().unwrap().inputs[0].clone();
    assert!(
        f.input(0)["context"]["instructions"]
            .as_str()
            .unwrap()
            .starts_with("  CUSTOM\r\n\n\n")
    );
    let state = f
        .database
        .lock()
        .unwrap()
        .latest(f.thread, "scripted")
        .unwrap()
        .unwrap();
    let saved: Saved = serde_json::from_slice(&state.resume_state).unwrap();
    assert_eq!(
        saved.inputs[0].hash,
        *blake3::hash(text.as_bytes()).as_bytes()
    );
    assert!(
        f.port
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, AgentEvent::Notice { .. }))
    );
    assert!(!text.contains("private/prompts"));
}
