// SPDX-License-Identifier: MPL-2.0
//! Shared scheduler/AI occurrence semantics. No current policy or allocated IDs.
use crate::{
    RoutineAssociation, ScheduleContextPointer, ScheduleDefinition, routine_definition_digest,
};
use koru_delegate_core::{
    HandoffId, Objective, RoutineDefinitionIdentity, RoutineDigest32, RoutineDigestError,
    RoutineSlot, WorkspaceId, routine_digest_json,
};
use serde::Serialize;

/// Derive this before current model selection, Store capture or ID allocation.
/// A matching retained slot is replayed without recomputing from current policy.
/// This hashes evidence only; the fresh path must verify the actual document.
pub fn routine_semantic_digest(
    slot: &RoutineSlot,
    definition: &ScheduleDefinition,
    workspace: WorkspaceId,
    owner_handoff: Option<HandoffId>,
) -> Result<RoutineDigest32, RoutineDigestError> {
    let creation = definition
        .routine
        .as_ref()
        .ok_or(RoutineDigestError::Invalid)?;
    if !slot.valid()
        || !creation.valid()
        || creation.owner != slot.owner
        || creation.bot != slot.bot
        || creation.home != workspace
    {
        return Err(RoutineDigestError::Invalid);
    }
    let definition_identity = RoutineDefinitionIdentity {
        revision: definition.revision,
        digest: routine_definition_digest(definition, workspace, owner_handoff)?,
        owner_handoff,
    };
    routine_digest_json(
        b"koru.bot.routine.semantic.v1",
        &SemanticV1 {
            version: 1,
            slot,
            definition: definition_identity,
            creation,
            objective: &definition.template.objective,
            task_context: &definition.template.context,
        },
        4 * 1024 * 1024,
    )
}

// Field order is the versioned canonical encoding. Context is an ordered list;
// set-valued fields in the full definition use its existing BTreeSet encoding.
#[derive(Serialize)]
struct SemanticV1<'a> {
    version: u8,
    slot: &'a RoutineSlot,
    definition: RoutineDefinitionIdentity,
    creation: &'a RoutineAssociation,
    objective: &'a Objective,
    task_context: &'a [ScheduleContextPointer],
}

#[cfg(test)]
mod tests;
