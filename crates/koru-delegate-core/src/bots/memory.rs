// SPDX-License-Identifier: MPL-2.0
//! Private Memory operation identities. None of these values grants authority.
use super::{BotOwner, BotProvenance, NativeResultKey, v4};
use crate::{BotId, BotRequestId, DocumentId, DocumentVersionId, WorkspaceId};
pub use koru_store_core::native_effect::same_native_edit_revision;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MEMORY_EDIT_ALLOCATION_LIMIT: usize = 256;
pub const MEMORY_EDIT_UNLINKED_LIMIT: usize = 16;
pub const MEMORY_ENSURE_TITLE: &str = "Memory";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryEditAction {
    Remember,
    Correct,
    Forget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEditSubjectV1 {
    pub version: u32,
    pub owner: BotOwner,
    pub bot: BotId,
    pub submission: BotRequestId,
    pub captured_config_revision: u64,
    pub action: MemoryEditAction,
}
impl MemoryEditSubjectV1 {
    pub fn valid(&self) -> bool {
        self.version == 1
            && !self.owner.user_scope.as_uuid().is_nil()
            && !self.owner.device.as_uuid().is_nil()
            && v4(self.bot.as_uuid())
            && v4(self.submission.as_uuid())
            && revision(self.captured_config_revision)
    }

    pub fn matches(&self, provenance: &BotProvenance) -> bool {
        self.valid()
            && self.owner == provenance.owner
            && self.bot == provenance.bot
            && self.submission == provenance.submission
            && self.captured_config_revision == provenance.config_revision
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEditAuthorityV1 {
    pub version: u32,
    pub owner: BotOwner,
    pub bot: BotId,
    pub submission: BotRequestId,
    pub captured_config_revision: u64,
    pub creation_request: BotRequestId,
    pub workspace: WorkspaceId,
    pub memory_document: DocumentId,
    pub action: MemoryEditAction,
}
impl MemoryEditAuthorityV1 {
    pub fn subject(&self) -> MemoryEditSubjectV1 {
        MemoryEditSubjectV1 {
            version: self.version,
            owner: self.owner,
            bot: self.bot,
            submission: self.submission,
            captured_config_revision: self.captured_config_revision,
            action: self.action,
        }
    }

    pub fn valid(&self) -> bool {
        self.subject().valid()
            && v4(self.creation_request.as_uuid())
            && !self.workspace.as_uuid().is_nil()
            && !self.memory_document.as_uuid().is_nil()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryBaseSelector {
    History { version: DocumentVersionId },
    OriginalSeed {},
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllocateMemoryEditRequest {
    pub key: NativeResultKey,
    pub caller_fingerprint: [u8; 32],
    pub subject: MemoryEditSubjectV1,
    pub base: MemoryBaseSelector,
    pub current_config_revision: u64,
    pub current_policy_revision: u64,
    #[serde(with = "optional_node_uuid")]
    pub existing_fact_id: Option<Uuid>,
}
impl AllocateMemoryEditRequest {
    pub fn valid(&self) -> bool {
        self.subject.valid()
            && v4(self.key.operation_id)
            && revision(self.current_config_revision)
            && revision(self.current_policy_revision)
            && self.current_config_revision >= self.subject.captured_config_revision
            && match (&self.base, self.subject.action, self.existing_fact_id) {
                (MemoryBaseSelector::OriginalSeed {}, MemoryEditAction::Remember, None) => true,
                (MemoryBaseSelector::History { version }, action, fact) => {
                    !version.as_uuid().is_nil()
                        && match action {
                            MemoryEditAction::Remember => fact.is_none(),
                            MemoryEditAction::Correct | MemoryEditAction::Forget => fact.is_some(),
                        }
                }
                _ => false,
            }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEnsureIdentity {
    pub request_id: BotRequestId,
    pub expected_revision: u64,
    pub title: String,
}
impl std::fmt::Debug for MemoryEnsureIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryEnsureIdentity")
            .field("request_id", &self.request_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryBaseResolution {
    Unresolved {},
    Resolved {
        authority: MemoryEditAuthorityV1,
        version: DocumentVersionId,
        expected_revision: Vec<u8>,
    },
}
impl std::fmt::Debug for MemoryBaseResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unresolved {} => f.write_str("MemoryBaseResolution::Unresolved"),
            Self::Resolved { .. } => f.write_str("MemoryBaseResolution::Resolved([REDACTED])"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEditAllocation {
    pub key: NativeResultKey,
    pub caller_fingerprint: [u8; 32],
    pub subject: MemoryEditSubjectV1,
    pub base: MemoryBaseSelector,
    pub ensure: Option<MemoryEnsureIdentity>,
    pub resolution: MemoryBaseResolution,
    pub source_run_document: DocumentId,
    #[serde(with = "node_uuid")]
    pub fact_id: Uuid,
    #[serde(with = "optional_node_uuid")]
    pub detail_id: Option<Uuid>,
    #[serde(with = "optional_node_uuid")]
    pub source_id: Option<Uuid>,
    pub created_at_ms: i64,
}
impl MemoryEditAllocation {
    pub fn valid(&self) -> bool {
        if !self.subject.valid()
            || !v4(self.key.operation_id)
            || self.source_run_document.as_uuid().is_nil()
        {
            return false;
        }
        match (&self.base, &self.ensure) {
            (MemoryBaseSelector::History { version }, None) if !version.as_uuid().is_nil() => {}
            (MemoryBaseSelector::OriginalSeed {}, Some(ensure))
                if self.subject.action == MemoryEditAction::Remember
                    && v4(ensure.request_id.as_uuid())
                    && ensure.request_id.as_uuid() != self.key.operation_id
                    && revision(ensure.expected_revision)
                    && ensure.expected_revision >= self.subject.captured_config_revision
                    && ensure.title == MEMORY_ENSURE_TITLE => {}
            _ => return false,
        }
        match (self.subject.action, self.detail_id, self.source_id) {
            (MemoryEditAction::Forget, None, None) => {}
            (
                MemoryEditAction::Remember | MemoryEditAction::Correct,
                Some(detail),
                Some(source),
            ) if v4(detail)
                && v4(source)
                && detail != source
                && detail != self.fact_id
                && source != self.fact_id => {}
            _ => return false,
        }
        if self.subject.action == MemoryEditAction::Remember && !v4(self.fact_id) {
            return false;
        }
        match &self.resolution {
            MemoryBaseResolution::Unresolved {} => true,
            MemoryBaseResolution::Resolved {
                authority,
                version,
                expected_revision,
            } => {
                authority.valid()
                    && authority.subject() == self.subject
                    && !version.as_uuid().is_nil()
                    && match self.base {
                        MemoryBaseSelector::History { version: requested } => requested == *version,
                        MemoryBaseSelector::OriginalSeed {} => true,
                    }
                    && same_native_edit_revision(expected_revision, expected_revision)
                        .unwrap_or(false)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeEffectPurpose {
    Result {},
    Memory { authority: MemoryEditAuthorityV1 },
}

fn revision(value: u64) -> bool {
    value > 0 && value <= i64::MAX as u64
}

// Core NodeId accepts the full UUID domain. Only spelling is narrowed here;
// newly generated identities are checked separately, never existing fact IDs.
fn parse_node<E: serde::de::Error>(raw: String) -> Result<Uuid, E> {
    let value = Uuid::parse_str(&raw).map_err(E::custom)?;
    if value.to_string() != raw {
        return Err(E::custom("Memory node UUID must be canonical"));
    }
    Ok(value)
}
mod node_uuid {
    use super::*;
    pub fn serialize<S: serde::Serializer>(value: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
        value.serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Uuid, D::Error> {
        parse_node(String::deserialize(deserializer)?)
    }
}
mod optional_node_uuid {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &Option<Uuid>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Uuid>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(parse_node)
            .transpose()
    }
}

#[cfg(test)]
mod tests;
