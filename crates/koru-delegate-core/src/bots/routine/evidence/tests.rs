// SPDX-License-Identifier: MPL-2.0
use super::*;

fn record() -> RoutineClaimRecord {
    let p = super::super::tests::prepared();
    RoutineClaimRecord {
        claim: RoutineClaimIdentity {
            slot: p.slot,
            definition: p.definition,
            ids: p.ids,
            preparation_hash: p.preparation_hash,
            claimed_at_ms: 11,
        },
        submission_version: 2,
        submission: p.submission,
        link_state: RoutineLinkState::Held {},
        updated_at_ms: 11,
    }
}
#[test]
fn routine_claim_record_rejects_mixed_winner_payload_and_link_identity() {
    let r = record();
    assert!(r.valid());
    let ack = RoutineLinkAcknowledgement {
        claim: r.claim.clone(),
        outcome: RoutineLinkOutcome::Linked {},
    };
    assert!(r.accepts(&ack));
    let mut changed = ack.clone();
    changed.claim.preparation_hash = RoutineDigest32::from_bytes([9; 32]);
    assert!(!r.accepts(&changed));
    changed = ack.clone();
    changed.claim.claimed_at_ms += 1;
    assert!(!r.accepts(&changed));
    let mut mixed = r.clone();
    mixed.submission.provenance.chat_id = mixed.claim.ids.user_message;
    assert!(!mixed.valid());
    mixed = r.clone();
    mixed.submission_version = 1;
    assert!(!mixed.valid());
    mixed = r;
    mixed.claim.definition.owner_handoff = Some(HandoffId::mint());
    assert!(!mixed.valid());
}
#[test]
fn routine_link_states_are_monotonic_and_terminal_never_reopens() {
    let mut r = record();
    let linked = RoutineLinkAcknowledgement {
        claim: r.claim.clone(),
        outcome: RoutineLinkOutcome::Linked {},
    };
    let terminal = RoutineLinkAcknowledgement {
        claim: r.claim.clone(),
        outcome: RoutineLinkOutcome::Terminal {
            reason: RoutineTerminalReason::Stopped,
        },
    };
    assert!(r.accepts(&linked));
    assert!(r.accepts(&terminal));
    r.link_state = RoutineLinkState::Linked {};
    assert!(r.accepts(&linked));
    assert!(r.accepts(&terminal));
    r.link_state = terminal.outcome.state();
    assert!(!r.accepts(&linked));
    assert!(r.accepts(&terminal));
    let other = RoutineLinkAcknowledgement {
        claim: r.claim.clone(),
        outcome: RoutineLinkOutcome::Terminal {
            reason: RoutineTerminalReason::AlreadyTerminal,
        },
    };
    assert!(!r.accepts(&other));
}
#[test]
fn routine_observation_distinguishes_positive_absence_skip_and_claim() {
    let r = record();
    let slot = r.claim.slot;
    let absent = RoutineClaimObservation::Absent {
        slot,
        observed_at_ms: 60_010,
    };
    let skip = RoutineClaimObservation::Skipped {
        record: RoutineSkippedClaim {
            slot,
            definition_revision: 1,
            planned_run: r.claim.ids.run,
            outcome: RoutineSkipOutcome::SkippedBusy {
                holder: RunId::mint(),
            },
            recorded_at_ms: 12,
        },
    };
    for value in [absent, skip, RoutineClaimObservation::Claimed { record: r }] {
        assert!(value.valid());
        assert_eq!(value.slot(), &slot);
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            serde_json::from_slice::<RoutineClaimObservation>(&bytes).unwrap(),
            value
        );
    }
    assert!(
        serde_json::from_str::<RoutineClaimObservation>(r#"{"observation":"unavailable"}"#)
            .is_err()
    );
    assert!(serde_json::from_str::<RoutineSkipOutcome>(r#"{"outcome":"skipped_busy"}"#).is_err());
    assert!(
        serde_json::from_str::<RoutineLinkState>(r#"{"state":"held","caller_authorized":true}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<RoutineSkipOutcome>(r#"{"outcome":"skipped_missed","holder":null}"#)
            .is_err()
    );
}
#[test]
fn routine_route_preserves_store_uuid_domain_and_binds_complete_identity() {
    let p = super::super::tests::prepared();
    let mut route = RoutineRouteIdentity {
        owner: p.slot.owner,
        bot: p.slot.bot,
        schedule: p.slot.schedule,
        workspace: WorkspaceId::from_uuid(Uuid::nil()),
        creation_request: p.ids.submission,
        seed_hash: p.semantic_hash,
        created_at_ms: 0,
    };
    assert!(route.matches_slot(&p.slot));
    route.owner.user_scope = crate::bots::UserScopeId::from_uuid(Uuid::nil());
    assert!(route.valid());
    assert!(!route.matches_slot(&p.slot));
    route.creation_request = BotRequestId::from_uuid(Uuid::nil());
    assert!(!route.valid());
}
