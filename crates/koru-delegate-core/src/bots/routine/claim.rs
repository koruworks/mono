// SPDX-License-Identifier: MPL-2.0

//! Process-local facts produced by consuming live document authority.
use super::*;
use crate::WorkspaceId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoutineRepresentativeMiss {
    pub occurrence_ms: i64,
    pub planned_run: RunId,
}

/// This is not a wire capability or a boundary against arbitrary Rust code with
/// database access. The trusted runtime must consume actual document authority
/// immediately before passing these facts to its ledger. No IPC accepts them.
/// They deliberately implement neither serde nor Clone.
pub struct RoutineClaimFacts {
    prepared: RoutinePrepared,
    workspace: WorkspaceId,
    creation_request: BotRequestId,
    representative_miss: Option<RoutineRepresentativeMiss>,
}
impl std::fmt::Debug for RoutineClaimFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutineClaimFacts")
            .field("slot", &self.prepared.slot)
            .finish_non_exhaustive()
    }
}
impl RoutineClaimFacts {
    /// Trusted document-adapter constructor, not caller authentication. The slot
    /// and definition must be independently derived from the live document.
    pub fn from_consumed_authority(
        prepared: RoutinePrepared,
        slot: RoutineSlot,
        definition: RoutineDefinitionIdentity,
        workspace: WorkspaceId,
        creation_request: BotRequestId,
        representative_miss: Option<RoutineRepresentativeMiss>,
    ) -> Option<Self> {
        if !prepared.valid()
            || prepared.slot != slot
            || prepared.definition != definition
            || prepared.submission.workspace != workspace
            || !v4(creation_request.as_uuid())
            || representative_miss.is_some_and(|miss| {
                miss.occurrence_ms >= slot.occurrence_ms
                    || !v4(miss.planned_run.as_uuid())
                    || miss.planned_run == prepared.ids.run
            })
        {
            return None;
        }
        Some(Self {
            prepared,
            workspace,
            creation_request,
            representative_miss,
        })
    }
    pub fn prepared(&self) -> &RoutinePrepared {
        &self.prepared
    }
    pub fn workspace(&self) -> WorkspaceId {
        self.workspace
    }
    pub fn creation_request(&self) -> BotRequestId {
        self.creation_request
    }
    pub fn representative_miss(&self) -> Option<RoutineRepresentativeMiss> {
        self.representative_miss
    }
}
