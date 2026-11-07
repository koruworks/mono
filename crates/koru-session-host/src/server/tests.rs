// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_session_protocol::{
    AgentEvent, AgentEventObservation, AgentTurnId, ApprovalChoice, ApprovalChoiceId,
    ApprovalDecision, ApprovalId, ApprovalIntent, CapabilityName, CapabilitySet,
    SequencedAgentEvent, StreamSequence, capability,
};
use std::str::FromStr;

fn protocol(version: u32, managed: bool, dynamic: bool) -> ConnectionProtocol {
    ConnectionProtocol {
        version,
        attachments: false,
        quoted_paths: false,
        profile_launch: false,
        observer_output: false,
        observation_cursors: false,
        managed_sessions: true,
        managed_events: true,
        managed_approval: managed,
        presented_attention: false,
        projects: false,
        workspace_lifecycle: false,
        delegation: false,
        session_origin: false,
        launch_selection: false,
        ai_control: None,
        capabilities: CapabilitySet::new(if dynamic {
            vec![CapabilityName::from_str(capability::DYNAMIC_APPROVALS).unwrap()]
        } else {
            vec![]
        }),
    }
}

#[test]
fn dynamic_approvals_need_a_managed_protocol_and_negotiated_capability() {
    assert!(!protocol(7, true, true).dynamic_approvals());
    assert!(!protocol(14, true, false).dynamic_approvals());
    assert!(!protocol(14, false, true).dynamic_approvals());
    assert!(protocol(14, true, true).dynamic_approvals());
}

/// **Only a v21 peer that offered `managed-questions` gets an agent that
/// asks** (koru-ai D108): every other peer's sessions decline as before,
/// and its `answer_questions` is refused.
#[test]
fn questions_need_protocol_twenty_one_and_their_capability() {
    let with_questions = |version: u32| {
        let mut peer = protocol(version, true, false);
        peer.capabilities = CapabilitySet::new(vec![
            CapabilityName::from_str(capability::MANAGED_QUESTIONS).unwrap(),
        ]);
        peer
    };
    assert!(with_questions(21).questions());
    assert!(!with_questions(20).questions(), "a v20 peer is never asked");
    assert!(!protocol(21, true, true).questions(), "not offered");
    let mut unmanaged = with_questions(21);
    unmanaged.managed_approval = false;
    assert!(!unmanaged.questions());
    assert!(
        crate::protocol_offer()
            .capabilities
            .supports(capability::MANAGED_QUESTIONS),
        "the host offers it"
    );
}

/// **Only a v21 peer that offered `agent-commands` lists an agent's skills
/// or names them with an input** (koru-ai D121); the host offers it.
#[test]
fn agent_commands_need_protocol_twenty_one_and_their_capability() {
    let with_commands = |version: u32| {
        let mut peer = protocol(version, true, false);
        peer.capabilities = CapabilitySet::new(vec![
            CapabilityName::from_str(capability::AGENT_COMMANDS).unwrap(),
        ]);
        peer
    };
    assert!(with_commands(21).agent_commands());
    assert!(!with_commands(20).agent_commands(), "a v20 peer has none");
    assert!(!protocol(21, true, true).agent_commands(), "not offered");
    assert!(
        crate::protocol_offer()
            .capabilities
            .supports(capability::AGENT_COMMANDS),
        "the host offers it"
    );
}

/// **Only a v22 peer names a turn's model with its input**: the version
/// alone, no capability; an older peer's input with one is refused.
#[test]
fn a_turns_model_needs_protocol_twenty_two() {
    assert!(protocol(22, true, false).turn_model());
    assert!(
        !protocol(21, true, true).turn_model(),
        "a v21 peer has none"
    );
    assert_eq!(
        crate::protocol_offer().versions.max,
        koru_session_protocol::PATH_REFERENCES_PROTOCOL_VERSION,
        "the host offers it"
    );
}

#[test]
fn path_references_need_protocol_twenty_three_and_capability() {
    let with_references = |version: u32| {
        let mut peer = protocol(version, true, false);
        peer.capabilities = CapabilitySet::new(vec![
            CapabilityName::from_str(capability::PATH_REFERENCES).unwrap(),
        ]);
        peer
    };
    assert!(with_references(23).path_references());
    assert!(!with_references(22).path_references());
    assert!(!protocol(23, true, false).path_references());
    assert!(
        crate::protocol_offer()
            .capabilities
            .supports(capability::PATH_REFERENCES)
    );
}

