// SPDX-License-Identifier: MPL-2.0

//! Handle-local staging for one exact, SQL-journaled decision projection.
//! No Store connection, execution permission or public delta import capability.

use super::*;
use koru_delegate_core::decisions::*;
use serde::{Deserialize, Serialize};

const K_PROJECTION: &str = "decision_projection";
const BOT_PROJECTION_SCHEMA: i64 = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunProjectionIntent {
    pub identity: DecisionProjectionIdentity,
    pub target: RunProjectionTargetV1,
}
impl RunProjectionIntent {
    pub fn validate(&self) -> Result<()> {
        self.target
            .matches_identity(&self.identity)
            .map_err(invalid)?;
        Ok(())
    }
}

/// The actual host supplies its retained acknowledged VV, never merely the
/// connected document's current version or an empty send queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedRunProjection {
    identity: DecisionProjectionIdentity,
    observed_version: DecisionVersionVector,
}
impl AppliedRunProjection {
    pub fn identity(&self) -> &DecisionProjectionIdentity {
        &self.identity
    }
    pub fn observed_version(&self) -> &DecisionVersionVector {
        &self.observed_version
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    identity: DecisionProjectionIdentity,
    status: RunStatus,
    bookkeeping: Option<DecisionSourceBookkeeping>,
    timeline_start: u64,
    timeline_end: u64,
}

/// Lives in the sole connected handle, not in a separate per-Run cache. The
/// first initialization copies current state (including retained timeline), not
/// full history. Export limits are post-allocation acceptance bounds.
#[derive(Default)]
pub struct RunProjectionStaging {
    staged: Option<RunDocument>,
    prepared: Option<DecisionPreparedProjection>,
    applied: bool,
}
impl RunProjectionStaging {
    pub fn prepare(
        &mut self,
        connected: &RunDocument,
        intent: &RunProjectionIntent,
    ) -> Result<DecisionPreparedProjection> {
        intent.validate()?;
        if let Some(prepared) = &self.prepared {
            if prepared.identity == intent.identity
                && prepared.target == intent.target.canonical_bytes().map_err(invalid)?
            {
                return Ok(prepared.clone());
            }
            return Err(invalid(()));
        }
        let result = self.prepare_new(connected, intent);
        if result.is_err() {
            // No preparation receipt exists: discard partially written staging,
            // including oversized exports. The connected object is unchanged.
            self.staged = None;
        }
        result
    }
    fn prepare_new(
        &mut self,
        connected: &RunDocument,
        intent: &RunProjectionIntent,
    ) -> Result<DecisionPreparedProjection> {
        connected.check_projection_subject(&intent.identity, &intent.target, false)?;
        connected.check_projection_predecessor(&intent.target)?;
        self.refresh(connected)?;
        let stage = self.staged.as_ref().ok_or_else(|| invalid(()))?;
        let base = stage.doc.oplog_vv();
        stage.write_projection(intent)?;
        let result = stage.doc.oplog_vv();
        let delta =
            DecisionPreparedDelta::new(stage.export_updates_since(&base)?).map_err(invalid)?;
        let prepared = DecisionPreparedProjection {
            identity: intent.identity.clone(),
            target: intent.target.canonical_bytes().map_err(invalid)?,
            base_version: DecisionVersionVector::new(base.encode()).map_err(invalid)?,
            result_version: DecisionVersionVector::new(result.encode()).map_err(invalid)?,
            delta_digest: decision_projection_delta_digest(&delta).map_err(invalid)?,
            delta,
        };
        prepared.validate().map_err(invalid)?;
        self.prepared = Some(prepared.clone());
        Ok(prepared)
    }

