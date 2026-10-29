// SPDX-License-Identifier: MPL-2.0

use crate::*;

use super::state::sample_summary;

/// **v14 is one number and one capability.** The version says a peer can
/// read the family's tags; the capability says it may use them; the constant
/// pins the two together so a host gate cannot drift from the changelog.
#[test]
fn the_delegation_family_rides_protocol_fourteen_and_its_own_capability() {
    assert_eq!(MAX_PROTOCOL_VERSION, 17);
    assert_eq!(TOOL_DETAIL_PROTOCOL_VERSION, 17);
    assert_eq!(SESSION_ORIGIN_PROTOCOL_VERSION, 16);
    assert_eq!(PROVIDER_MODELS_PROTOCOL_VERSION, 15);
    assert_eq!(DELEGATION_PROTOCOL_VERSION, 14);
    assert!(capability::ALL.contains(&capability::DELEGATION));
    // The pinned count: a capability is append-only and a new one is a
    // decision, so this line is what makes adding the next one deliberate.
    // The thirty-first, `launch-resume`, rides no version: it gates a defaulted
    // `resume` flag on a creation (ADR-474).
    assert_eq!(capability::ALL.len(), 31);
    assert_eq!(
        capability::DELEGATION
            .parse::<CapabilityName>()
            .unwrap()
            .as_str(),
        "delegation"
    );
}

/// The lookup's four answers say what they are, and two of them carry the
/// session so a caller can reattach without a second round trip.
#[test]
fn creation_lookup_tags_are_stable_and_say_whether_something_runs() {
    let summary = sample_summary();
    for (lookup, tag, running) in [
        (
            CreationLookup::Known {
                session: summary.clone(),
            },
            "known",
            true,
        ),
        (CreationLookup::Creating, "creating", true),
        (
            CreationLookup::KnownEnded {
                session: summary.clone(),
            },
            "known_ended",
            false,
        ),
        (CreationLookup::NoEvidence, "no_evidence", false),
    ] {
        let value = serde_json::to_value(&lookup).unwrap();
        assert_eq!(value["kind"], tag);
        assert_eq!(lookup.is_running(), running, "{tag}");
        assert_eq!(
            serde_json::from_value::<CreationLookup>(value).unwrap(),
            lookup,
            "{tag} does not round trip"
        );
    }
}

/// **A cancellation without a reason is a run cancellation** — the meaning
/// every cancellation had before a takeover could end a step without ending
/// the job — and a run cancellation is never downgraded by a later takeover.
#[test]
fn the_cancel_reason_defaults_to_run_and_run_outranks_takeover() {
    let legacy: AgentEvent = serde_json::from_value(serde_json::json!({
        "kind": "turn_cancelled",
        "turn_id": super::turn_id().to_string(),
    }))
    .unwrap();
    assert_eq!(
        legacy,
        AgentEvent::TurnCancelled {
            turn_id: super::turn_id(),
            reason: CancelReason::Run,
        }
    );
    let takeover = serde_json::to_value(AgentEvent::TurnCancelled {
        turn_id: super::turn_id(),
        reason: CancelReason::TakeoverBoundary,
    })
    .unwrap();
    assert_eq!(takeover["reason"], "takeover_boundary");

    assert_eq!(
        CancelReason::Run.strongest(CancelReason::TakeoverBoundary),
        CancelReason::Run
    );
    assert_eq!(
        CancelReason::TakeoverBoundary.strongest(CancelReason::Run),
        CancelReason::Run
    );
    assert_eq!(
        CancelReason::TakeoverBoundary.strongest(CancelReason::TakeoverBoundary),
        CancelReason::TakeoverBoundary
    );
    assert_eq!(CancelReason::default(), CancelReason::Run);
    assert_eq!(TakeoverUrgency::default(), TakeoverUrgency::AtTurnBoundary);
}

/// Release, not preemption: the cycle goes one way, a crash can only ever
/// advance it, and nothing skips the person in the middle.
#[test]
fn the_handover_cycle_only_moves_forward() {
    use HandoverState::*;
    let legal = [
        (Idle, Requested),
        (Idle, Released),
        (Requested, Released),
        (Released, Taken),
        (Released, Idle),
        (Taken, Returned),
        (Returned, Idle),
    ];
    let all = [Idle, Requested, Released, Taken, Returned];
    for from in all {
        for to in all {
            let expected = from == to || legal.contains(&(from, to));
            assert_eq!(from.can_transition_to(to), expected, "{from:?} -> {to:?}");
        }
    }
    assert_eq!(HandoverState::default(), Idle);
}

