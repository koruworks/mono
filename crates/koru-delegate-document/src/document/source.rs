// SPDX-License-Identifier: MPL-2.0

//! Narrow cold source identity recovery. Called on the fixed document worker,
//! once for an unresolved key; never by the actor's status/maintenance polling.

use super::*;
use koru_delegate_core::decisions::{
    DECISION_PAYLOAD_BYTES, DecisionDigest, DecisionProjectionIdentity, DecisionUsageSnapshot,
    encode_decision_value,
};
use koru_delegate_core::{AgentTurnId, AmendmentRevision, ModelReport, ProviderKind, RunAttemptId};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DecisionUsageLookup {
    pub attempt: RunAttemptId,
    pub turn: AgentTurnId,
    pub provider: ProviderKind,
    pub requested: ModelReport,
    pub reported: ModelReport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum RecoveredDecisionUsage {
    Absent,
    /// The historical document fact has no EventId or cached relationship.
    /// It proves stable identity/revision, never complete report equality.
    AppliedFact(UsageFact),
    FullEnvelope {
        fact: UsageFact,
        snapshot: DecisionUsageSnapshot,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DecisionUsageObservation {
    pub projection: DecisionProjectionIdentity,
    pub lookup: DecisionUsageLookup,
    pub usage: RecoveredDecisionUsage,
    pub model: Option<ModelIdentity>,
}

impl RunDocument {
    /// Iterate current list values individually, retaining one key's highest
    /// amendment and bounded revision digests. No history export, timeline
    /// vector, whole model-map decode or actor-side Store access is involved.
    pub fn observe_decision_usage(
        &self,
        projection: &DecisionProjectionIdentity,
        lookup: &DecisionUsageLookup,
    ) -> Result<DecisionUsageObservation> {
        encode_decision_value(lookup, DECISION_PAYLOAD_BYTES).map_err(invalid)?;
        let marker = self.marker()?.ok_or_else(|| invalid(()))?;
        if marker.identity != *projection || marker.timeline_end != self.timeline_len() as u64 {
            return Err(invalid(()));
        }
        let mut latest: Option<UsageFact> = None;
        let mut revisions: BTreeMap<AmendmentRevision, DecisionDigest> = BTreeMap::new();
        let mut retained_bytes = 2usize;
        let timeline = self.timeline();
        for index in 0..timeline.len() {
            let Some(ValueOrContainer::Value(value)) = timeline.get(index) else {
                return Err(invalid(()));
            };
            crate::bounded_value::Budget(DECISION_PAYLOAD_BYTES)
                .value(&value, 0)
                .map_err(invalid)?;
            match TimelineItem::decode(value) {
                TimelineItem::Usage(fact)
                    if fact.attempt == lookup.attempt && fact.turn == lookup.turn =>
                {
                    if latest
                        .as_ref()
                        .is_some_and(|old| old.identity != fact.identity)
                    {
                        return Err(invalid(()));
                    }
                    let digest = koru_delegate_core::routine_digest_json(
                        b"koru.bot.source.usage-recovery.v1",
                        &fact,
                        DECISION_PAYLOAD_BYTES,
                    )
                    .map_err(invalid)?;
                    match revisions.get(&fact.revision) {
                        Some(old) if *old != digest => return Err(invalid(())),
                        Some(_) => {} // Identical stored retries are explicitly legal.
                        None => {
                            let bytes = encode_decision_value(
                                &(fact.revision, digest),
                                DECISION_PAYLOAD_BYTES,
                            )
                            .map_err(invalid)?;
                            retained_bytes = retained_bytes
                                .checked_add(bytes.len() + 1)
                                .ok_or_else(|| invalid(()))?;
                            if retained_bytes > DECISION_PAYLOAD_BYTES {
                                return Err(invalid(()));
                            }
                            revisions.insert(fact.revision, digest);
                        }
                    }
                    if latest
                        .as_ref()
                        .is_none_or(|old| fact.revision > old.revision)
                    {
                        latest = Some(fact);
                    }
                }
                TimelineItem::Unrecognized => return Err(invalid(())),
                _ => {}
            }
        }
        let mut model: Option<ModelIdentity> = None;
        let mut model_error = false;
        let Some(ValueOrContainer::Container(loro::Container::Map(models))) =
            self.run().get(K_MODELS)
        else {
            return Err(invalid(()));
        };
        // Loro's map iterator yields individual current values. Decode only
        // one bounded identity at a time; prefer this usage key's existing ref.
        models.for_each(|key, value| {
            if model_error {
                return;
            }
            let decoded = (|| -> Result<ModelIdentity> {
                let ValueOrContainer::Value(value) = value else {
                    return Err(invalid(()));
                };
                crate::bounded_value::Budget(DECISION_PAYLOAD_BYTES)
                    .value(&value, 0)
                    .map_err(invalid)?;
                let identity: ModelIdentity = from_stored(value, Part::Models)?;
                if identity.id.to_storage() != key {
                    return Err(invalid(()));
                }
                Ok(identity)
            })();
            let identity = match decoded {
                Ok(identity) => identity,
                Err(_) => {
                    model_error = true;
                    return;
                }
            };
            if identity.provider == lookup.provider
                && identity.requested == lookup.requested
                && identity.reported == lookup.reported
            {
                let preferred = latest
                    .as_ref()
                    .is_some_and(|fact| fact.model == identity.id);
                let already_preferred = model.as_ref().is_some_and(|model| {
                    latest.as_ref().is_some_and(|fact| fact.model == model.id)
                });
                if preferred
                    || (!already_preferred && model.as_ref().is_none_or(|old| identity.id < old.id))
                {
                    model = Some(identity);
                }
            }
        });
        if model_error {
            return Err(invalid(()));
        }
        let usage = match latest {
            None => RecoveredDecisionUsage::Absent,
            Some(fact) => match marker.bookkeeping.and_then(|bookkeeping| bookkeeping.usage) {
                Some(snapshot) if snapshot.fact() == fact => {
                    RecoveredDecisionUsage::FullEnvelope { fact, snapshot }
                }
                _ => RecoveredDecisionUsage::AppliedFact(fact),
            },
        };
        let observation = DecisionUsageObservation {
            projection: projection.clone(),
            lookup: lookup.clone(),
            usage,
            model,
        };
        encode_decision_value(&observation, DECISION_PAYLOAD_BYTES).map_err(invalid)?;
        Ok(observation)
    }
}

fn invalid<T>(_: T) -> RunDocumentError {
    RunDocumentError::Malformed {
        part: Part::Timeline,
    }
}
