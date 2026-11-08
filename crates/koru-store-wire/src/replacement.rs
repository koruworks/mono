// SPDX-License-Identifier: MPL-2.0

use koru_store_core::ProfileId;
pub use koru_store_core::service::{ListingState, MovedWorkspace, RemovedWorkspace, Replacement};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RemoveWorkspaceReplacing {
    /// Localized name supplied by the caller for a required replacement.
    pub name: String,
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removal_attempt: Option<koru_store_core::ids::RemovalAttemptId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MoveWorkspaceReplacing {
    pub owner: ProfileId,
    pub name: String,
}

/// HTTP 409: the authority committed, but its catalog transaction did not.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListingPendingFailure {
    pub code: String,
    pub message: String,
    pub cause: String,
    pub outcome: RemovedWorkspace,
}