/// **An unknown model round-trips as unknown without weakening validation.**
/// A usage report from a peer that predates model identity reads as "the
/// provider said nothing", the field is never invented, and the type that
/// carries a name still refuses what a name cannot be.
#[test]
fn model_identity_round_trips_unknown_and_stays_bounded() {
    let legacy: AgentUsage = serde_json::from_value(serde_json::json!({
        "input_tokens": 11,
        "output_tokens": 7,
        "cached_input_tokens": null,
    }))
    .unwrap();
    assert_eq!(legacy.model, None);
    assert_eq!(legacy.cached_input, CachedInputRelationship::Undeclared);
    assert_eq!(
        legacy.cached_input_tokens, None,
        "unavailable is never zero"
    );

    let reported = AgentUsage {
        input_tokens: Some(11),
        output_tokens: Some(7),
        cached_input_tokens: Some(3),
        cached_input: CachedInputRelationship::Included,
        model: Some("gpt-5.1-codex".parse().unwrap()),
    };
    let value = serde_json::to_value(&reported).unwrap();
    assert_eq!(value["model"], "gpt-5.1-codex");
    assert_eq!(value["cached_input"], "included");
    assert_eq!(
        serde_json::from_value::<AgentUsage>(value).unwrap(),
        reported
    );

    assert!("".parse::<ModelName>().is_err());
    assert!("has space".parse::<ModelName>().is_err());
    assert!("quoted\"name".parse::<ModelName>().is_err());
    assert!("x".repeat(129).parse::<ModelName>().is_err());
    assert!("x".repeat(128).parse::<ModelName>().is_ok());
    assert!(
        serde_json::from_value::<ModelName>(serde_json::json!("tab\there")).is_err(),
        "a control character is not a model name on the wire either"
    );

    let identity = ModelIdentity {
        requested: Some("claude-opus-4-1".parse().unwrap()),
        reported: None,
    };
    let value = serde_json::to_value(&identity).unwrap();
    assert_eq!(value["requested"], "claude-opus-4-1");
    assert!(value["reported"].is_null());
    assert_eq!(
        serde_json::from_value::<ModelIdentity>(serde_json::json!({})).unwrap(),
        ModelIdentity::default()
    );
}

/// A summary from a v13 host has neither field; a v14 summary read by a
/// v13 peer carries two it ignores. Both directions keep the row.
#[test]
fn a_pre_delegation_summary_reads_idle_with_no_model() {
    let mut value = serde_json::to_value(sample_summary()).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("handover").expect("written");
    object.remove("model").expect("written");
    let read: SessionSummary = serde_json::from_value(value).unwrap();
    assert_eq!(read.handover, HandoverState::Idle);
    assert_eq!(read.model, None);
    assert_eq!(read, sample_summary());
}

/// The one new session-event tag names the version it needs, and every
/// older tag still says one, so a host can filter for an older subscriber
/// instead of ending its connection with a frame it cannot read.
#[test]
fn only_the_handover_event_needs_protocol_fourteen() {
    assert_eq!(
        SessionEvent::HandoverChanged {
            handover: HandoverState::Released,
        }
        .minimum_protocol_version(),
        DELEGATION_PROTOCOL_VERSION
    );
    for event in [
        SessionEvent::Started,
        SessionEvent::Bell,
        SessionEvent::Output { byte_count: 1 },
        SessionEvent::Exited { code: None },
        SessionEvent::AttentionChanged {
            attention: AttentionState::default(),
        },
    ] {
        assert_eq!(event.minimum_protocol_version(), 1, "{event:?}");
    }
    let value = serde_json::to_value(SessionEvent::HandoverChanged {
        handover: HandoverState::Taken,
    })
    .unwrap();
    assert_eq!(value["kind"], "handover_changed");
    assert_eq!(value["handover"], "taken");
}

/// The refusal a retired key earns is a stable conflict, not prose.
#[test]
fn a_retired_creation_is_a_stable_conflict_code() {
    assert_eq!(ErrorCode::CreationRetired.as_str(), "creation_retired");
    assert_eq!(ErrorCode::CreationRetired.class(), ErrorClass::Conflict);
    assert!(ErrorCode::ALL.contains(&ErrorCode::CreationRetired));
    assert_eq!(
        serde_json::from_str::<ErrorCode>("\"creation_retired\"").unwrap(),
        ErrorCode::CreationRetired
    );
}
