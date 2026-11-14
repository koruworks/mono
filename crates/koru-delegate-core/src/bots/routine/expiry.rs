// SPDX-License-Identifier: MPL-2.0

//! Consumed expiry facts contain no preparation or executable request.
use super::*;
use crate::WorkspaceId;

/// Trusted process-local evidence, neither serializable nor Clone. Only the
/// runtime's live document adapter supplies it to the expiry ledger method;
/// arbitrary Rust with database access is not an IPC authorization boundary.
pub struct RoutineExpiredClaimFacts {
    slot: RoutineSlot,
    definition: RoutineDefinitionIdentity,
    workspace: WorkspaceId,
    creation_request: BotRequestId,
    planned_run: RunId,
    representative_miss: Option<RoutineRepresentativeMiss>,
}
impl std::fmt::Debug for RoutineExpiredClaimFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutineExpiredClaimFacts")
            .field("slot", &self.slot)
            .finish_non_exhaustive()
    }
}
impl RoutineExpiredClaimFacts {
    /// The trusted adapter derives subject/definition/route from live document
    /// authority. This constructor validates shape, not the clock or authority.
    pub fn from_consumed_authority(
        slot: RoutineSlot,
        definition: RoutineDefinitionIdentity,
        workspace: WorkspaceId,
        creation_request: BotRequestId,
        planned_run: RunId,
        representative_miss: Option<RoutineRepresentativeMiss>,
    ) -> Option<Self> {
        if !slot.valid()
            || !definition.valid()
            || WorkspaceId::parse(&workspace.to_storage()) != Some(workspace)
            || !v4(creation_request.as_uuid())
            || !v4(planned_run.as_uuid())
            || representative_miss.is_some_and(|miss| {
                miss.occurrence_ms >= slot.occurrence_ms
                    || !v4(miss.planned_run.as_uuid())
                    || miss.planned_run == planned_run
            })
        {
            return None;
        }
        Some(Self {
            slot,
            definition,
            workspace,
            creation_request,
            planned_run,
            representative_miss,
        })
    }
    pub fn slot(&self) -> RoutineSlot {
        self.slot
    }
    pub fn definition(&self) -> RoutineDefinitionIdentity {
        self.definition
    }
    pub fn workspace(&self) -> WorkspaceId {
        self.workspace
    }
    pub fn creation_request(&self) -> BotRequestId {
        self.creation_request
    }
    pub fn planned_run(&self) -> RunId {
        self.planned_run
    }
    pub fn representative_miss(&self) -> Option<RoutineRepresentativeMiss> {
        self.representative_miss
    }
}
