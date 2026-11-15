// SPDX-License-Identifier: MPL-2.0

use super::*;

fn base(h: &Harness) -> DecisionPublicationBase {
    let f = h.origin();
    let admitted = h.db.bot_admission(f.run).unwrap().unwrap();
    let intent = h.db.intent(f.run).unwrap().unwrap();
    let mut header = RunHeader::from_request(admitted.submission.request, 777);
    header.bot = Some(admitted.submission.provenance);
    DecisionPublicationBase {
        workspace: intent.workspace,
        document: intent.run_document.unwrap(),
        source_schema: 3u8.try_into().unwrap(),
        observed_created_at_ms: 777,
        timeline_len: 0,
        subject_digest: decision_projection_subject_digest(&header).unwrap(),
        predecessor: DecisionProjectionPredecessor {
            projection: None,
            status: RunStatus::running(),
            failure_reason: None,
        },
        bookkeeping: None,
    }
}

fn source(h: &Harness, base: &DecisionPublicationBase, sequence: u64) -> RunProjectionTargetV1 {
    let sequence = StreamSequence::new(sequence).unwrap();
    RunProjectionTargetV1::Source {
        version: DecisionVersionV1,
        predecessor: base.predecessor.clone(),
        effects: DecisionProjectionEffects {
            event: None,
            status: base.predecessor.status.clone(),
            failure_reason: base.predecessor.failure_reason,
            economics_at_ms: None,
            steps: DecisionList::default(),
            models: DecisionList::default(),
            publications: DecisionList::default(),
        },
        bookkeeping: DecisionSourceBookkeeping {
            boundary: DecisionSourceBoundary {
                fence: h.origin(),
                producer_epoch: uuid::Uuid::new_v4(),
                stream: DecisionSourceStream::Agent,
                sequence,
                resulting: DecisionSourceFrontiers {
                    agent_after: Some(sequence),
                    session_after: None,
                },
            },
            unflushed: DecisionList::default(),
            usage: None,
            allocated_steps: DecisionList::default(),
        },
    }
}

fn prepared(record: &DecisionProjectionRecord) -> DecisionPreparedProjection {
    let delta = DecisionPreparedDelta::new(vec![5, 6]).unwrap();
    DecisionPreparedProjection {
        identity: record.identity.clone(),
        target: record.target.canonical_bytes().unwrap(),
        base_version: DecisionVersionVector::new(vec![1]).unwrap(),
        result_version: DecisionVersionVector::new(vec![2]).unwrap(),
        delta_digest: decision_projection_delta_digest(&delta).unwrap(),
        delta,
    }
}

fn apply(h: &mut Harness, record: &DecisionProjectionRecord) -> DecisionProjectionRecord {
    let prepared = prepared(record);
    h.db.record_decision_preparation(h.origin().owner, &prepared)
        .unwrap();
    h.db.acknowledge_decision_projection(
        h.origin().owner,
        &DecisionProjectionAcknowledgment {
            identity: record.identity.clone(),
            delta_digest: prepared.delta_digest,
            observed_version: DecisionVersionVector::new(vec![3]).unwrap(),
        },
    )
    .unwrap()
}

fn advance_base(base: &mut DecisionPublicationBase, record: &DecisionProjectionRecord) {
    base.source_schema = 5u8.try_into().unwrap();
    base.predecessor = DecisionProjectionPredecessor {
        projection: Some(DecisionProjectionPrior {
            transition: record.identity.transition,
            sequence: record.identity.sequence,
            target_digest: record.identity.target_digest,
        }),
        status: record.target.effects().status.clone(),
        failure_reason: record.target.effects().failure_reason,
    };
    base.bookkeeping = record.target.resulting_bookkeeping().unwrap();
}

