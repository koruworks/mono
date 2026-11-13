// SPDX-License-Identifier: MPL-2.0

//! Durable Library references, never document content or Store authority.
use super::*;
use koru_delegate_core::{BlockNodeId, DocumentVersionId};
use std::collections::BTreeSet;

pub const BOT_KNOWLEDGE_LIMIT: usize = 16;
pub const BOT_MEMORY_INDEX_LIMIT: usize = 256;
pub const BOT_LIBRARY_RECEIPT_LIMIT: usize = 16 * 1024;
pub const BOT_MEMORY_SCOPE: &str = "koru-bot-memory";

fn valid_id(id: uuid::Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}

pub fn validate_library_request(
    owner: BotOwner,
    bot: BotId,
    request: BotRequestId,
    expected_revision: u64,
) -> Result<(), BotError> {
    if expected_revision == 0
        || expected_revision > i64::MAX as u64
        || ![
            owner.user_scope.as_uuid(),
            owner.device.as_uuid(),
            bot.as_uuid(),
            request.as_uuid(),
        ]
        .into_iter()
        .all(valid_id)
    {
        return Err(BotError::Invalid);
    }
    Ok(())
}

pub fn validate_knowledge(pages: &[BotPage], home: WorkspaceId) -> Result<(), BotError> {
    if pages.len() > BOT_KNOWLEDGE_LIMIT
        || pages.iter().any(|page| {
            page.workspace != home
                || !valid_id(page.document.as_uuid())
                || !valid_id(page.workspace.as_uuid())
        })
        || pages.iter().collect::<BTreeSet<_>>().len() != pages.len()
    {
        return Err(BotError::Invalid);
    }
    Ok(())
}

