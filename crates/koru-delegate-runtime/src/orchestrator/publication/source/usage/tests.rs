// SPDX-License-Identifier: MPL-2.0

//! O111 staging cases over pure values. No document, ledger or provider.

use super::*;
use crate::DecisionUsageLookup;
use koru_delegate_core::UsageFact;
use koru_delegate_core::decisions::DecisionProjectionIdentity;
use koru_session_protocol::CachedInputRelationship;

fn report(input: u64, relationship: CachedInputRelationship) -> AgentUsage {
    AgentUsage {
        input_tokens: Some(input),
        output_tokens: Some(7),
        cached_input_tokens: Some(2),
        cached_input: relationship,
        model: None,
    }
}

fn snapshot(attempt: RunAttemptId, turn: AgentTurnId, model: ModelRef) -> DecisionUsageSnapshot {
    DecisionUsageSnapshot {
        id: UsageFactId::mint(),
        revision: AmendmentRevision(3),
        event: EventId::mint(),
        attempt,
        turn,
        input: Some(10),
        output: Some(7),
        cached_input: Some(2),
        cached_input_relationship: CachedInputRelationship::Included,
        model,
        completeness: DecisionUsageCompleteness::Complete {},
    }
}

fn staged(stage: UsageStage) -> DecisionUsageSnapshot {
    match stage {
        UsageStage::Stage { snapshot, .. } => snapshot,
        UsageStage::Duplicate => panic!("expected a staged amendment"),
    }
}

#[test]
fn decision_usage_full_envelope_reuses_event_only_for_complete_equality() {
    let (attempt, turn, model) = (RunAttemptId::mint(), AgentTurnId::mint(), ModelRef::mint());
    let old = snapshot(attempt, turn, model);
    let recovered = RecoveredDecisionUsage::FullEnvelope {
        fact: old.fact(),
        snapshot: old.clone(),
    };
    let same = report(10, CachedInputRelationship::Included);
    assert_eq!(
        stage_usage(&recovered, UsageModel::Known(model), attempt, turn, &same).unwrap(),
        UsageStage::Duplicate
    );
    // Equal totals with a different declared relationship is a correction:
    // same identity, next checked revision, a new event.
    let relationship = report(10, CachedInputRelationship::Separate);
    let corrected = staged(
        stage_usage(
            &recovered,
            UsageModel::Known(model),
            attempt,
            turn,
            &relationship,
        )
        .unwrap(),
    );
    assert_eq!(corrected.id, old.id);
    assert_eq!(corrected.revision, AmendmentRevision(4));
    assert_ne!(corrected.event, old.event);
    assert_eq!(
        corrected.cached_input_relationship,
        CachedInputRelationship::Separate
    );
    // A different model with equal totals is not the same publication either.
    let moved = staged(
        stage_usage(
            &recovered,
            UsageModel::Known(ModelRef::mint()),
            attempt,
            turn,
            &same,
        )
        .unwrap(),
    );
    assert_eq!((moved.id, moved.revision), (old.id, AmendmentRevision(4)));
}

#[test]
fn decision_usage_applied_fact_never_claims_equality_from_totals() {
    let (attempt, turn, model) = (RunAttemptId::mint(), AgentTurnId::mint(), ModelRef::mint());
    let old = snapshot(attempt, turn, model);
    let fact: UsageFact = old.fact();
    let recovered = RecoveredDecisionUsage::AppliedFact(fact.clone());
    // The stored fact has the same totals, but no EventId or relationship:
    // the later stream observation is the next checked revision, new event.
    let next = staged(
        stage_usage(
            &recovered,
            UsageModel::Known(model),
            attempt,
            turn,
            &report(10, CachedInputRelationship::Included),
        )
        .unwrap(),
    );
    assert_eq!(next.id, fact.identity);
    assert_eq!(next.revision, AmendmentRevision(4));
    assert_ne!(next.event, old.event, "an old EventId is never invented");
    // Exhausted revisions and a recovered fact of another key both refuse.
    let mut exhausted = fact.clone();
    exhausted.revision = AmendmentRevision(u32::MAX);
    assert!(
        stage_usage(
            &RecoveredDecisionUsage::AppliedFact(exhausted),
            UsageModel::Known(model),
            attempt,
            turn,
            &report(10, CachedInputRelationship::Included),
        )
        .is_err()
    );
    assert!(
        stage_usage(
            &recovered,
            UsageModel::Known(model),
            attempt,
            AgentTurnId::mint(),
            &report(10, CachedInputRelationship::Included),
        )
        .is_err()
    );
}

