// SPDX-License-Identifier: MPL-2.0

pub use koru_store_core::ids::RemovalAttemptId;
pub use koru_store_core::profile_removal::{RemovalKind, RemovalMark};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BeginProfileRemoval {
    pub kind: RemovalKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProfileRemovalAttempt {
    pub attempt: RemovalAttemptId,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct CompleteProfileRemoval {
    #[serde(default)]
    pub attempt: Option<RemovalAttemptId>,
}
