// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_session_protocol::AgentTurnId;

fn sequence(value: u64) -> StreamSequence {
    StreamSequence::new(value).unwrap()
}

#[test]
fn observation_is_a_bounded_tail_with_an_atomic_resume_cursor() {
    let stats = SessionStats::new(true);
    let session_id = SessionId::mint();
    let turn_id = AgentTurnId::mint();
    let mut stream = AgentStreamState::new(sequence(1));
    for _ in 0..300 {
        stream.publish(
            session_id,
            AgentEvent::MessageDelta {
                turn_id,
                text: "x".into(),
            },
            &stats,
        );
    }

    let observation = stream.observe();
    assert_eq!(observation.events.len(), REPLAY_EVENT_CAPACITY);
    assert!(observation.earlier_truncated);
    assert_eq!(observation.cursor, sequence(301));

    let subscriber = stream.subscribe(Some(observation.cursor), &stats);
    stream.publish(session_id, AgentEvent::WaitingForInput { turn_id }, &stats);
    assert!(matches!(
        subscriber.recv_timeout(std::time::Duration::from_millis(10)),
        Ok(AgentEventUpdate::Event(SequencedAgentEvent { sequence: next, .. }))
            if next == sequence(302)
    ));
}

#[test]
fn a_complete_short_observation_is_not_marked_truncated() {
    let stats = SessionStats::new(false);
    let mut stream =
        AgentStreamState::new(sequence(koru_session_protocol::AGENT_EVENT_INITIAL_CURSOR));
    let empty = stream.observe();
    assert!(empty.events.is_empty());
    assert_eq!(
        empty.cursor.get(),
        koru_session_protocol::AGENT_EVENT_INITIAL_CURSOR
    );
    assert!(!empty.earlier_truncated);
    stream.publish(
        SessionId::mint(),
        AgentEvent::TurnStarted {
            turn_id: AgentTurnId::mint(),
        },
        &stats,
    );
    let observation = stream.observe();
    assert_eq!(observation.events.len(), 1);
    assert!(!observation.earlier_truncated);
    assert_eq!(
        observation.cursor.get(),
        koru_session_protocol::AGENT_EVENT_FIRST_SEQUENCE
    );
    assert_eq!(observation.events[0].sequence, observation.cursor);
}

#[test]
fn approval_labels_evict_replay_and_force_old_cursors_to_resynchronize() {
    use koru_session_protocol::{ApprovalChoice, ApprovalChoiceId, ApprovalId, ApprovalIntent};
    let stats = SessionStats::new(false);
    let session_id = SessionId::mint();
    let turn_id = AgentTurnId::mint();
    let mut stream = AgentStreamState::new(sequence(1));
    let offer = || AgentEvent::ApprovalRequired {
        turn_id,
        approval_id: ApprovalId::mint(),
        summary: String::new(),
        choices: vec![ApprovalChoice {
            id: ApprovalChoiceId::mint(),
            intent: ApprovalIntent::Reject,
            label: "x".repeat(REPLAY_BYTE_CAPACITY / 2),
        }],
    };
    stream.publish(session_id, offer(), &stats);
    stream.publish(session_id, offer(), &stats);
    let observation = stream.observe();
    assert_eq!(observation.events.len(), 1);
    assert_eq!(observation.events[0].sequence, sequence(3));
    assert!(observation.earlier_truncated);
    assert!(stream.replay_bytes <= REPLAY_BYTE_CAPACITY);
    let subscriber = stream.subscribe(Some(sequence(1)), &stats);
    assert!(matches!(
        subscriber.recv_timeout(std::time::Duration::from_millis(10)),
        Ok(AgentEventUpdate::Gap { .. })
    ));
    let mut oversized = offer();
    if let AgentEvent::ApprovalRequired { choices, .. } = &mut oversized {
        choices[0].label = "x".repeat(REPLAY_BYTE_CAPACITY);
    }
    stream.publish(session_id, oversized, &stats);
    assert!(stream.observe().events.is_empty());
    assert_eq!(stream.replay_bytes, 0);
}