#[test]
fn decision_projection_source_replays_before_fresh_state_and_retains_first_preparation() {
    let mut h = Harness::new();
    let decision = h.prepare();
    let base = base(&h);
    let target = source(&h, &base, 1);
    let id = DecisionTransitionId::mint();
    let owner = h.origin().owner;
    let record =
        h.db.record_decision_source(owner, id, decision.wait.wait, &base, &target)
            .unwrap();
    let first = prepared(&record);
    h.db.record_decision_preparation(owner, &first).unwrap();
    let mut changed = first.clone();
    changed.base_version = DecisionVersionVector::new(vec![99]).unwrap();
    assert!(h.db.record_decision_preparation(owner, &changed).is_err());
    let applied = apply(&mut h, &record);
    h.revoke();
    assert_eq!(
        h.db.record_decision_source(owner, id, decision.wait.wait, &base, &target)
            .unwrap(),
        applied
    );
    let replay =
        h.db.acknowledge_decision_projection(
            owner,
            &DecisionProjectionAcknowledgment {
                identity: record.identity.clone(),
                delta_digest: first.delta_digest,
                observed_version: DecisionVersionVector::new(vec![99]).unwrap(),
            },
        )
        .unwrap();
    assert_eq!(
        replay, applied,
        "later observed VV never replaces the first retained bytes"
    );
}

#[test]
fn decision_projection_source_reconnect_preserves_both_frontiers() {
    let mut h = Harness::new();
    let decision = h.prepare();
    let mut base = base(&h);
    let owner = h.origin().owner;
    let first_target = source(&h, &base, 8);
    let first =
        h.db.record_decision_source(
            owner,
            DecisionTransitionId::mint(),
            decision.wait.wait,
            &base,
            &first_target,
        )
        .unwrap();
    apply(&mut h, &first);
    advance_base(&mut base, &first);
    let mut next = source(&h, &base, 9); // a genuinely new physical pump epoch
    if let RunProjectionTargetV1::Source { bookkeeping, .. } = &mut next {
        bookkeeping.boundary.resulting.session_after = Some(StreamSequence::new(1).unwrap());
    }
    assert!(
        h.db.record_decision_source(
            owner,
            DecisionTransitionId::mint(),
            decision.wait.wait,
            &base,
            &next
        )
        .is_err()
    );
    let rewound = source(&h, &base, 8);
    assert!(
        h.db.record_decision_source(
            owner,
            DecisionTransitionId::mint(),
            decision.wait.wait,
            &base,
            &rewound
        )
        .is_err()
    );
    let next = source(&h, &base, 9);
    let retained =
        h.db.record_decision_source(
            owner,
            DecisionTransitionId::mint(),
            decision.wait.wait,
            &base,
            &next,
        )
        .unwrap();
    assert_eq!(retained.target, next);
    assert_eq!(h.count("bot_decision_publications"), 2);
}

#[test]
fn decision_projection_source_refuses_changed_subject_placement_and_pending_source() {
    let mut h = Harness::new();
    let decision = h.prepare();
    let base = base(&h);
    let target = source(&h, &base, 1);
    let owner = h.origin().owner;
    for changed in 0..3 {
        let mut bad = base.clone();
        match changed {
            0 => bad.document = fixtures::document(250),
            1 => bad.observed_created_at_ms += 1,
            _ => bad.subject_digest = DecisionDigest::from_bytes([99; 32]),
        }
        assert!(
            h.db.record_decision_source(
                owner,
                DecisionTransitionId::mint(),
                decision.wait.wait,
                &bad,
                &target
            )
            .is_err()
        );
    }
    assert_eq!(h.count("bot_decision_publications"), 0);
    h.db.record_decision_source(
        owner,
        DecisionTransitionId::mint(),
        decision.wait.wait,
        &base,
        &target,
    )
    .unwrap();
    assert!(
        h.db.record_decision_source(
            owner,
            DecisionTransitionId::mint(),
            decision.wait.wait,
            &base,
            &target
        )
        .is_err()
    );
    assert_eq!(h.count("bot_decision_publications"), 1);
}
