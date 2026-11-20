// SPDX-License-Identifier: MPL-2.0

//! Native Routine creation observations exposed by the window adapter.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotRoutineCreationState {
    Prepared,
    Unconfirmed,
    Bound,
    Conflict,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotRoutineCreationRefusal {
    OriginMismatch,
    DocumentChanged,
    OwnerChanged,
    AuthorityChanged,
}

/// Optimistic concurrency token, never proof of Store or owner authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotRoutineCreationObserved {
    pub state: BotRoutineCreationState,
    pub updated_at_ms: i64,
}
