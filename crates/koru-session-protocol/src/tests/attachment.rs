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
        idempotency_key: super::idempotency_key(),
        text: "画像を確認 العربية".into(),
        attachment_ids: vec![first, second],
        mode: None,
        fast: None,
    };
    let json = serde_json::to_string(&input).unwrap();
    let back: PendingInput = serde_json::from_str(&json).unwrap();
    assert_eq!(back, input);
    assert_eq!(back.attachment_ids, [first, second]);
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
