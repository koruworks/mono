// SPDX-License-Identifier: MPL-2.0

//! A steer over the ACP steering extension (Claude's wrapper):
//! `_session/steering` into the running turn, only where initialize
//! advertised it, text only, and `promptRequired` refused so the person's
//! message is sent as a normal message once the reply ends.

use super::*;

fn steering(outcome: &'static str) -> Harness {
    harness(
        FakeAcpConfig {
            steering: Some(outcome),
            ..Default::default()
        },
        None,
        None,
    )
}

fn steer_input(turn_id: AgentTurnId, text: &str) -> AgentInput {
    AgentInput {
        text: text.into(),
        skills: Vec::new(),
        ..input(turn_id)
    }
}

fn sent_steers(h: &Harness) -> Vec<Value> {
    h.state
        .lock()
        .unwrap()
        .sent()
        .iter()
        .filter(|frame| frame["method"] == "_session/steering")
        .cloned()
        .collect()
}

#[test]
fn a_steer_is_a_steering_request_into_the_running_turn() {
    let mut h = steering("injected");
    start(&mut h.control);
    let turn = AgentTurnId::mint();
    h.control.submit(input(turn)).unwrap();
    drain(&mut h.control);

    h.control
        .steer(steer_input(turn, "also check the tests"))
        .unwrap();

    let sent = sent_steers(&h);
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]["params"],
        json!({
            "sessionId": SESSION,
            "prompt": [{"type": "text", "text": "also check the tests"}],
            "_meta": {"steering": {"idleBehavior": "promptRequired"}},
        }),
        "the running session, the text as one block, and no detached turn when idle"
    );
    assert_eq!(h.state.lock().unwrap().prompts, 1, "no second prompt");
    // The running turn is still the one that ends.
    h.injected
        .lock()
        .unwrap()
        .push_back(response(3, json!({"stopReason":"end_turn"})));
    assert!(matches!(
        drain(&mut h.control).as_slice(),
        [AgentEvent::TurnCompleted { turn_id }] if *turn_id == turn
    ));
}

/// The wrapper answers `promptRequired` when its turn had already settled:
/// the steer is refused as any input during a turn used to be, and the
/// message goes as a normal send after the reply.
#[test]
fn prompt_required_is_refused_so_the_message_is_sent_normally() {
    let mut h = steering("promptRequired");
    start(&mut h.control);
    let turn = AgentTurnId::mint();
    h.control.submit(input(turn)).unwrap();
    drain(&mut h.control);
    let refused = h.control.steer(steer_input(turn, "later")).unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidTransition);
    assert_eq!(h.state.lock().unwrap().steers, 1);
    assert_eq!(h.state.lock().unwrap().prompts, 1, "nothing started here");
}

#[test]
fn an_agent_that_did_not_advertise_steering_keeps_the_refusal() {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    start(&mut h.control);
    let turn = AgentTurnId::mint();
    h.control.submit(input(turn)).unwrap();
    drain(&mut h.control);
    let refused = h.control.steer(steer_input(turn, "now")).unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidTransition);
    assert!(sent_steers(&h).is_empty(), "nothing was asked of the agent");
}

#[test]
fn only_the_running_turn_is_steered_and_only_with_text() {
    let mut h = steering("injected");
    start(&mut h.control);
    // No turn yet.
    let idle = h
        .control
        .steer(steer_input(AgentTurnId::mint(), "x"))
        .unwrap_err();
    assert_eq!(idle.code, ErrorCode::InvalidTransition);
    let turn = AgentTurnId::mint();
    h.control.submit(input(turn)).unwrap();
    drain(&mut h.control);
    // Another turn's id.
    let other = h
        .control
        .steer(steer_input(AgentTurnId::mint(), "x"))
        .unwrap_err();
    assert_eq!(other.code, ErrorCode::InvalidTransition);
    // A picture.
    let mut picture = steer_input(turn, "see this");
    picture.attachments = vec![AgentAttachment {
        id: koru_session_protocol::AttachmentId::mint(),
        kind: koru_session_protocol::AttachmentKind::Image,
        media_type: "image/png".into(),
    }];
    assert_eq!(
        h.control.steer(picture).unwrap_err().code,
        ErrorCode::DeliveryUnsupported
    );
    assert!(sent_steers(&h).is_empty());
}
