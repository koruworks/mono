// SPDX-License-Identifier: MPL-2.0

use crate::*;

fn validation_error() -> AttachmentValidationError {
    AttachmentValidationError {
        code: ErrorCode::InvalidImage,
        message: "decoder rejected payload".into(),
    }
}

#[test]
fn attachment_ingest_transition_table_is_exhaustive() {
    let states = [
        AttachmentStatus::Ingesting,
        AttachmentStatus::Pending,
        AttachmentStatus::Rejected {
            error: validation_error(),
        },
        AttachmentStatus::Removed,
    ];
    let expected = [
        [true, true, true, true],
        [false, true, false, true],
        [false, false, true, true],
        [false, false, false, true],
    ];
    for (from_index, from) in states.iter().enumerate() {
        for (to_index, to) in states.iter().enumerate() {
            assert_eq!(
                from.can_transition_to(to),
                expected[from_index][to_index],
                "{from:?} -> {to:?}"
            );
        }
    }
}

#[test]
fn delivery_transition_preserves_one_turn() {
    let turn = super::turn_id();
    let other: AgentTurnId = super::id("66666666-6666-4666-8666-666666666666");
    let pending = DeliveryState::Pending;
    let delivering = DeliveryState::Delivering { turn_id: turn };
    let delivered = DeliveryState::Delivered { turn_id: turn };
    let failed = DeliveryState::Failed {
        turn_id: turn,
        error: SessionFailure {
            code: ErrorCode::DeliveryFailed,
            message: "provider rejected input".into(),
        },
    };
    assert!(pending.can_transition_to(&delivering));
    assert!(delivering.can_transition_to(&delivered));
    assert!(delivering.can_transition_to(&failed));
    assert!(failed.can_transition_to(&delivering));
    assert!(!delivered.can_transition_to(&delivering));
    assert!(!delivering.can_transition_to(&DeliveryState::Delivered { turn_id: other }));
}

#[test]
fn delivery_transition_table_is_exhaustive_for_one_turn() {
    let turn = super::turn_id();
    let states = [
        DeliveryState::Pending,
        DeliveryState::Delivering { turn_id: turn },
        DeliveryState::Delivered { turn_id: turn },
        DeliveryState::Failed {
            turn_id: turn,
            error: SessionFailure {
                code: ErrorCode::DeliveryFailed,
                message: "delivery rejected".into(),
            },
        },
    ];
    let expected = [
        [true, true, false, false],
        [false, true, true, true],
        [false, false, true, false],
        [false, true, false, true],
    ];
    for (from_index, from) in states.iter().enumerate() {
        for (to_index, to) in states.iter().enumerate() {
            assert_eq!(
                from.can_transition_to(to),
                expected[from_index][to_index],
                "{from:?} -> {to:?}"
            );
        }
    }
}

#[test]
fn turn_association_is_immutable_and_replay_safe() {
    let mut attachment = sample_attachment();
    let turn = super::turn_id();
    let other: AgentTurnId = super::id("66666666-6666-4666-8666-666666666666");
    assert_eq!(attachment.associate_turn(turn), Ok(()));
    assert_eq!(attachment.associate_turn(turn), Ok(()));
    assert_eq!(
        attachment.associate_turn(other),
        Err(ErrorCode::InvalidTransition)
    );
}

#[test]
fn pending_input_preserves_attachment_order_and_unicode() {
    let first = super::attachment_id();
    let second: AttachmentId = super::id("77777777-7777-4777-8777-777777777777");
    let input = PendingInput {
        model: None,
        idempotency_key: super::idempotency_key(),
        text: "画像を確認 العربية".into(),
        attachment_ids: vec![first, second],
        mode: None,
        fast: None,
        reasoning: None,
        skills: Vec::new(),
    };
    let json = serde_json::to_string(&input).unwrap();
    let back: PendingInput = serde_json::from_str(&json).unwrap();
    assert_eq!(back, input);
    assert_eq!(back.attachment_ids, [first, second]);
}

