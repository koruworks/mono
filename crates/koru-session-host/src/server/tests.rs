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
        writer_loop(host, control, subscriptions, dynamic, true),
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

/// **`mode_changed` reaches only a v19 reader** (koru-ai D7); an older one
/// gets an empty delta of the same turn and sequence, which reads as nothing.
#[tokio::test]
async fn an_older_reader_gets_an_empty_delta_for_a_mode_change() {
    async fn written(agent_modes: bool) -> serde_json::Value {
        let (host, mut peer) = tokio::io::duplex(16384);
        let (control_send, control) = mpsc::channel(4);
        let (subscription_send, subscriptions) = mpsc::channel(4);
        let turn_id = AgentTurnId::mint();
        subscription_send
            .send(FromHost::Subscription {
                frame: SubscriptionFrame {
                    subscription_id: SubscriptionId(1),
                    update: SubscriptionUpdate::AgentEvent {
                        event: SequencedAgentEvent {
                            session_id: SessionId::mint(),
                            sequence: StreamSequence::new(7).unwrap(),
                            event: AgentEvent::ModeChanged {
                                turn_id,
                                mode: "acceptEdits".parse().unwrap(),
                            },
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
            writer_loop(host, control, subscriptions, true, agent_modes),
            read
        );
        frame["frame"]["update"]["event"].clone()
    }
    let current = written(true).await;
    assert_eq!(current["event"]["kind"], "mode_changed");
    assert_eq!(current["event"]["mode"], "acceptEdits");
    let older = written(false).await;
    assert_eq!(older["sequence"], 7);
    assert_eq!(older["event"]["kind"], "message_delta");
    assert_eq!(older["event"]["text"], "");
}