/// The trusted preparer has checked current Store existence/read authority for
/// every page. SQLite independently checks home, owner and the current binding.
/// No public command accepts this value in the persistence-only increment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedBotKnowledge {
    pub owner: BotOwner,
    pub bot: BotId,
    pub request: BotRequestId,
    pub expected_revision: u64,
    pub pages: Vec<BotPage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotMemoryIdentity {
    #[serde(with = "owner_serde")]
    pub owner: BotOwner,
    pub bot: BotId,
    pub workspace: WorkspaceId,
    pub creation_request: BotRequestId,
    pub candidate: DocumentId,
    pub store_request: BotRequestId,
    pub seed_format: u32,
    pub seed_title: String,
    pub seed_hash: [u8; 32],
}
impl BotMemoryIdentity {
    pub fn validate(&self) -> Result<(), BotError> {
        if self.seed_format != 1
            || self.seed_title.is_empty()
            || self.seed_title.len() > 256
            || ![
                self.owner.user_scope.as_uuid(),
                self.owner.device.as_uuid(),
                self.bot.as_uuid(),
                self.workspace.as_uuid(),
                self.creation_request.as_uuid(),
                self.candidate.as_uuid(),
                self.store_request.as_uuid(),
            ]
            .into_iter()
            .all(valid_id)
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotMemoryState {
    Prepared,
    Unconfirmed,
    Bound,
    Conflict,
    Abandoned,
}
impl BotMemoryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Unconfirmed => "unconfirmed",
            Self::Bound => "bound",
            Self::Conflict => "conflict",
            Self::Abandoned => "abandoned",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "prepared" => Self::Prepared,
            "unconfirmed" => Self::Unconfirmed,
            "bound" => Self::Bound,
            "conflict" => Self::Conflict,
            "abandoned" => Self::Abandoned,
            _ => return None,
        })
    }
    pub fn receipt_stage(self) -> &'static str {
        match self {
            Self::Prepared | Self::Unconfirmed => "prepared",
            Self::Bound => "completed",
            Self::Conflict | Self::Abandoned => "rejected",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotMemoryRefusal {
    OwnerUnavailable,
    StoreUnavailable,
    DocumentChanged,
    DocumentMoved,
    DocumentTrashed,
    LifecycleChanged,
    OriginMismatch,
}
impl BotMemoryRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OwnerUnavailable => "owner_unavailable",
            Self::StoreUnavailable => "store_unavailable",
            Self::DocumentChanged => "document_changed",
            Self::DocumentMoved => "document_moved",
            Self::DocumentTrashed => "document_trashed",
            Self::LifecycleChanged => "lifecycle_changed",
            Self::OriginMismatch => "origin_mismatch",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "owner_unavailable" => Self::OwnerUnavailable,
            "store_unavailable" => Self::StoreUnavailable,
            "document_changed" => Self::DocumentChanged,
            "document_moved" => Self::DocumentMoved,
            "document_trashed" => Self::DocumentTrashed,
            "lifecycle_changed" => Self::LifecycleChanged,
            "origin_mismatch" => Self::OriginMismatch,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotMemoryCreation {
    pub identity: BotMemoryIdentity,
    pub state: BotMemoryState,
    pub origin_edit_revision: Option<Vec<u8>>,
    pub refusal: Option<BotMemoryRefusal>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
impl BotMemoryCreation {
    pub fn validate(&self) -> Result<(), BotError> {
        self.identity.validate()?;
        if (self.state == BotMemoryState::Bound) != self.origin_edit_revision.is_some()
            || (self.state == BotMemoryState::Conflict) != self.refusal.is_some()
            || self
                .origin_edit_revision
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 65536)
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotMemoryBinding {
    #[serde(with = "owner_serde")]
    pub owner: BotOwner,
    pub bot: BotId,
    pub page: BotPage,
    pub creation_request: BotRequestId,
    pub bound_at_ms: i64,
}

/// Verified by the future Store adapter, not inferred by the repository. The
/// live candidate must still have the exact original seeded edit revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotMemoryConfirmation {
    pub identity: BotMemoryIdentity,
    pub original_edit_revision: Vec<u8>,
    pub live_edit_revision: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotMemorySeed {
    pub title: String,
    pub hash: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotMemoryTransition {
    Unknown,
    Disagreement(BotMemoryRefusal),
    ReadFailure,
    Abandon,
    Retry,
}
impl BotMemoryConfirmation {
    pub fn validate(&self) -> Result<(), BotError> {
        self.identity.validate()?;
        if self.original_edit_revision.is_empty()
            || self.original_edit_revision.len() > 65536
            || self.original_edit_revision != self.live_edit_revision
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotLibraryReceiptV1 {
    SetKnowledge {
        version: u32,
        revision: u64,
        pages: Vec<BotPage>,
    },
    EnsureMemory {
        version: u32,
        creation_request: BotRequestId,
        candidate: BotPage,
    },
}
impl BotLibraryReceiptV1 {
    pub fn validate(&self) -> Result<(), BotError> {
        match self {
            Self::SetKnowledge {
                version,
                revision,
                pages,
            } => {
                if *version != 1 || *revision == 0 || *revision > i64::MAX as u64 {
                    return Err(BotError::Invalid);
                }
                if let Some(first) = pages.first() {
                    validate_knowledge(pages, first.workspace)?;
                }
            }
            Self::EnsureMemory {
                version,
                creation_request,
                candidate,
            } if *version != 1
                || !valid_id(creation_request.as_uuid())
                || !valid_id(candidate.workspace.as_uuid())
                || !valid_id(candidate.document.as_uuid()) =>
            {
                return Err(BotError::Invalid);
            }
            _ => {}
        }
        if serde_json::to_vec(self)
            .map_err(|_| BotError::Invalid)?
            .len()
            > BOT_LIBRARY_RECEIPT_LIMIT
        {
            return Err(BotError::Invalid);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotKnowledgeCapture {
    pub page: BotPage,
    pub version: DocumentVersionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BotCapturedMemory {
    Absent {},
    Captured {
        page: BotPage,
        version: DocumentVersionId,
        creation_request: BotRequestId,
        nodes: Vec<BlockNodeId>,
        omitted_count: u64,
        invalid_count: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotLibraryCapture {
    pub version: u32,
    #[serde(with = "owner_serde")]
    pub owner: BotOwner,
    pub bot: BotId,
    pub submission: BotRequestId,
    pub config_revision: u64,
    pub knowledge: Vec<BotKnowledgeCapture>,
    pub memory: BotCapturedMemory,
    pub index_digest: [u8; 32],
}
impl BotLibraryCapture {
    pub fn validate(&self, home: WorkspaceId) -> Result<(), BotError> {
        if self.version != 1
            || self.config_revision == 0
            || self.config_revision > i64::MAX as u64
            || ![
                self.owner.user_scope.as_uuid(),
                self.owner.device.as_uuid(),
                self.bot.as_uuid(),
                self.submission.as_uuid(),
            ]
            .into_iter()
            .all(valid_id)
            || self
                .knowledge
                .iter()
                .any(|p| !valid_id(p.version.as_uuid()))
        {
            return Err(BotError::Invalid);
        }
        validate_knowledge(
            &self
                .knowledge
                .iter()
                .map(|p| p.page.clone())
                .collect::<Vec<_>>(),
            home,
        )?;
        if let BotCapturedMemory::Captured {
            page,
            version,
            creation_request,
            nodes,
            omitted_count,
            invalid_count,
        } = &self.memory
        {
            if page.workspace != home
                || nodes.len() > BOT_MEMORY_INDEX_LIMIT
                || ![
                    page.workspace.as_uuid(),
                    page.document.as_uuid(),
                    version.as_uuid(),
                    creation_request.as_uuid(),
                ]
                .into_iter()
                .all(valid_id)
                || nodes.iter().any(|n| !valid_id(n.as_uuid()))
                || nodes.iter().collect::<BTreeSet<_>>().len() != nodes.len()
                || *omitted_count > i64::MAX as u64
                || *invalid_count > i64::MAX as u64
                || self.knowledge.iter().any(|k| k.page == *page)
            {
                return Err(BotError::Invalid);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotLibrary {
    pub knowledge: Vec<BotPage>,
    pub creation: Option<BotMemoryCreation>,
    pub binding: Option<BotMemoryBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotMemoryPendingCursor {
    pub owner: BotOwner,
    pub updated_at_ms: i64,
    pub request: BotRequestId,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotMemoryPendingPage {
    pub creations: Vec<BotMemoryCreation>,
    pub next: Option<BotMemoryPendingCursor>,
}

mod owner_serde {
    use super::*;
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Owner {
        user_scope: UserScopeId,
        device: DeviceId,
    }
    pub fn serialize<S: serde::Serializer>(
        value: &BotOwner,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        Owner {
            user_scope: value.user_scope,
            device: value.device,
        }
        .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BotOwner, D::Error> {
        let value = Owner::deserialize(deserializer)?;
        Ok(BotOwner {
            user_scope: value.user_scope,
            device: value.device,
        })
    }
}

#[cfg(test)]
mod tests;