    /// Receives the sole ledger's exact retained preparation. A warm handle
    /// additionally compares its original candidate byte-for-byte. Cold recovery
    /// uses the same preflight and never reruns the target to mint operations.
    pub fn apply(
        &mut self,
        connected: &RunDocument,
        prepared: &DecisionPreparedProjection,
    ) -> Result<()> {
        let target = checked_prepared(prepared)?;
        if self.prepared.as_ref().is_some_and(|p| p != prepared) {
            return Err(invalid(()));
        }
        connected.check_projection_subject(&prepared.identity, &target, true)?;
        let base = decode_vv(&prepared.base_version)?;
        require_document(covers(&connected.doc.state_vv(), &base))?;
        if connected.check_applied(prepared, &target).is_ok() {
            // A cold shallow stage may no longer retain the older delta's
            // history. Exact applied marker/effects plus result coverage already
            // prove idempotent application; durability is still checked later.
            self.prepared = Some(prepared.clone());
            self.applied = true;
            return Ok(());
        }
        if self.staged.is_none() {
            self.refresh(connected)?;
        }
        let stage = self.staged.as_ref().ok_or_else(|| invalid(()))?;
        checked_import(stage, prepared.delta.as_slice())?;
        stage.check_applied(prepared, &target)?;
        // Preflight above is disposable. Import into the original connected
        // object, preserving any unrelated parked updates it already holds.
        self.prepared = Some(prepared.clone());
        self.applied = true;
        checked_import(connected, prepared.delta.as_slice())?;
        connected.check_applied(prepared, &target)?;
        self.prepared = Some(prepared.clone());
        Ok(())
    }

    pub fn acknowledge(
        &mut self,
        connected: &RunDocument,
        prepared: &DecisionPreparedProjection,
        acknowledged: &VersionVector,
    ) -> Result<AppliedRunProjection> {
        let target = checked_prepared(prepared)?;
        require_document(self.prepared.as_ref() == Some(prepared))?;
        require_document(covers(acknowledged, &decode_vv(&prepared.result_version)?))?;
        // Settle may absorb remote operations: inspect the current attached
        // state again after the actual durable acknowledgement.
        connected.check_applied(prepared, &target)?;
        let result = AppliedRunProjection {
            identity: prepared.identity.clone(),
            observed_version: DecisionVersionVector::new(acknowledged.encode()).map_err(invalid)?,
        };
        self.prepared = None;
        self.applied = false;
        Ok(result)
    }

    pub fn discard(&mut self, transition: DecisionTransitionId) -> Result<()> {
        require_document(!self.applied)?;
        if self
            .prepared
            .as_ref()
            .is_some_and(|p| p.identity.transition != transition)
        {
            return Err(invalid(()));
        }
        self.prepared = None;
        self.staged = None;
        Ok(())
    }

    fn refresh(&mut self, connected: &RunDocument) -> Result<()> {
        coherent(connected)?;
        if let Some(stage) = &self.staged {
            require_document(covers(&connected.doc.oplog_vv(), &stage.doc.oplog_vv()))?;
            checked_import(
                stage,
                &connected.export_updates_since(&stage.doc.oplog_vv())?,
            )?;
            require_document(stage.doc.state_vv() == connected.doc.state_vv())?;
        } else {
            let bytes = connected.doc.export(ExportMode::shallow_snapshot(
                &connected.doc.state_frontiers(),
            ))?;
            let doc = LoroDoc::new();
            let imported = doc.import(&bytes)?;
            require_document(imported.pending.is_none())?;
            let stage = RunDocument::adopt(doc)?;
            coherent(&stage)?;
            require_document(stage.doc.state_vv() == connected.doc.state_vv())?;
            self.staged = Some(stage);
        }
        Ok(())
    }
}

impl RunDocument {
    fn check_projection_subject(
        &self,
        identity: &DecisionProjectionIdentity,
        target: &RunProjectionTargetV1,
        retry: bool,
    ) -> Result<()> {
        coherent(self)?;
        target.matches_identity(identity).map_err(invalid)?;
        let schema = self.schema_version().ok_or_else(|| invalid(()))?;
        if schema > BOT_PROJECTION_SCHEMA {
            return Err(RunDocumentError::ReadOnly {
                found: schema,
                understood: BOT_PROJECTION_SCHEMA,
            });
        }
        let header = self.header()?;
        let bot = header.bot.as_ref().ok_or_else(|| invalid(()))?;
        let original = i64::from(u8::from(identity.source_schema));
        require_document(
            (schema == original || (retry && schema == BOT_PROJECTION_SCHEMA))
                && matches!(schema, 3..=5)
                && (schema != 3 || bot.routine.is_none())
                && (schema != 4 || bot.routine.is_some())
                && header.bot_fields_valid()
                && header.run == identity.run
                && target.fence().owner == bot.owner
                && target.fence().bot == bot.bot
                && header.executing_device() == Some(bot.owner.device)
                && decision_projection_subject_digest(&header).map_err(invalid)?
                    == identity.subject_digest,
        )?;
        Ok(())
    }

