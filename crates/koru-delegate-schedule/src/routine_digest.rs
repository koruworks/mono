// SPDX-License-Identifier: MPL-2.0

use crate::{ScheduleDefinition, routine_wire};
use koru_delegate_core::{
    HandoffId, RoutineDigest32, RoutineDigestError, WorkspaceId, routine_digest_json,
};
use serde::Serialize;

/// Canonical complete Routine definition bytes, including inert template
/// fields. List order is retained; existing BTreeSets encode in sorted order.
/// This validates identity shape only, never a live document or current grant.
pub fn routine_definition_digest(
    definition: &ScheduleDefinition,
    workspace: WorkspaceId,
    owner_handoff: Option<HandoffId>,
) -> Result<RoutineDigest32, RoutineDigestError> {
    let association = definition
        .routine
        .as_ref()
        .ok_or(RoutineDigestError::Invalid)?;
    if definition.revision == 0
        || !association.valid()
        || association.home != workspace
        || owner_handoff.is_some_and(|id| id.to_storage().parse::<HandoffId>().ok() != Some(id))
    {
        return Err(RoutineDigestError::Invalid);
    }
    routine_digest_json(
        b"koru.bot.routine.definition.v1",
        &DefinitionDigestV1 {
            version: 1,
            workspace,
            owner_handoff,
            definition,
        },
        4 * 1024 * 1024,
    )
}

#[derive(Serialize)]
struct DefinitionDigestV1<'a> {
    version: u8,
    workspace: WorkspaceId,
    owner_handoff: Option<HandoffId>,
    #[serde(with = "routine_wire::Definition")]
    definition: &'a ScheduleDefinition,
}

#[cfg(test)]
mod tests;