/// **A turn's level is v20 and leaves no trace when unset** (koru-ai D70):
/// an input without one is the v19 shape byte for byte, and one with a
/// level says so — the client's gate reads `needs_turn_reasoning`.
#[test]
fn a_turns_level_is_written_only_when_chosen() {
    let mut input = PendingInput {
        model: None,
        idempotency_key: super::idempotency_key(),
        text: "go on".into(),
        attachment_ids: Vec::new(),
        mode: None,
        fast: None,
        reasoning: None,
        skills: Vec::new(),
    };
    let plain = serde_json::to_value(&input).unwrap();
    assert!(plain.get("reasoning").is_none());
    assert!(!input.needs_turn_reasoning());
    input.reasoning = Some(LaunchReasoning::XHigh);
    let json = serde_json::to_value(&input).unwrap();
    assert_eq!(json["reasoning"], "xhigh");
    assert!(input.needs_turn_reasoning());
    assert!(!input.needs_agent_modes());
    let back: PendingInput = serde_json::from_value(json).unwrap();
    assert_eq!(back, input);
}

/// **A turn's model is v22 and leaves no trace when unset**: an input
/// without one is the v21 shape byte for byte, and one with a model says
/// so — the client's and the host's gates read `needs_turn_model`.
#[test]
fn a_turns_model_is_written_only_when_chosen() {
    let mut input = PendingInput {
        idempotency_key: super::idempotency_key(),
        text: "go on".into(),
        attachment_ids: Vec::new(),
        mode: None,
        fast: None,
        reasoning: None,
        skills: Vec::new(),
        model: None,
    };
    let plain = serde_json::to_value(&input).unwrap();
    assert!(plain.get("model").is_none());
    assert!(!input.needs_turn_model());
    input.model = Some("opus[1m]".parse().unwrap());
    let json = serde_json::to_value(&input).unwrap();
    assert_eq!(json["model"], "opus[1m]");
    assert!(input.needs_turn_model());
    assert!(!input.needs_turn_reasoning() && !input.needs_agent_modes());
    let back: PendingInput = serde_json::from_value(json).unwrap();
    assert_eq!(back, input);
    // A v21 peer's input, without the field, reads as no model.
    let older: PendingInput = serde_json::from_value(plain).unwrap();
    assert_eq!(older.model, None);
    assert!(TURN_MODEL_PROTOCOL_VERSION > AGENT_COMMANDS_PROTOCOL_VERSION);
}

/// **A notice is bounded and blank is nothing** (koru-ai D69).
#[test]
fn an_agent_notice_is_bounded_and_trimmed() {
    assert_eq!(bound_agent_notice("  \n "), None);
    assert_eq!(
        bound_agent_notice("  Hook failed  ").as_deref(),
        Some("Hook failed")
    );
    let long = "é".repeat(AGENT_NOTICE_CHARS + 20);
    let bounded = bound_agent_notice(&long).unwrap();
    assert_eq!(bounded.chars().count(), AGENT_NOTICE_CHARS);
    assert!(bounded.ends_with('…'));
    let notice = AgentEvent::Notice {
        turn_id: AgentTurnId::mint(),
        text: "Hook failed".into(),
    };
    assert_eq!(notice.minimum_protocol_version(), 20);
    assert_eq!(serde_json::to_value(&notice).unwrap()["kind"], "notice");
}

fn sample_attachment() -> Attachment {
    Attachment {
        id: super::attachment_id(),
        session_id: super::session_id(),
        kind: AttachmentKind::Image,
        source: AttachmentSource::Clipboard,
        media_type: "image/png".into(),
        byte_count: 12,
        dimensions: Some(ImageDimensions {
            width: 2,
            height: 3,
        }),
        status: AttachmentStatus::Pending,
        delivery_mode: DeliveryMode::Unsupported,
        delivery: DeliveryState::Pending,
        turn_id: None,
        preview_available: true,
        created_at_ms: 9,
    }
}