    fn marker(&self) -> Result<Option<Marker>> {
        match self.run().get(K_PROJECTION) {
            None => Ok(None),
            Some(ValueOrContainer::Value(value)) => {
                // Bound the new marker before the owning JSON decoder copies it.
                crate::bounded_value::Budget(DECISION_PAYLOAD_BYTES)
                    .value(&value, 0)
                    .map_err(invalid)?;
                from_stored(value, Part::Header).map(Some)
            }
            _ => Err(invalid(())),
        }
    }

    fn check_projection_predecessor(&self, target: &RunProjectionTargetV1) -> Result<()> {
        let header = self.header()?;
        require_document(
            header.status == target.predecessor().status
                && header.failure_reason == target.predecessor().failure_reason,
        )?;
        match (&target.predecessor().projection, self.marker()?) {
            (None, None) => Ok(()),
            (Some(prior), Some(marker)) => {
                require_document(
                    prior.transition == marker.identity.transition
                        && prior.sequence == marker.identity.sequence
                        && prior.target_digest == marker.identity.target_digest,
                )?;
                if let RunProjectionTargetV1::Control {
                    retained_bookkeeping,
                    ..
                } = target
                {
                    require_document(*retained_bookkeeping == marker.bookkeeping)?;
                }
                if let (Some(next), Some(old)) =
                    (target.source_bookkeeping(), marker.bookkeeping.as_ref())
                {
                    if next.boundary.fence == old.boundary.fence {
                        let (old_sequence, old_other, next_other) = match next.boundary.stream {
                            DecisionSourceStream::Agent => (
                                old.boundary.resulting.agent_after,
                                old.boundary.resulting.session_after,
                                next.boundary.resulting.session_after,
                            ),
                            DecisionSourceStream::Session => (
                                old.boundary.resulting.session_after,
                                old.boundary.resulting.agent_after,
                                next.boundary.resulting.agent_after,
                            ),
                        };
                        require_document(
                            old_sequence.is_none_or(|v| next.boundary.sequence > v)
                                && old_other == next_other,
                        )?;
                    }
                }
                Ok(())
            }
            _ => Err(invalid(())),
        }
    }

    fn write_projection(&self, intent: &RunProjectionIntent) -> Result<()> {
        self.check_projection_subject(&intent.identity, &intent.target, false)?;
        self.check_projection_predecessor(&intent.target)?;
        let effects = intent.target.effects();
        let start = self.timeline().len();
        for step in effects.steps.as_slice() {
            self.timeline()
                .push(to_stored(&StoredItem::Step(step.clone()), Part::Timeline)?)?;
        }
        if let Some(usage) = intent
            .target
            .source_bookkeeping()
            .and_then(|b| b.usage.as_ref())
        {
            self.timeline()
                .push(to_stored(&StoredItem::Usage(usage.fact()), Part::Timeline)?)?;
        }
        let models = match self.run().get(K_MODELS) {
            Some(ValueOrContainer::Container(loro::Container::Map(models))) => models,
            _ => return Err(invalid(())),
        };
        for model in effects.models.as_slice() {
            models.insert(&model.id.to_storage(), to_stored(model, Part::Models)?)?;
        }
        if let Some(now) = effects.economics_at_ms {
            self.run().insert(
                K_ECONOMICS,
                to_stored(&self.economics(now), Part::Economics)?,
            )?;
        }
        self.run()
            .insert(K_STATUS, to_stored(&effects.status, Part::Header)?)?;
        if let Some(reason) = effects.failure_reason {
            self.run()
                .insert("failure_reason", to_stored(&reason, Part::Header)?)?;
        } else {
            self.run().delete("failure_reason")?;
        }
        let marker = Marker {
            identity: intent.identity.clone(),
            status: effects.status.clone(),
            bookkeeping: intent.target.bookkeeping().cloned(),
            timeline_start: start as u64,
            timeline_end: self.timeline().len() as u64,
        };
        koru_delegate_core::routine_digest_json(
            b"koru.bot.decision.projection.marker-shape.v1",
            &marker,
            DECISION_PAYLOAD_BYTES,
        )
        .map_err(invalid)?;
        self.run()
            .insert(K_PROJECTION, to_stored(&marker, Part::Header)?)?;
        self.run().insert(K_SCHEMA, BOT_PROJECTION_SCHEMA)?;
        self.doc.commit();
        Ok(())
    }

