// SPDX-License-Identifier: MPL-2.0

use super::*;

fn prepared() -> DecisionPreparedProjection {
    DecisionPreparedProjection {
        identity: DecisionProjectionIdentity {
            transition: id(70),
            run: id(4),
            wait: id(14),
            decision: Some(DecisionProjectionRevision {
                decision: id(15),
                revision: rev(2),
            }),
            sequence: rev(1),
            workspace: WorkspaceId::from_uuid(uuid(10)),
            document: DocumentId::from_uuid(uuid(71)),
            source_schema: 5.try_into().unwrap(),
            subject_digest: digest(1),
            kind: DecisionProjectionKind::Control,
            target_digest: digest(2),
        },
        target: DecisionBytes::new(b"{}".to_vec()).unwrap(),
        base_version: DecisionBytes::new(vec![1]).unwrap(),
        result_version: DecisionBytes::new(vec![2]).unwrap(),
        delta: DecisionBytes::new(vec![3]).unwrap(),
        delta_digest: digest(3),
    }
}
fn bookkeeping() -> DecisionSourceBookkeeping {
    DecisionSourceBookkeeping {
        boundary: DecisionSourceBoundary {
            fence: edit().origin(),
            producer_epoch: uuid(80),
            stream: DecisionSourceStream::Agent,
            sequence: StreamSequence::new(10).unwrap(),
            resulting: DecisionSourceFrontiers {
                agent_after: Some(StreamSequence::new(10).unwrap()),
                session_after: Some(StreamSequence::new(4).unwrap()),
            },
        },
        unflushed: DecisionList::new(vec![DecisionTurnTally {
            turn: id(7),
            originator: TurnOriginator::Orchestrator,
            tool_calls: 1,
            tool_failures: 0,
            failure_breakdown: ToolFailures::default(),
        }])
        .unwrap(),
        usage: Some(DecisionUsageSnapshot {
            id: id(81),
            revision: AmendmentRevision(1),
            event: id(82),
            attempt: id(5),
            turn: id(7),
            input: Some(10),
            output: None,
            cached_input: Some(2),
            cached_input_relationship: koru_session_protocol::CachedInputRelationship::Undeclared,
            model: id(83),
            completeness: DecisionUsageCompleteness::Complete {},
        }),
        allocated_steps: DecisionList::new(vec![id(84)]).unwrap(),
    }
}
#[test]
fn decision_prepared_projection_accepts_distinct_physical_document() {
    let p = prepared();
    assert_ne!(p.identity.document.as_uuid(), p.identity.run.as_uuid());
    assert_eq!(
        decode_decision_frame::<DecisionPreparedProjection>(&encode_decision_frame(&p).unwrap())
            .unwrap(),
        p
    );
    // This proves shape only. C/D must establish exact run_intents placement.
}
#[test]
fn decision_projection_preserves_opaque_bytes_and_bounds() {
    let mut p = prepared();
    p.delta = DecisionBytes::new(vec![255; DECISION_DELTA_BYTES]).unwrap();
    p.base_version = DecisionBytes::new(vec![255; 65_536]).unwrap();
    p.result_version = DecisionBytes::new(vec![255; 65_536]).unwrap();
    assert!(encode_decision_frame(&p).is_ok());
    assert!(DecisionPreparedDelta::new(vec![0; DECISION_DELTA_BYTES + 1]).is_err());
    assert!(serde_json::from_str::<DecisionSourceSchema>("2").is_err());
    assert!(serde_json::from_str::<DecisionSourceSchema>("6").is_err());
    let debug = format!("{p:?}");
    assert!(!debug.contains("255, 255"));
    assert!(debug.contains("REDACTED"));
}
#[test]
fn decision_open_projection_probe_checks_all_legal_two_body_shapes() {
    use DecisionProjectionKind as K;
    use DecisionProjectionPhase as P;
    let row = |sequence, kind, phase| DecisionOpenProjection {
        run: id(4),
        sequence: rev(sequence),
        kind,
        phase,
    };
    assert!(validate_open_decision_projections(&[]).is_ok());
    for first in [
        row(1, K::Source, P::Pending),
        row(1, K::Source, P::Prepared),
        row(1, K::Control, P::Pending),
        row(1, K::Control, P::Prepared),
    ] {
        assert!(validate_open_decision_projections(std::slice::from_ref(&first)).is_ok());
    }
    for first in [
        row(1, K::Source, P::Pending),
        row(1, K::Source, P::Prepared),
        row(1, K::Control, P::Prepared),
    ] {
        assert!(
            validate_open_decision_projections(&[first, row(2, K::Control, P::Pending)]).is_ok()
        );
    }
    for rows in [
        vec![
            row(1, K::Control, P::Pending),
            row(2, K::Source, P::Pending),
        ],
        vec![
            row(1, K::Source, P::Pending),
            row(2, K::Control, P::Prepared),
        ],
        vec![
            row(1, K::Control, P::Pending),
            row(2, K::Control, P::Pending),
        ],
        vec![
            row(1, K::Source, P::Prepared),
            row(2, K::Control, P::Prepared),
        ],
        vec![
            row(2, K::Source, P::Prepared),
            row(1, K::Control, P::Pending),
        ],
        vec![
            row(1, K::Source, P::Prepared),
            row(2, K::Control, P::Pending),
            row(3, K::Source, P::Pending),
        ],
    ] {
        assert!(
            validate_open_decision_projections(&rows).is_err(),
            "{rows:?}"
        );
    }
}
#[test]
fn decision_source_bookkeeping_keeps_both_cursors_tallies_and_usage() {
    let b = bookkeeping();
    let decoded =
        decode_decision_frame::<DecisionSourceBookkeeping>(&encode_decision_frame(&b).unwrap())
            .unwrap();
    assert_eq!(decoded, b);
    assert_ne!(
        b.boundary.resulting.agent_after,
        b.boundary.resulting.session_after
    );
    let fact = b.usage.as_ref().unwrap().fact();
    assert_eq!(fact.identity, id(81));
    assert_eq!(fact.input, Some(10));
    assert_eq!(fact.output, None);
    assert_eq!(b.unflushed.as_slice()[0].tool_calls, 1);
}
#[test]
fn decision_source_bookkeeping_rejects_lossy_failure_codec_and_extra_fields() {
    let base = json(&bookkeeping());
    let mut b = base.clone();
    b["unflushed"][0]["failure_breakdown"]["extra"] = true.into();
    assert!(decode::<DecisionSourceBookkeeping>(&b).is_err());
    let mut b = base.clone();
    b["usage"]["completeness"]["extra"] = true.into();
    assert!(decode::<DecisionSourceBookkeeping>(&b).is_err());
    let mut b = base.clone();
    b["unflushed"][0]["tool_failures"] = 1.into();
    assert!(decode::<DecisionSourceBookkeeping>(&b).is_err());
    let mut b = base;
    b["boundary"]["resulting"]["agent_after"] = 9.into();
    assert!(decode::<DecisionSourceBookkeeping>(&b).is_err());
}
#[test]
fn decision_source_aggregate_bound_refuses_instead_of_truncating() {
    let mut b = bookkeeping();
    let t = b.unflushed.as_slice()[0].clone();
    let mut tallies = Vec::new();
    for i in 0..1000 {
        let mut t = t.clone();
        t.turn = id(1000 + i);
        tallies.push(t);
    }
    b.unflushed = DecisionList::new(tallies).unwrap();
    assert_eq!(b.validate(), Err(DecisionError::TooLarge));
    assert_eq!(b.unflushed.as_slice().len(), 1000);
}
#[test]
fn decision_source_rejects_duplicate_allocated_steps() {
    let mut b = bookkeeping();
    b.allocated_steps = DecisionList::new(vec![id(84), id(84)]).unwrap();
    assert!(b.validate().is_err());
}

#[test]
fn decision_source_failure_snapshot_never_folds_or_discards_retained_entries() {
    let base = json(&bookkeeping());
    let entry = serde_json::json!({"tool":"other","failure":"unclassified","calls":1});
    let mut b = base.clone();
    b["unflushed"][0]["failure_breakdown"]["tallies"] = vec![entry.clone(); 9].into();
    assert!(decode::<DecisionSourceBookkeeping>(&b).is_err());
    let mut b = base.clone();
    b["unflushed"][0]["failure_breakdown"]["tallies"] = vec![entry.clone(); 2].into();
    assert!(decode::<DecisionSourceBookkeeping>(&b).is_err());
    let mut zero = entry;
    zero["calls"] = 0.into();
    let mut b = base;
    b["unflushed"][0]["failure_breakdown"]["tallies"] = vec![zero].into();
    assert!(decode::<DecisionSourceBookkeeping>(&b).is_err());
}