fn events() -> Vec<SequencedAgentEvent> {
    let turn_id = AgentTurnId::mint();
    let approval_id = ApprovalId::mint();
    let session_id = SessionId::mint();
    let id = ApprovalChoiceId::mint();
    vec![
        SequencedAgentEvent {
            session_id,
            sequence: StreamSequence::new(10).unwrap(),
            event: AgentEvent::ApprovalRequired {
                turn_id,
                approval_id,
                summary: "<redacted>".into(),
                choices: vec![ApprovalChoice {
                    id,
                    intent: ApprovalIntent::AllowRemembered,
                    label: "<redacted>".into(),
                }],
            },
        },
        SequencedAgentEvent {
            session_id,
            sequence: StreamSequence::new(11).unwrap(),
            event: AgentEvent::ApprovalResolved {
                turn_id,
                approval_id,
                decision: ApprovalDecision::Approve,
                choice: Some(id),
            },
        },
    ]
}

/// Exercise the actual writer so a missed output branch cannot pass by testing
/// the sanitizer alone. Both channels remain open until every frame is read.
async fn written_frames(dynamic: bool) -> Vec<serde_json::Value> {
    let (host, mut peer) = tokio::io::duplex(16384);
    let (control_send, control) = mpsc::channel(4);
    let (subscription_send, subscriptions) = mpsc::channel(4);
    let retained = events();
    control_send
        .send(FromHost::Response {
            frame: ResponseFrame {
                id: 1,
                result: Ok(Response::AgentEventsObserved {
                    observation: AgentEventObservation {
                        events: retained.clone(),
                        cursor: StreamSequence::new(11).unwrap(),
                        earlier_truncated: true,
                    },
                }),
            },
        })
        .await
        .unwrap();
    for event in &retained {
        subscription_send
            .send(FromHost::Subscription {
                frame: SubscriptionFrame {
                    subscription_id: SubscriptionId(1),
                    update: SubscriptionUpdate::AgentEvent {
                        event: event.clone(),
                    },
                },
            })
            .await
            .unwrap();
    }
    let read = async move {
        let mut frames = Vec::new();
        for _ in 0..3 {
            let (_, frame) = read_frame::<_, serde_json::Value>(&mut peer).await.unwrap();
            frames.push(frame);
        }
        drop(control_send);
        drop(subscription_send);
        frames
    };
    let (_, frames) = tokio::join!(
        writer_loop(
            host,
            control,
            subscriptions,
            dynamic,
            koru_session_protocol::MAX_PROTOCOL_VERSION,
        ),
        read
    );
    // Per-reader projection must never erase the retained choices.
    assert!(
        matches!(&retained[0].event, AgentEvent::ApprovalRequired { choices, .. }
        if choices.len() == 1)
    );
    frames
}

#[tokio::test]
async fn old_readers_receive_legacy_live_replay_and_recovery_frames() {
    let frames = written_frames(false).await;
    let observed = &frames[0]["frame"]["result"]["Ok"]["observation"];
    assert_eq!(observed["cursor"], 11);
    assert_eq!(observed["earlier_truncated"], true);
    for event in observed["events"].as_array().unwrap() {
        assert!(event["event"].get("choices").is_none());
        assert!(event["event"].get("choice").is_none());
    }
    assert_eq!(observed["events"][1]["event"]["decision"], "approve");
    for frame in &frames[1..] {
        let event = &frame["frame"]["update"]["event"]["event"];
        assert!(event.get("choices").is_none());
        assert!(event.get("choice").is_none());
    }
}

#[tokio::test]
async fn negotiated_readers_keep_exact_choices_and_resolution_identity() {
    let frames = written_frames(true).await;
    let events = &frames[0]["frame"]["result"]["Ok"]["observation"]["events"];
    let id = &events[0]["event"]["choices"][0]["id"];
    assert!(id.is_string());
    assert_eq!(&events[1]["event"]["choice"], id);
    assert_eq!(
        &frames[1]["frame"]["update"]["event"]["event"]["choices"][0]["id"],
        id
    );
    assert_eq!(
        &frames[2]["frame"]["update"]["event"]["event"]["choice"],
        id
    );
}

/// The one agent event `writer_loop` writes for a peer at `version`.
async fn written_agent_event(event: AgentEvent, version: u32) -> serde_json::Value {
    let (host, mut peer) = tokio::io::duplex(16384);
    let (control_send, control) = mpsc::channel(4);
    let (subscription_send, subscriptions) = mpsc::channel(4);
    subscription_send
        .send(FromHost::Subscription {
            frame: SubscriptionFrame {
                subscription_id: SubscriptionId(1),
                update: SubscriptionUpdate::AgentEvent {
                    event: SequencedAgentEvent {
                        session_id: SessionId::mint(),
                        sequence: StreamSequence::new(7).unwrap(),
                        event,
                    },
                },
            },
        })
        .await
        .unwrap();
    let read = async move {
        let (_, frame) = read_frame::<_, serde_json::Value>(&mut peer).await.unwrap();
        drop(control_send);
        drop(subscription_send);
        frame
    };
    let (_, frame) = tokio::join!(
        writer_loop(host, control, subscriptions, true, version),
        read
    );
    frame["frame"]["update"]["event"].clone()
}