    fn check_applied(
        &self,
        prepared: &DecisionPreparedProjection,
        target: &RunProjectionTargetV1,
    ) -> Result<()> {
        self.check_projection_subject(&prepared.identity, target, true)?;
        require_document(
            self.schema_version() == Some(BOT_PROJECTION_SCHEMA)
                && covers(&self.doc.state_vv(), &decode_vv(&prepared.result_version)?),
        )?;
        let marker = self.marker()?.ok_or_else(|| invalid(()))?;
        let effects = target.effects();
        require_document(
            marker.identity == prepared.identity
                && marker.status == effects.status
                && marker.bookkeeping.as_ref() == target.bookkeeping()
                && self.header()?.status == effects.status
                && self.header()?.failure_reason == effects.failure_reason,
        )?;
        let start = usize::try_from(marker.timeline_start).map_err(invalid)?;
        let count = effects.steps.as_slice().len()
            + usize::from(
                target
                    .source_bookkeeping()
                    .is_some_and(|b| b.usage.is_some()),
            );
        require_document(
            start.checked_add(count) == usize::try_from(marker.timeline_end).ok()
                && marker.timeline_end <= self.timeline().len() as u64,
        )?;
        for (offset, step) in effects.steps.as_slice().iter().enumerate() {
            require_document(
                self.timeline()
                    .get(start + offset)
                    .map(|v| v.get_deep_value())
                    == Some(to_stored(&StoredItem::Step(step.clone()), Part::Timeline)?),
            )?;
        }
        if let Some(usage) = target.source_bookkeeping().and_then(|b| b.usage.as_ref()) {
            require_document(
                self.timeline()
                    .get(start + count - 1)
                    .map(|v| v.get_deep_value())
                    == Some(to_stored(&StoredItem::Usage(usage.fact()), Part::Timeline)?),
            )?;
        }
        let models = match self.run().get(K_MODELS) {
            Some(ValueOrContainer::Container(loro::Container::Map(models))) => models,
            _ => return Err(invalid(())),
        };
        for model in effects.models.as_slice() {
            require_document(
                models
                    .get(&model.id.to_storage())
                    .map(|v| v.get_deep_value())
                    == Some(to_stored(model, Part::Models)?),
            )?;
        }
        if let Some(now) = effects.economics_at_ms {
            require_document(self.header()?.economics == self.economics(now))?;
        }
        Ok(())
    }
}

fn checked_prepared(prepared: &DecisionPreparedProjection) -> Result<RunProjectionTargetV1> {
    prepared.validate().map_err(invalid)?;
    let target = RunProjectionTargetV1::decode(prepared.target.as_slice()).map_err(invalid)?;
    target
        .matches_identity(&prepared.identity)
        .map_err(invalid)?;
    require_document(
        target.canonical_bytes().map_err(invalid)? == prepared.target
            && decision_projection_delta_digest(&prepared.delta).map_err(invalid)?
                == prepared.delta_digest,
    )?;
    let base = decode_vv(&prepared.base_version)?;
    let result = decode_vv(&prepared.result_version)?;
    require_document(result != base && covers(&result, &base))?;
    Ok(target)
}
fn decode_vv(bytes: &DecisionVersionVector) -> Result<VersionVector> {
    VersionVector::decode(bytes.as_slice()).map_err(invalid)
}
fn covers(value: &VersionVector, required: &VersionVector) -> bool {
    matches!(
        value.partial_cmp(required),
        Some(std::cmp::Ordering::Equal | std::cmp::Ordering::Greater)
    )
}
fn coherent(doc: &RunDocument) -> Result<()> {
    require_document(!doc.doc.is_detached() && doc.doc.state_vv() == doc.doc.oplog_vv())
}
fn checked_import(doc: &RunDocument, bytes: &[u8]) -> Result<()> {
    let result = doc.doc.import(bytes)?;
    require_document(result.pending.is_none())?;
    coherent(doc)
}
fn require_document(ok: bool) -> Result<()> {
    if ok { Ok(()) } else { Err(invalid(())) }
}
fn invalid(_: impl std::fmt::Debug) -> RunDocumentError {
    RunDocumentError::Malformed { part: Part::Header }
}
