// SPDX-License-Identifier: MPL-2.0

pub use koru_store_core::service::{
    EnsuredWorkspace, WorkspaceAuthority, WorkspaceAuthorityState, WorkspaceAvailabilityIssue,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `POST /v1/profiles/{owner}/workspaces/ensure-live`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct EnsureLiveWorkspace {
    pub name: String,
}
