// SPDX-License-Identifier: MPL-2.0

//! Engine-owned Bot configuration. No execution, HTTP or database owner lives here.

use crate::{ChatId, DocumentId, ModelSelection, ProfileId, WorkspaceId};
pub use koru_delegate_core::{BotId, BotRequestId, DeviceId};
pub use koru_store_core::UserScopeId;
use serde::{Deserialize, Serialize};

pub mod replay;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BotAccess {
    Default,
    Workspace,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotPage {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BotConfig {
    pub name: String,
    pub role: BotPage,
    pub working_folder: String,
    #[serde(deserialize_with = "strict_model")]
    #[cfg_attr(feature = "schema", schemars(with = "StrictModel"))]
    pub model: ModelSelection,
    pub mode: Option<String>,
    pub fast: Option<bool>,
}

impl std::fmt::Debug for BotConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotConfig").finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Bot {
    pub id: BotId,
    pub home_workspace: WorkspaceId,
    pub user_scope: UserScopeId,
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub owner_device: DeviceId,
    pub config: BotConfig,
    pub access: BotAccess,
    pub selected_pages: Vec<BotPage>,
    pub revision: u64,
    pub policy_revision: u64,
    pub main_chat: ChatId,
    pub paused: bool,
    pub folder_lock_request: Option<BotRequestId>,
    pub folder_frozen_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BotOwner {
    pub user_scope: UserScopeId,
    pub device: DeviceId,
}

/// Immutable allowlist supplied only after fresh owner checks. Not a public DTO.
#[derive(Clone, Debug, Default)]
pub struct ChatReadAccess {
    allowed_bot_chats: Vec<ChatId>,
}
impl ChatReadAccess {
    pub fn ordinary() -> Self {
        Self::default()
    }
    pub fn authorized(chats: Vec<ChatId>) -> Self {
        Self {
            allowed_bot_chats: chats,
        }
    }
    pub fn is_ordinary(&self) -> bool {
        self.allowed_bot_chats.is_empty()
    }
    pub fn json(&self) -> String {
        serde_json::to_string(&self.allowed_bot_chats).expect("UUID serialization")
    }
}

/// Fresh authority from the same root's Store and validated supervisor bootstrap.
pub trait BotAuthority: Send + Sync {
    fn owner(&self) -> Result<BotOwner, BotError>;
    fn workspace(&self, workspace: WorkspaceId) -> Result<ProfileId, BotError>;
    fn page(&self, page: &BotPage) -> Result<(), BotError>;
    /// One bounded command's authority inspection; adapters batch this under one Store lock.
    fn inspect(
        &self,
        workspaces: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        Ok(BotAuthoritySnapshot {
            owner: self.owner()?,
            workspaces: workspaces
                .iter()
                .map(|id| (*id, self.workspace(*id)))
                .collect(),
            pages: pages
                .iter()
                .map(|page| (page.clone(), self.page(page)))
                .collect(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotCommand {
    Create {
        request_id: BotRequestId,
        bot_id: BotId,
        home_workspace: WorkspaceId,
        config: BotConfig,
        access: BotAccess,
        selected_pages: Vec<BotPage>,
    },
    Read {
        bot_id: BotId,
    },
    List {
        workspace: WorkspaceId,
    },
    Update {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
        config: BotConfig,
    },
    UpdateAccess {
        request_id: BotRequestId,
        bot_id: BotId,
        expected_revision: u64,
        expected_policy_revision: u64,
        access: BotAccess,
        selected_pages: Vec<BotPage>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum BotResponse {
    Bot { bot: Bot },
    List { bots: Vec<Bot> },
}

/// Closed, bounded receipt vocabulary. Role creation is coordinated in the UI plan;
/// its Store intent must already be durable before the first create call.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotReceiptOutcome {
    Configuration {
        version: u32,
        bot: Bot,
    },
    RoleCreation {
        version: u32,
        bot: BotId,
        store_request: uuid::Uuid,
        destination: WorkspaceId,
        request_fingerprint: String,
        verified_page: Option<BotPage>,
        verified_version: Option<Vec<u8>>,
    },
}

impl BotReceiptOutcome {
    pub fn validate(&self) -> Result<(), BotError> {
        match self {
            Self::Configuration { version: 1, .. } => Ok(()),
            Self::RoleCreation {
                version: 1,
                destination,
                verified_page,
                verified_version,
                request_fingerprint,
                ..
            } if request_fingerprint.len() == 64
                && verified_page
                    .as_ref()
                    .is_none_or(|page| page.workspace == *destination)
                && verified_page.is_some() == verified_version.is_some()
                && verified_version
                    .as_ref()
                    .is_none_or(|version| !version.is_empty() && version.len() <= 65536) =>
            {
                Ok(())
            }
            _ => Err(BotError::Invalid),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotError {
    Invalid,
    NotFound,
    Forbidden,
    Unavailable,
    Conflict,
    RevisionConflict,
    FolderLocked,
    Storage,
}
impl BotError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Invalid => "invalid_request",
            Self::NotFound => "not_found",
            Self::Forbidden => "wrong_owner",
            Self::Unavailable => "workspace_unavailable",
            Self::Conflict => "request_conflict",
            Self::RevisionConflict => "revision_conflict",
            Self::FolderLocked => "folder_locked",
            Self::Storage => "storage_error",
        }
    }
}

pub trait BotRepository {
    fn bot(&self, id: BotId) -> Result<Option<Bot>, BotError>;
    fn bots(&self, scope: UserScopeId, workspace: WorkspaceId) -> Result<Vec<Bot>, BotError>;
    fn save_bot(
        &mut self,
        bot: &Bot,
        profile: ProfileId,
        request: BotRequestId,
        fingerprint: &str,
        expected: Option<u64>,
    ) -> Result<Bot, BotError>;
    fn bot_receipt(
        &self,
        scope: UserScopeId,
        request: BotRequestId,
        fingerprint: &str,
    ) -> Result<Option<Bot>, BotError>;
}

pub fn validate_config(config: &BotConfig, workspace: WorkspaceId) -> Result<(), BotError> {
    if config.name.trim().is_empty()
        || config.name.len() > 256
        || config.role.workspace != workspace
        || !std::path::Path::new(&config.working_folder).is_absolute()
        || config.working_folder.len() > 32768
        || config.working_folder.contains('\0')
        || config.model.requested_model.trim().is_empty()
        || config.model.requested_model.len() > 512
        || !config.model.provider.as_str().starts_with("agent-")
        || config
            .mode
            .as_ref()
            .is_some_and(|mode| mode.is_empty() || mode.len() > 128)
    {
        return Err(BotError::Invalid);
    }
    Ok(())
}

pub fn normalize_pages(pages: &mut Vec<BotPage>, workspace: WorkspaceId) -> Result<(), BotError> {
    if pages.len() > 1024 || pages.iter().any(|page| page.workspace != workspace) {
        return Err(BotError::Invalid);
    }
    pages.sort();
    pages.dedup();
    Ok(())
}

#[derive(Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct StrictModel {
    provider: crate::AiProviderId,
    requested_model: String,
    default_reasoning: crate::ReasoningChoice,
}
fn strict_model<'de, D: serde::Deserializer<'de>>(d: D) -> Result<ModelSelection, D::Error> {
    let m = StrictModel::deserialize(d)?;
    Ok(ModelSelection {
        provider: m.provider,
        requested_model: m.requested_model,
        default_reasoning: m.default_reasoning,
    })
}
#[cfg(test)]
mod tests;

/// Internal per-command evidence. Never a wire payload or authority cache.
#[derive(Clone)]
pub struct BotAuthoritySnapshot {
    pub owner: BotOwner,
    pub workspaces: std::collections::BTreeMap<WorkspaceId, Result<ProfileId, BotError>>,
    pub pages: std::collections::BTreeMap<BotPage, Result<(), BotError>>,
}
impl BotAuthority for BotAuthoritySnapshot {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, id: WorkspaceId) -> Result<ProfileId, BotError> {
        self.workspaces
            .get(&id)
            .cloned()
            .unwrap_or(Err(BotError::Forbidden))
    }
    fn page(&self, page: &BotPage) -> Result<(), BotError> {
        self.pages
            .get(page)
            .cloned()
            .unwrap_or(Err(BotError::Forbidden))
    }
}
