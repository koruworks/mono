// SPDX-License-Identifier: MPL-2.0

//! O111 usage staging inside the exclusive window. The cache holds only exact
//! keys learned from the fixed worker's cold lookup or from this producer's own
//! applied Source envelope. It is never filled from totals, from the ordinary
//! retry identity, or by inventing an old EventId or cached relationship.

use crate::orchestrator::usage::completeness_of;
use crate::{DecisionUsageObservation, RecoveredDecisionUsage};
use koru_delegate_core::decisions::{
    DecisionError, DecisionUsageCompleteness, DecisionUsageSnapshot,
};
use koru_delegate_core::{
    AgentTurnId, AmendmentRevision, Completeness, EventId, ModelIdentity, ModelRef, ModelReport,
    ProviderKind, RunAttemptId, UsageFactId,
};
use koru_session_protocol::AgentUsage;

#[cfg(test)]
mod tests;

/// A Run has few turns reporting usage at once. Eviction only forgets
/// evidence, which forces another exact cold lookup; it never invents any.
const USAGE_KEYS: usize = 16;
const MODEL_KEYS: usize = 16;

pub(crate) type UsageKey = (RunAttemptId, AgentTurnId);
pub(crate) type ModelKey = (ProviderKind, ModelReport, ModelReport);

#[derive(Default)]
pub(crate) struct SourceUsageCache {
    usage: Vec<(UsageKey, RecoveredDecisionUsage)>,
    /// `None` is positive evidence that no matching identity is retained.
    models: Vec<(ModelKey, Option<ModelRef>)>,
}

impl SourceUsageCache {
    pub(crate) fn usage(&self, key: &UsageKey) -> Option<&RecoveredDecisionUsage> {
        self.usage
            .iter()
            .find(|(known, _)| known == key)
            .map(|(_, usage)| usage)
    }

    pub(crate) fn model(&self, key: &ModelKey) -> Option<Option<ModelRef>> {
        self.models
            .iter()
            .find(|(known, _)| known == key)
            .map(|(_, model)| *model)
    }

    /// One cold worker answer for exactly its own key pair.
    pub(crate) fn record_observation(&mut self, observation: &DecisionUsageObservation) {
        let lookup = &observation.lookup;
        insert(
            &mut self.usage,
            USAGE_KEYS,
            (lookup.attempt, lookup.turn),
            observation.usage.clone(),
        );
        insert(
            &mut self.models,
            MODEL_KEYS,
            (
                lookup.provider.clone(),
                lookup.requested.clone(),
                lookup.reported.clone(),
            ),
            observation.model.as_ref().map(|model| model.id),
        );
    }

    /// Only after this producer's exact Applied receipt and physical return:
    /// the complete envelope it published is now the key's FullEnvelope.
    pub(crate) fn install(&mut self, snapshot: &DecisionUsageSnapshot, models: &[ModelIdentity]) {
        insert(
            &mut self.usage,
            USAGE_KEYS,
            (snapshot.attempt, snapshot.turn),
            RecoveredDecisionUsage::FullEnvelope {
                fact: snapshot.fact(),
                snapshot: snapshot.clone(),
            },
        );
        for model in models {
            insert(
                &mut self.models,
                MODEL_KEYS,
                (
                    model.provider.clone(),
                    model.requested.clone(),
                    model.reported.clone(),
                ),
                Some(model.id),
            );
        }
    }

    pub(crate) fn clear(&mut self) {
        self.usage.clear();
        self.models.clear();
    }
}

fn insert<K: PartialEq, V>(entries: &mut Vec<(K, V)>, bound: usize, key: K, value: V) {
    entries.retain(|(known, _)| *known != key);
    if entries.len() >= bound {
        entries.remove(0);
    }
    entries.push((key, value));
}

/// What one accepted stream usage report means for the Run.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UsageStage {
    /// The exact retained complete envelope already carries this report:
    /// the same EventId was enqueued with it, so nothing new is journaled.
    Duplicate,
    Stage {
        snapshot: DecisionUsageSnapshot,
        /// A model identity first observed now; existing refs are reused.
        model: Option<ModelIdentity>,
    },
}

/// The model this report names, resolved before staging. `Mint` only when the
/// cold lookup positively found no identity with these three values.
pub(crate) enum UsageModel {
    Known(ModelRef),
    Mint {
        provider: ProviderKind,
        requested: ModelReport,
        reported: ModelReport,
    },
}

pub(crate) fn stage_usage(
    recovered: &RecoveredDecisionUsage,
    model: UsageModel,
    attempt: RunAttemptId,
    turn: AgentTurnId,
    usage: &AgentUsage,
) -> Result<UsageStage, DecisionError> {
    let (model, minted) = match model {
        UsageModel::Known(model) => (model, None),
        UsageModel::Mint {
            provider,
            requested,
            reported,
        } => {
            let identity = ModelIdentity {
                id: ModelRef::mint(),
                provider,
                requested,
                reported,
            };
            (identity.id, Some(identity))
        }
    };
    let completeness = match completeness_of(usage) {
        Completeness::Complete => DecisionUsageCompleteness::Complete {},
        Completeness::Incomplete { reason } => DecisionUsageCompleteness::Incomplete { reason },
    };
    let report =
        |id: UsageFactId, revision: AmendmentRevision, event: EventId| DecisionUsageSnapshot {
            id,
            revision,
            event,
            attempt,
            turn,
            input: usage.input_tokens,
            output: usage.output_tokens,
            cached_input: usage.cached_input_tokens,
            cached_input_relationship: usage.cached_input,
            model,
            completeness: completeness.clone(),
        };
    let snapshot = match recovered {
        RecoveredDecisionUsage::Absent => report(
            UsageFactId::mint(),
            AmendmentRevision::FIRST,
            EventId::mint(),
        ),
        RecoveredDecisionUsage::FullEnvelope { fact, snapshot } => {
            if snapshot.fact() != *fact || fact.attempt != attempt || fact.turn != turn {
                return Err(DecisionError::Invalid);
            }
            // Complete report equality, including the cached relationship,
            // completeness and model: only then is this the same publication.
            if report(snapshot.id, snapshot.revision, snapshot.event) == *snapshot {
                return Ok(UsageStage::Duplicate);
            }
            report(snapshot.id, next(snapshot.revision)?, EventId::mint())
        }
        RecoveredDecisionUsage::AppliedFact(fact) => {
            if fact.attempt != attempt || fact.turn != turn {
                return Err(DecisionError::Invalid);
            }
            // The stored fact has no EventId or cached relationship. A later
            // accepted stream report is the next checked revision with a new
            // event, even when its totals equal the stored ones.
            report(fact.identity, next(fact.revision)?, EventId::mint())
        }
    };
    Ok(UsageStage::Stage {
        snapshot,
        model: minted,
    })
}

fn next(revision: AmendmentRevision) -> Result<AmendmentRevision, DecisionError> {
    revision
        .0
        .checked_add(1)
        .map(AmendmentRevision)
        .ok_or(DecisionError::Invalid)
}
