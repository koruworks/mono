// SPDX-License-Identifier: MPL-2.0
//! Bounded applied-state observation, never execution or complete-history proof.
use super::*;
use koru_delegate_core::ActorRef;
use koru_delegate_schedule::RoutineAssociation;
use std::io::Write;

const PROJECTION_BYTES: usize = 4 * 1024 * 1024;
const HANDOFF_COUNT: usize = 256;
const HANDOFF_BYTES: usize = 4096;
const VALUE_DEPTH: usize = 64;

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
impl fmt::Debug for RoutineOverview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoutineOverview([REDACTED])")
    }
}

fn invalid() -> ScheduleDocumentError {
    ScheduleDocumentError::Malformed("routine_overview")
}

/// Counts raw atomic values before the existing codec expands them into JSON.
/// LoroValue clones share their backing allocation; no deep-value read occurs.
struct Budget(usize);
impl Budget {
    fn take(&mut self, n: usize) -> Result<()> {
        self.0 = self.0.checked_sub(n).ok_or_else(invalid)?;
        Ok(())
    }
    fn scalar(&mut self, value: &impl Serialize) -> Result<()> {
        serde_json::to_writer(self, value).map_err(|_| invalid())
    }
    fn value(&mut self, value: &LoroValue, depth: usize) -> Result<()> {
        if depth > VALUE_DEPTH {
            return Err(invalid());
        }
        match value {
            LoroValue::Null => self.take(4),
            LoroValue::Bool(v) => self.scalar(v),
            LoroValue::I64(v) => self.scalar(v),
            LoroValue::Double(v) if v.is_finite() => self.scalar(v),
            LoroValue::String(v) => self.scalar(&v.as_str()),
            LoroValue::List(values) => {
                self.take(2 + values.len().saturating_sub(1))?;
                for value in values.iter() {
                    self.value(value, depth + 1)?;
                }
                Ok(())
            }
            LoroValue::Map(values) => {
                self.take(2 + values.len().saturating_sub(1))?;
                for (key, value) in values.iter() {
                    self.scalar(key)?;
                    self.take(1)?;
                    self.value(value, depth + 1)?;
                }
                Ok(())
            }
            _ => Err(invalid()),
        }
    }
}
impl Write for Budget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("Routine overview limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl ScheduleDocument {
    /// Observe only the applied current definition/lifecycle. Does not inspect
    /// firings, export history, prove a live claim, or bound Store cold-open heap.
    pub fn routine_overview(&self) -> Result<RoutineOverview> {
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
        budget.take(2)?;
        for key in [K_SCHEMA, K_ID, K_CREATED, K_OWNER, K_DEFINITION, K_NAME] {
            let raw = value(&root, key)?;
            if key == K_NAME && !matches!(&raw, LoroValue::String(name) if name.len() <= 512) {
                return Err(invalid());
            }
            budget.scalar(&key)?;
            budget.take(2)?;
            budget.value(&raw, 0)?;
        }
        // state() treats any retirement marker as present. Its body is not
        // needed by this projection and must not be decoded or size-gated.
        if root.get(K_RETIRED).is_none() {
            let handoffs = self.handoff_list();
            if handoffs.len() > HANDOFF_COUNT {
                return Err(invalid());
            }
            budget.scalar(&HANDOFFS)?;
            budget.take(4 + handoffs.len().saturating_sub(1))?;
            for index in 0..handoffs.len() {
                let Some(ValueOrContainer::Value(raw)) = handoffs.get(index) else {
                    return Err(invalid());
                };
                let mut entry = Budget(HANDOFF_BYTES);
                entry.value(&raw, 0)?;
                budget.take(HANDOFF_BYTES - entry.0)?;
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
        Ok(RoutineOverview {
            schedule: header.schedule,
            created_at_ms: header.created_at_ms,
            initial_owner: header.initial_owner,
            association,
            requested_by: header.definition.template.requested_by,
            name: self.name()?,
            definition_revision: header.definition.revision,
            state: self.state()?,
        })
    }
}

#[cfg(test)]
mod tests;