/// **`notice` reaches only a v20 reader** (koru-ai D69); a v19 one gets an
/// empty delta of the same turn and sequence: the notice is dropped for it,
/// never made part of the reply.
#[tokio::test]
async fn an_older_reader_gets_an_empty_delta_for_a_notice() {
    let turn_id = AgentTurnId::mint();
    let notice = || AgentEvent::Notice {
        turn_id,
        text: "Hook failed: exited with code 127".into(),
    };
    let current = written_agent_event(notice(), 20).await;
    assert_eq!(current["event"]["kind"], "notice");
    assert_eq!(
        current["event"]["text"],
        "Hook failed: exited with code 127"
    );
    let older = written_agent_event(notice(), 19).await;
    assert_eq!(older["sequence"], 7);
    assert_eq!(older["event"]["kind"], "message_delta");
    assert_eq!(older["event"]["text"], "");
    // A mode change still reaches a v19 reader.
    let mode = written_agent_event(
        AgentEvent::ModeChanged {
            turn_id,
            mode: "plan".parse().unwrap(),
        },
        19,
    )
    .await;
    assert_eq!(mode["event"]["kind"], "mode_changed");
}

/// **An agent's questions reach only a v21 reader** (koru-ai D108); a v20
/// one gets an empty delta of the same turn and sequence for each, which
/// reads as nothing — and a notice still reaches it.
#[tokio::test]
async fn an_older_reader_gets_an_empty_delta_for_questions() {
    let turn_id = AgentTurnId::mint();
    let request_id = koru_session_protocol::QuestionRequestId::mint();
    let asked = || AgentEvent::QuestionsRequired {
        turn_id,
        request_id,
        questions: vec![koru_session_protocol::AgentQuestion {
            id: "q".into(),
            header: String::new(),
            question: "Which one?".into(),
            options: Vec::new(),
            allow_other: false,
            secret: false,
        }],
        blocking: true,
        delivery: koru_session_protocol::QuestionsDelivery::InTurn,
    };
    let closed = || AgentEvent::QuestionsResolved {
        turn_id,
        request_id,
        outcome: koru_session_protocol::QuestionsOutcome::Skipped,
    };
    let current = written_agent_event(asked(), 21).await;
    assert_eq!(current["event"]["kind"], "questions_required");
    assert_eq!(current["event"]["questions"][0]["question"], "Which one?");
    assert_eq!(
        written_agent_event(closed(), 21).await["event"]["kind"],
        "questions_resolved"
    );
    // A set answered by the next message (Codex's async questions) says so
    // to a v21 reader, and is the same empty delta to a v20 one.
    let next_message = || {
        let AgentEvent::QuestionsRequired {
            turn_id,
            request_id,
            questions,
            ..
        } = asked()
        else {
            unreachable!("a set")
        };
        AgentEvent::QuestionsRequired {
            turn_id,
            request_id,
            questions,
            blocking: false,
            delivery: koru_session_protocol::QuestionsDelivery::NextMessage,
        }
    };
    assert_eq!(
        written_agent_event(next_message(), 21).await["event"]["delivery"],
        "next_message"
    );
    for event in [asked(), closed(), next_message()] {
        let older = written_agent_event(event, 20).await;
        assert_eq!(older["sequence"], 7);
        assert_eq!(older["event"]["kind"], "message_delta");
        assert_eq!(older["event"]["text"], "");
    }
    let notice = written_agent_event(
        AgentEvent::Notice {
            turn_id,
            text: "Hook failed".into(),
        },
        20,
    )
    .await;
    assert_eq!(notice["event"]["kind"], "notice");
}

/// **`mode_changed` reaches only a v19 reader** (koru-ai D7); an older one
/// gets an empty delta of the same turn and sequence, which reads as nothing.
#[tokio::test]
async fn an_older_reader_gets_an_empty_delta_for_a_mode_change() {
    let mode = || AgentEvent::ModeChanged {
        turn_id: AgentTurnId::mint(),
        mode: "acceptEdits".parse().unwrap(),
    };
    let current = written_agent_event(mode(), 19).await;
    assert_eq!(current["event"]["kind"], "mode_changed");
    assert_eq!(current["event"]["mode"], "acceptEdits");
    let older = written_agent_event(mode(), 18).await;
    assert_eq!(older["sequence"], 7);
    assert_eq!(older["event"]["kind"], "message_delta");
    assert_eq!(older["event"]["text"], "");
}