#[test]
fn decision_usage_absent_mints_first_revision_and_only_unseen_model() {
    let (attempt, turn) = (RunAttemptId::mint(), AgentTurnId::mint());
    let provider: ProviderKind = "unknown".parse().unwrap();
    let UsageStage::Stage { snapshot, model } = stage_usage(
        &RecoveredDecisionUsage::Absent,
        UsageModel::Mint {
            provider: provider.clone(),
            requested: ModelReport::Unknown,
            reported: ModelReport::Unknown,
        },
        attempt,
        turn,
        &report(1, CachedInputRelationship::Undeclared),
    )
    .unwrap() else {
        panic!("absent usage stages")
    };
    assert_eq!(snapshot.revision, AmendmentRevision::FIRST);
    let model = model.expect("a positively absent identity is minted once");
    assert_eq!(snapshot.model, model.id);
    assert_eq!(model.provider, provider);
    // Undeclared relationship with counts stays explicitly incomplete.
    assert!(matches!(
        snapshot.completeness,
        DecisionUsageCompleteness::Incomplete { .. }
    ));
}

#[test]
fn decision_usage_cache_is_exact_bounded_and_upgraded_only_by_applied_envelope() {
    let (attempt, turn, model) = (RunAttemptId::mint(), AgentTurnId::mint(), ModelRef::mint());
    let provider: ProviderKind = "unknown".parse().unwrap();
    let old = snapshot(attempt, turn, model);
    let lookup = DecisionUsageLookup {
        attempt,
        turn,
        provider: provider.clone(),
        requested: ModelReport::Unknown,
        reported: ModelReport::Unknown,
    };
    let mut cache = SourceUsageCache::default();
    assert!(cache.usage(&(attempt, turn)).is_none());
    cache.record_observation(&DecisionUsageObservation {
        projection: projection_identity(),
        lookup: lookup.clone(),
        usage: RecoveredDecisionUsage::AppliedFact(old.fact()),
        model: None,
    });
    assert!(matches!(
        cache.usage(&(attempt, turn)),
        Some(RecoveredDecisionUsage::AppliedFact(_))
    ));
    let key = (provider.clone(), ModelReport::Unknown, ModelReport::Unknown);
    assert_eq!(cache.model(&key), Some(None), "positive absence is cached");
    let mut applied = old.clone();
    applied.revision = AmendmentRevision(4);
    let identity = ModelIdentity {
        id: model,
        provider: provider.clone(),
        requested: ModelReport::Unknown,
        reported: ModelReport::Unknown,
    };
    cache.install(&applied, std::slice::from_ref(&identity));
    assert!(matches!(
        cache.usage(&(attempt, turn)),
        Some(RecoveredDecisionUsage::FullEnvelope { snapshot, .. }) if *snapshot == applied
    ));
    assert_eq!(cache.model(&key), Some(Some(model)));
    for _ in 0..USAGE_KEYS {
        let mut other = applied.clone();
        other.turn = AgentTurnId::mint();
        cache.install(&other, &[]);
    }
    assert!(
        cache.usage(&(attempt, turn)).is_none(),
        "eviction forgets evidence; it never invents any"
    );
    cache.clear();
    assert_eq!(cache.model(&key), None);
}

fn projection_identity() -> DecisionProjectionIdentity {
    use koru_delegate_core::decisions::{
        BotWaitId, DecisionDigest, DecisionProjectionKind, DecisionRevision, DecisionTransitionId,
    };
    DecisionProjectionIdentity {
        transition: DecisionTransitionId::mint(),
        run: koru_delegate_core::RunId::mint(),
        wait: BotWaitId::mint(),
        decision: None,
        sequence: DecisionRevision::new(1).unwrap(),
        workspace: koru_delegate_core::WorkspaceId::new(),
        document: koru_delegate_core::DocumentId::new(),
        source_schema: 5u8.try_into().unwrap(),
        subject_digest: DecisionDigest::from_bytes([0; 32]),
        kind: DecisionProjectionKind::Source,
        target_digest: DecisionDigest::from_bytes([0; 32]),
    }
}
