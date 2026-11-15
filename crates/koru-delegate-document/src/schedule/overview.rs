// SPDX-License-Identifier: MPL-2.0
//! Bounded applied-state observation, never execution or complete-history proof.
use super::*;
use crate::bounded_value::Budget;
#[cfg(test)]
use crate::bounded_value::VALUE_DEPTH;
use koru_delegate_core::ActorRef;
use koru_delegate_schedule::RoutineAssociation;

const PROJECTION_BYTES: usize = 4 * 1024 * 1024;
const HANDOFF_COUNT: usize = 256;
const HANDOFF_BYTES: usize = 4096;

/// Private-owner vocabulary, deliberately without serde or authority methods.
#[derive(Clone, PartialEq, Eq)]
pub struct RoutineOverview {
    pub schedule: ScheduleId,
    pub created_at_ms: i64,
    pub initial_owner: DeviceId,
    pub association: RoutineAssociation,
    pub requested_by: ActorRef,
    pub name: ScheduleName,
    pub definition_revision: u32,
    pub state: ScheduleState,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RoutineDetailProjection {
    pub overview: RoutineOverview,
    pub effective_at_ms: i64,
    pub recurrence: koru_delegate_schedule::RecurrenceRule,
}
impl fmt::Debug for RoutineDetailProjection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoutineDetailProjection([REDACTED])")
    }
}
impl fmt::Debug for RoutineOverview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoutineOverview([REDACTED])")
    }
}

fn invalid() -> ScheduleDocumentError {
    ScheduleDocumentError::Malformed("routine_overview")
}

impl ScheduleDocument {
    /// Observe only the applied current definition/lifecycle. Does not inspect
    /// firings, export history, prove a live claim, or bound Store cold-open heap.
    pub fn routine_overview(&self) -> Result<RoutineOverview> {
        self.routine_detail().map(|p| p.overview)
    }

    /// The same bounded current projection, with timing but no firing history.
    pub fn routine_detail(&self) -> Result<RoutineDetailProjection> {
        if self.doc.is_detached() || self.doc.oplog_vv() != self.doc.state_vv() {
            return Err(invalid());
        }
        let root = self.root();
        if !matches!(
            root.get(K_SCHEMA),
            Some(ValueOrContainer::Value(LoroValue::I64(
                BOT_SCHEDULE_DOCUMENT_SCHEMA_VERSION
            )))
        ) {
            return Err(invalid());
        }
        let mut budget = Budget(PROJECTION_BYTES);
        budget.take(2).map_err(|_| invalid())?;
        for key in [K_SCHEMA, K_ID, K_CREATED, K_OWNER, K_DEFINITION, K_NAME] {
            let raw = value(&root, key)?;
            if key == K_NAME && !matches!(&raw, LoroValue::String(name) if name.len() <= 512) {
                return Err(invalid());
            }
            budget.scalar(&key).map_err(|_| invalid())?;
            budget.take(2).map_err(|_| invalid())?;
            budget.value(&raw, 0).map_err(|_| invalid())?;
        }
        // state() treats any retirement marker as present. Its body is not
        // needed by this projection and must not be decoded or size-gated.
        if root.get(K_RETIRED).is_none() {
            let handoffs = self.handoff_list();
            if handoffs.len() > HANDOFF_COUNT {
                return Err(invalid());
            }
            budget.scalar(&HANDOFFS).map_err(|_| invalid())?;
            budget
                .take(4 + handoffs.len().saturating_sub(1))
                .map_err(|_| invalid())?;
            for index in 0..handoffs.len() {
                let Some(ValueOrContainer::Value(raw)) = handoffs.get(index) else {
                    return Err(invalid());
                };
                let mut entry = Budget(HANDOFF_BYTES);
                entry.value(&raw, 0).map_err(|_| invalid())?;
                budget
                    .take(HANDOFF_BYTES - entry.0)
                    .map_err(|_| invalid())?;
            }
        }
        // The existing owners still decide schema, names and lifecycle. Their
        // allocations are now bounded, including state()'s full handoff fold.
        let header = self.header()?;
        if header.definition.revision == 0
            || header.definition.effective_at_ms < header.created_at_ms
        {
            return Err(invalid());
        }
        let association = header.definition.routine.ok_or_else(invalid)?;
        Ok(RoutineDetailProjection {
            overview: RoutineOverview {
                schedule: header.schedule,
                created_at_ms: header.created_at_ms,
                initial_owner: header.initial_owner,
                association,
                requested_by: header.definition.template.requested_by,
                name: self.name()?,
                definition_revision: header.definition.revision,
                state: self.state()?,
            },
            effective_at_ms: header.definition.effective_at_ms,
            recurrence: header.definition.recurrence,
        })
    }
}

#[cfg(test)]
mod tests;
