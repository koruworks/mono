// SPDX-License-Identifier: MPL-2.0

//! Atomic Profile and first-workspace creation. The legacy route is unchanged.
use koru_store_core::{Profile, ProfileId, Workspace};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FirstWorkspace {
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
}

/// `POST /v1/profiles/with-workspace`. Keep `id` through uncertain responses.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateProfileWithWorkspace {
    pub id: ProfileId,
    pub name: String,
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    pub workspace: FirstWorkspace,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreatedProfile {
    pub profile: Profile,
    pub workspace: Workspace,
}
