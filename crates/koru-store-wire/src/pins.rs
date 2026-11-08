// SPDX-License-Identifier: MPL-2.0

pub use koru_store_core::service::PinnedWorkspace;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `PUT /v1/workspaces/{id}/pinned` changes only the existing synced pin key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspacePin {
    pub pinned: bool,
}
